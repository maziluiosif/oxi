//! SSH port-forwarding tunnels for remote compute targets.
//!
//! A [`TunnelManager`] keeps at most one tunnel alive per provider: an SSH
//! connection to the configured host, plus a local TCP listener that forwards each
//! accepted connection to `127.0.0.1:<remote_runtime_port>` on the far side via
//! `direct-tcpip`. Callers ask for the *local* port to use as the effective base URL;
//! the manager connects lazily on first use and reuses the tunnel afterwards.
//!
//! Host key verification uses trust-on-first-use (TOFU): the fingerprint observed on the
//! first successful connection is pinned into the provider's `SshConfig`, and any later
//! connection presenting a different key is refused until the user explicitly accepts the
//! new key. This is a convenience tunnel to a host the user explicitly typed in, not a
//! general-purpose SSH client, so there is no `known_hosts`/CA machinery — just the pin.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use russh::client::{self, AuthResult};
use russh::keys::{HashAlg, PublicKey};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

use crate::settings::SshConfig;

/// Outcome of a successful tunnel connect.
#[derive(Debug, Clone)]
pub struct TunnelSuccess {
    /// Local `127.0.0.1` port forwarding to the remote runtime.
    pub local_port: u16,
    /// SHA-256 fingerprint ("SHA256:<base64>") of the host key the server presented.
    pub host_key_fingerprint: String,
}

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

/// russh handler implementing TOFU: records the fingerprint the server presents (into
/// `observed`) and accepts the connection when there is no pin yet or the pin matches;
/// returning `Ok(false)` on a mismatch aborts the handshake.
struct HostKeyVerifier {
    pinned: Option<String>,
    observed: Arc<Mutex<Option<String>>>,
}

impl client::Handler for HostKeyVerifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        let fp = server_public_key.fingerprint(HashAlg::Sha256).to_string();
        if let Ok(mut slot) = self.observed.lock() {
            *slot = Some(fp.clone());
        }
        Ok(match &self.pinned {
            None => true,
            Some(pinned) => *pinned == fp,
        })
    }
}

struct TunnelRequest {
    key: String,
    config: SshConfig,
    password: String,
    reply: oneshot::Sender<Result<TunnelSuccess, TunnelError>>,
}

struct ActiveTunnel {
    local_port: u16,
    host_key_fingerprint: String,
    // Keeping these alive keeps the listener + SSH session alive; dropping either tears
    // the tunnel down.
    accept_task: tokio::task::JoinHandle<()>,
    _session: Arc<client::Handle<HostKeyVerifier>>,
}

/// Cheap to clone; every clone talks to the same background tunnel-management task.
#[derive(Clone)]
pub struct TunnelManager {
    tx: mpsc::UnboundedSender<TunnelRequest>,
    /// Fingerprints observed on successful connects, keyed by provider slug. Drained by the
    /// app (see [`TunnelManager::take_observed_host_keys`]) to pin first-use keys into
    /// settings. Shared with the manager task.
    observed: Arc<Mutex<HashMap<String, String>>>,
}

impl TunnelManager {
    /// Spawn the manager's dedicated background thread + Tokio runtime. Call once at app
    /// startup; the returned handle is safe to share and call from any thread.
    pub fn spawn() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<TunnelRequest>();
        let observed: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let observed_task = observed.clone();
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
                    let observed = observed_task.clone();
                    tokio::spawn(async move {
                        let result =
                            ensure_one(&tunnels, &req.key, &req.config, &req.password).await;
                        if let Ok(ok) = &result
                            && let Ok(mut map) = observed.lock()
                        {
                            map.insert(req.key.clone(), ok.host_key_fingerprint.clone());
                        }
                        let _ = req.reply.send(result);
                    });
                }
            });
        });
        Self { tx, observed }
    }

    /// Ensure a tunnel is up for `key` (a provider slug), (re)connecting if needed, and return the
    /// local `127.0.0.1` port that proxies to `127.0.0.1:<remote_runtime_port>` on the
    /// remote host. Cheap to call repeatedly: an already-healthy tunnel is reused.
    pub async fn ensure_tunnel(
        &self,
        key: &str,
        config: &SshConfig,
        password: &str,
    ) -> Result<TunnelSuccess, TunnelError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(TunnelRequest {
                key: key.to_string(),
                config: config.clone(),
                password: password.to_string(),
                reply: reply_tx,
            })
            .map_err(|_| {
                TunnelError::Other("SSH tunnel manager is not running".to_string())
            })?;
        reply_rx.await.map_err(|_| {
            TunnelError::Other("SSH tunnel manager dropped the request".to_string())
        })?
    }

    /// Drain the fingerprints observed on successful connects since the last call
    /// (provider slug → "SHA256:..."). The app uses these to pin first-use keys into
    /// settings; already-pinned providers ignore their entry.
    pub fn take_observed_host_keys(&self) -> Vec<(String, String)> {
        match self.observed.lock() {
            Ok(mut map) => map.drain().collect(),
            Err(_) => Vec::new(),
        }
    }
}

