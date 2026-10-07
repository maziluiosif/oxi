//! SSH port-forwarding tunnels for remote compute targets.
//!
//! A [`TunnelManager`] keeps at most one tunnel alive per provider: an SSH
//! connection to the configured host, plus a local TCP listener that forwards each
//! accepted connection to `127.0.0.1:<remote_runtime_port>` on the far side via
//! `direct-tcpip`. Callers ask for the *local* port to use as the effective base URL;
//! the manager connects lazily on first use and reuses the tunnel afterwards.
//!
//! Host key verification uses trust-on-first-use (TOFU), see [`super::connect`]. This is a
//! convenience tunnel to a host the user explicitly typed in, not a general-purpose SSH
//! client, so there is no `known_hosts`/CA machinery — just the pin.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

use crate::settings::SshConfig;

use super::connect::{Session, connect};

/// Why a tunnel connect failed. [`TunnelError::HostKeyMismatch`] is distinguished so the UI
/// can offer to accept the new key; everything else is [`TunnelError::Other`].
#[derive(Debug, Clone)]
pub enum TunnelError {
    HostKeyMismatch { pinned: String, observed: String },
    Other(String),
}

impl std::fmt::Display for TunnelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TunnelError::HostKeyMismatch { pinned, observed } => write!(
                f,
                "SSH host key mismatch: pinned {pinned}, server presented {observed}. \
                 If the host was reinstalled, accept the new key in Settings."
            ),
            TunnelError::Other(e) => write!(f, "{e}"),
        }
    }
}

struct TunnelRequest {
    key: String,
    config: SshConfig,
    password: String,
    reply: oneshot::Sender<Result<u16, TunnelError>>,
}

struct ActiveTunnel {
    local_port: u16,
    // Keeping these alive keeps the listener + SSH session alive; dropping either tears
    // the tunnel down.
    accept_task: tokio::task::JoinHandle<()>,
    _session: Arc<Session>,
}

/// Cheap to clone; every clone talks to the same background tunnel-management task.
#[derive(Clone)]
pub struct TunnelManager {
    tx: mpsc::UnboundedSender<TunnelRequest>,
}

impl TunnelManager {
    /// Spawn the manager's dedicated background thread on the shared runtime. Call once at app
    /// startup; the returned handle is safe to share and call from any thread.
    pub fn spawn() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<TunnelRequest>();
        std::thread::spawn(move || {
            let Ok(rt) = crate::runtime::runtime() else {
                return;
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
    ) -> Result<u16, TunnelError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(TunnelRequest {
                key: key.to_string(),
                config: config.clone(),
                password: password.to_string(),
                reply: reply_tx,
            })
            .map_err(|_| TunnelError::Other("SSH tunnel manager is not running".to_string()))?;
        reply_rx
            .await
            .map_err(|_| TunnelError::Other("SSH tunnel manager dropped the request".to_string()))?
    }
}

async fn ensure_one(
    tunnels: &Arc<AsyncMutex<HashMap<String, ActiveTunnel>>>,
    key: &str,
    config: &SshConfig,
    password: &str,
) -> Result<u16, TunnelError> {
    {
        let map = tunnels.lock().await;
        if let Some(t) = map.get(key)
            && !t.accept_task.is_finished()
        {
            return Ok(t.local_port);
        }
    }
    let tunnel = open_tunnel(key, config, password).await?;
    let local_port = tunnel.local_port;
    tunnels.lock().await.insert(key.to_string(), tunnel);
    Ok(local_port)
}

async fn open_tunnel(
    key: &str,
    config: &SshConfig,
    password: &str,
) -> Result<ActiveTunnel, TunnelError> {
    let session = connect(key, config, password).await?;
    let remote_port = config.remote_runtime_port;

    // SSH auth succeeding only proves the host is reachable, not that the model runtime is
    // actually listening on `remote_port` over there. Probe it now so "Test connection" (and
    // the first real request) fail with a clear message instead of a cryptic HTTP error
    // later, when `forward_connection` would otherwise silently drop the local connection.
    let probe = session
        .channel_open_direct_tcpip("127.0.0.1", remote_port as u32, "127.0.0.1", 0)
        .await
        .map_err(|e| {
            TunnelError::Other(format!(
                "SSH connected, but nothing is reachable on 127.0.0.1:{remote_port} on \
                 {} (is the runtime running and listening on that port?): {e}",
                config.host
            ))
        })?;
    let _ = probe.close().await;

    let session = Arc::new(session);

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| TunnelError::Other(format!("failed to bind local tunnel port: {e}")))?;
    let local_port = listener
        .local_addr()
        .map_err(|e| TunnelError::Other(e.to_string()))?
        .port();
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
    session: &Session,
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
