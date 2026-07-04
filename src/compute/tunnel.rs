//! SSH port-forwarding tunnels for remote compute targets.
//!
//! A [`TunnelManager`] keeps at most one tunnel alive per provider: an SSH
//! connection to the configured host, plus a local TCP listener that forwards each
//! accepted connection to `127.0.0.1:<remote_runtime_port>` on the far side via
//! `direct-tcpip`. Callers ask for the *local* port to use as the effective base URL;
//! the manager connects lazily on first use and reuses the tunnel afterwards.
//!
//! Host key verification is intentionally permissive (trust-on-every-connect): this is a
//! convenience tunnel to a host the user explicitly typed in, not a general-purpose SSH
//! client. A future version could pin/verify keys; v1 favors "it just works" for a single
//! trusted LAN host.

use std::collections::HashMap;
use std::sync::Arc;

use russh::client::{self, AuthResult};
use russh::keys::PublicKey;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};

use crate::settings::SshConfig;

struct AcceptAnyServerKey;

impl client::Handler for AcceptAnyServerKey {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

struct TunnelRequest {
    key: String,
    config: SshConfig,
    password: String,
    reply: oneshot::Sender<Result<u16, String>>,
}

struct ActiveTunnel {
    local_port: u16,
    // Keeping these alive keeps the listener + SSH session alive; dropping either tears
    // the tunnel down.
    accept_task: tokio::task::JoinHandle<()>,
    _session: Arc<client::Handle<AcceptAnyServerKey>>,
}

/// Cheap to clone; every clone talks to the same background tunnel-management task.
#[derive(Clone)]
pub struct TunnelManager {
    tx: mpsc::UnboundedSender<TunnelRequest>,
}

impl TunnelManager {
    /// Spawn the manager's dedicated background thread + Tokio runtime. Call once at app
    /// startup; the returned handle is safe to share and call from any thread.
    pub fn spawn() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<TunnelRequest>();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(_) => return,
            };
            rt.block_on(async move {
                let tunnels: Arc<AsyncMutex<HashMap<String, ActiveTunnel>>> =
                    Arc::new(AsyncMutex::new(HashMap::new()));
                while let Some(req) = rx.recv().await {
                    let tunnels = tunnels.clone();
                    tokio::spawn(async move {
                        let result =
                            ensure_one(&tunnels, &req.key, &req.config, &req.password).await;
                        let _ = req.reply.send(result);
                    });
                }
            });
        });
        Self { tx }
    }

    /// Ensure a tunnel is up for `key` (a provider slug), (re)connecting if needed, and return the
    /// local `127.0.0.1` port that proxies to `127.0.0.1:<remote_runtime_port>` on the
    /// remote host. Cheap to call repeatedly: an already-healthy tunnel is reused.
    pub async fn ensure_tunnel(
        &self,
        key: &str,
        config: &SshConfig,
        password: &str,
    ) -> Result<u16, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(TunnelRequest {
                key: key.to_string(),
                config: config.clone(),
                password: password.to_string(),
                reply: reply_tx,
            })
            .map_err(|_| "SSH tunnel manager is not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "SSH tunnel manager dropped the request".to_string())?
    }
}

async fn ensure_one(
    tunnels: &Arc<AsyncMutex<HashMap<String, ActiveTunnel>>>,
    key: &str,
    config: &SshConfig,
    password: &str,
) -> Result<u16, String> {
    {
        let map = tunnels.lock().await;
        if let Some(t) = map.get(key) {
            if !t.accept_task.is_finished() {
                return Ok(t.local_port);
            }
        }
    }
    let tunnel = open_tunnel(config, password).await?;
    let port = tunnel.local_port;
    tunnels.lock().await.insert(key.to_string(), tunnel);
    Ok(port)
}

async fn open_tunnel(config: &SshConfig, password: &str) -> Result<ActiveTunnel, String> {
    if config.host.trim().is_empty() {
        return Err("SSH host is empty".to_string());
    }
    if config.user.trim().is_empty() {
        return Err("SSH user is empty".to_string());
    }
    let addr = format!("{}:{}", config.host, config.port);
    let ssh_config = Arc::new(client::Config::default());
    let mut session = client::connect(ssh_config, addr.as_str(), AcceptAnyServerKey)
        .await
        .map_err(|e| format!("SSH connect to {addr} failed: {e}"))?;
    let auth = session
        .authenticate_password(&config.user, password)
        .await
        .map_err(|e| format!("SSH auth failed: {e}"))?;
    if !matches!(auth, AuthResult::Success) {
        return Err("SSH authentication rejected (check user/password)".to_string());
    }
    let remote_port = config.remote_runtime_port;

    // SSH auth succeeding only proves the host is reachable, not that the model runtime is
    // actually listening on `remote_port` over there. Probe it now so "Test connection" (and
    // the first real request) fail with a clear message instead of a cryptic HTTP error
    // later, when `forward_connection` would otherwise silently drop the local connection.
    let probe = session
        .channel_open_direct_tcpip("127.0.0.1", remote_port as u32, "127.0.0.1", 0)
        .await
        .map_err(|e| {
            format!(
                "SSH connected, but nothing is reachable on 127.0.0.1:{remote_port} on \
                 {} (is the runtime running and listening on that port?): {e}",
                config.host
            )
        })?;
    let _ = probe.close().await;

    let session = Arc::new(session);

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("failed to bind local tunnel port: {e}"))?;
    let local_port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let session_for_task = session.clone();
    let accept_task = tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => break,
            };
            let session = session_for_task.clone();
            tokio::spawn(async move {
                let _ = forward_connection(stream, &session, remote_port).await;
            });
        }
    });

    Ok(ActiveTunnel {
        local_port,
        accept_task,
        _session: session,
    })
}

async fn forward_connection(
    mut local: TcpStream,
    session: &client::Handle<AcceptAnyServerKey>,
    remote_port: u16,
) -> Result<(), String> {
    let channel = session
        .channel_open_direct_tcpip("127.0.0.1", remote_port as u32, "127.0.0.1", 0)
        .await
        .map_err(|e| e.to_string())?;
    let mut remote = channel.into_stream();
    tokio::io::copy_bidirectional(&mut local, &mut remote)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