async fn ensure_one(
    tunnels: &Arc<AsyncMutex<HashMap<String, ActiveTunnel>>>,
    key: &str,
    config: &SshConfig,
    password: &str,
) -> Result<TunnelSuccess, TunnelError> {
    {
        let map = tunnels.lock().await;
        if let Some(t) = map.get(key)
            && !t.accept_task.is_finished()
        {
            return Ok(TunnelSuccess {
                local_port: t.local_port,
                host_key_fingerprint: t.host_key_fingerprint.clone(),
            });
        }
    }
    let tunnel = open_tunnel(config, password).await?;
    let success = TunnelSuccess {
        local_port: tunnel.local_port,
        host_key_fingerprint: tunnel.host_key_fingerprint.clone(),
    };
    tunnels.lock().await.insert(key.to_string(), tunnel);
    Ok(success)
}

async fn open_tunnel(config: &SshConfig, password: &str) -> Result<ActiveTunnel, TunnelError> {
    if config.host.trim().is_empty() {
        return Err(TunnelError::Other("SSH host is empty".to_string()));
    }
    if config.user.trim().is_empty() {
        return Err(TunnelError::Other("SSH user is empty".to_string()));
    }
    let addr = format!("{}:{}", config.host, config.port);
    let ssh_config = Arc::new(client::Config::default());
    let observed: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let verifier = HostKeyVerifier {
        pinned: config.pinned_host_key.clone(),
        observed: observed.clone(),
    };
    let mut session = match client::connect(ssh_config, addr.as_str(), verifier).await {
        Ok(s) => s,
        Err(e) => {
            // A mismatch surfaces as a generic handshake error (the handler returned
            // Ok(false)); detect it via the fingerprint we recorded rather than matching a
            // russh error variant.
            let observed_fp = observed.lock().ok().and_then(|s| s.clone());
            if let (Some(pinned), Some(observed_fp)) = (&config.pinned_host_key, observed_fp)
                && *pinned != observed_fp
            {
                return Err(TunnelError::HostKeyMismatch {
                    pinned: pinned.clone(),
                    observed: observed_fp,
                });
            }
            return Err(TunnelError::Other(format!("SSH connect to {addr} failed: {e}")));
        }
    };
    let host_key_fingerprint = observed.lock().ok().and_then(|s| s.clone()).unwrap_or_default();
    let auth = session
        .authenticate_password(&config.user, password)
        .await
        .map_err(|e| TunnelError::Other(format!("SSH auth failed: {e}")))?;
    if !matches!(auth, AuthResult::Success) {
        return Err(TunnelError::Other(
            "SSH authentication rejected (check user/password)".to_string(),
        ));
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
        host_key_fingerprint,
        accept_task,
        _session: session,
    })
}

async fn forward_connection(
    mut local: TcpStream,
    session: &client::Handle<HostKeyVerifier>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use russh::client::Handler;
    use russh::keys::ssh_key::PublicKey;

    // A fixed, well-formed ed25519 public key in OpenSSH format (from ssh-key's own test data).
    const SAMPLE_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPk+8niRPcg1r7p4n2K06qGdKQ7OM+yZJwHOAKFB68oM alice@example.com";

    fn sample_key() -> PublicKey {
        PublicKey::from_openssh(SAMPLE_KEY).expect("valid test key")
    }

    async fn check(pinned: Option<String>) -> (bool, Option<String>) {
        let observed = Arc::new(Mutex::new(None));
        let mut v = HostKeyVerifier {
            pinned,
            observed: observed.clone(),
        };
        let accepted = v.check_server_key(&sample_key()).await.unwrap();
        let recorded = observed.lock().unwrap().clone();
        (accepted, recorded)
    }

    #[test]
    fn verifier_accepts_and_records_when_unpinned() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (accepted, recorded) = rt.block_on(check(None));
        assert!(accepted);
        assert!(recorded.is_some());
    }

    #[test]
    fn verifier_accepts_matching_pin() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let fp = sample_key().fingerprint(HashAlg::Sha256).to_string();
        let (accepted, recorded) = rt.block_on(check(Some(fp.clone())));
        assert!(accepted);
        assert_eq!(recorded, Some(fp));
    }

    #[test]
    fn verifier_rejects_mismatched_pin() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (accepted, recorded) = rt.block_on(check(Some("SHA256:bogus".to_string())));
        assert!(!accepted);
        // The observed fingerprint is still recorded so the caller can surface it.
        assert!(recorded.is_some());
        assert_ne!(recorded.as_deref(), Some("SHA256:bogus"));
    }
}
