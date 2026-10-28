//! Authenticated SSH connections shared by the port-forwarding tunnels and one-shot remote
//! commands, including trust-on-first-use host key pinning.
//!
//! Every successful connect records the host key it saw under the caller's key (a provider
//! slug). The app drains those with [`take_observed_host_keys`] and pins first-use keys into
//! the provider's `SshConfig`; a later connection presenting a different key is refused with
//! [`TunnelError::HostKeyMismatch`] until the user accepts the new key in Settings.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use russh::client::{self, AuthResult};
use russh::keys::{HashAlg, PublicKeyOrCertificate};

use crate::settings::SshConfig;

use super::TunnelError;

/// russh handler implementing TOFU: records the fingerprint the server presents (into
/// `observed`) and accepts the connection when there is no pin yet or the pin matches;
/// returning `Ok(false)` on a mismatch aborts the handshake.
pub(super) struct HostKeyVerifier {
    pinned: Option<String>,
    observed: Arc<Mutex<Option<String>>>,
}

impl client::Handler for HostKeyVerifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // A host certificate is pinned by the host key it carries; its CA is not trusted.
        let fp = server_public_key
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string();
        if let Ok(mut slot) = self.observed.lock() {
            *slot = Some(fp.clone());
        }
        Ok(match &self.pinned {
            None => true,
            Some(pinned) => *pinned == fp,
        })
    }
}

pub(super) type Session = client::Handle<HostKeyVerifier>;

/// Fingerprints seen on successful connects since the app last drained them, keyed by
/// provider slug.
static OBSERVED_HOST_KEYS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn record_observed_host_key(key: &str, fingerprint: &str) {
    if fingerprint.is_empty() {
        return;
    }
    let mut observed = OBSERVED_HOST_KEYS.lock().unwrap_or_else(|e| e.into_inner());
    observed
        .get_or_insert_with(HashMap::new)
        .insert(key.to_string(), fingerprint.to_string());
}

/// Drain the fingerprints observed on successful connects since the last call
/// (provider slug → "SHA256:..."). The app uses these to pin first-use keys into settings;
/// already-pinned providers ignore their entry.
pub fn take_observed_host_keys() -> Vec<(String, String)> {
    let mut observed = OBSERVED_HOST_KEYS.lock().unwrap_or_else(|e| e.into_inner());
    observed
        .take()
        .map(|map| map.into_iter().collect())
        .unwrap_or_default()
}

/// Connect to `config`'s host, verify its key against the pin, and authenticate (see
/// [`authenticate`]). On success the observed fingerprint is recorded under `key` for pinning.
pub(super) async fn connect(
    key: &str,
    config: &SshConfig,
    password: &str,
) -> Result<Session, TunnelError> {
    if config.host.trim().is_empty() {
        return Err(TunnelError::Other("SSH host is empty".to_string()));
    }
    let user = ssh_user(config)?;
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
            return Err(TunnelError::Other(format!(
                "SSH connect to {addr} failed: {e}"
            )));
        }
    };
    authenticate(&mut session, &user, password).await?;
    // Only pin a key once the host has also accepted our credentials, so a typo'd host that
    // happens to answer doesn't get its key remembered.
    if let Some(fp) = observed.lock().ok().and_then(|s| s.clone()) {
        record_observed_host_key(key, &fp);
    }
    Ok(session)
}

/// The configured SSH user, or the local login name when the field is left empty (what
/// `ssh host` does).
fn ssh_user(config: &SshConfig) -> Result<String, TunnelError> {
    let user = config.user.trim();
    if !user.is_empty() {
        return Ok(user.to_string());
    }
    ["USER", "USERNAME", "LOGNAME"]
        .iter()
        .find_map(|var| std::env::var(var).ok().filter(|v| !v.trim().is_empty()))
        .ok_or_else(|| TunnelError::Other("SSH user is empty".to_string()))
}

/// Default private keys `ssh` itself tries, in its order.
fn default_identity_files() -> Vec<std::path::PathBuf> {
    let Some(ssh_dir) = dirs::home_dir().map(|h| h.join(".ssh")) else {
        return Vec::new();
    };
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|name| ssh_dir.join(name))
        .filter(|path| path.is_file())
        .collect()
}

/// Try public keys first, like `ssh` does: identities held by the SSH agent, then the
/// default `~/.ssh/id_*` files (the password doubles as the passphrase of an encrypted
/// one), then the password itself when one is set.
async fn authenticate(
    session: &mut Session,
    user: &str,
    password: &str,
) -> Result<(), TunnelError> {
    let rsa_hash = session
        .best_supported_rsa_hash()
        .await
        .ok()
        .flatten()
        .flatten();
    let mut offered_key = false;

    #[cfg(unix)]
    if let Ok(mut agent) = russh::keys::agent::client::AgentClient::connect_env().await {
        let identities = agent.request_identities().await.unwrap_or_default();
        for identity in identities {
            let russh::keys::agent::AgentIdentity::PublicKey { key, .. } = identity else {
                continue;
            };
            offered_key = true;
            if let Ok(AuthResult::Success) = session
                .authenticate_publickey_with(user, key, rsa_hash, &mut agent)
                .await
            {
                return Ok(());
            }
        }
    }

    let mut locked_key = false;
    for path in default_identity_files() {
        let key = match russh::keys::load_secret_key(&path, None) {
            Ok(key) => key,
            Err(_) if !password.is_empty() => {
                match russh::keys::load_secret_key(&path, Some(password)) {
                    Ok(key) => key,
                    Err(_) => continue,
                }
            }
            Err(_) => {
                locked_key = true;
                continue;
            }
        };
        offered_key = true;
        let key = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), rsa_hash);
        if let Ok(AuthResult::Success) = session.authenticate_publickey(user, key).await {
            return Ok(());
        }
    }

    if !password.is_empty() {
        let auth = session
            .authenticate_password(user, password)
            .await
            .map_err(|e| TunnelError::Other(format!("SSH auth failed: {e}")))?;
        if matches!(auth, AuthResult::Success) {
            return Ok(());
        }
        return Err(TunnelError::Other(
            "SSH authentication rejected (check user, key, or password)".to_string(),
        ));
    }
    Err(TunnelError::Other(if locked_key {
        "SSH key is passphrase-protected: add it to ssh-agent (ssh-add) or enter the passphrase as the password".to_string()
    } else if offered_key {
        format!("SSH key rejected for {user} (is your public key in the host's authorized_keys?)")
    } else {
        "No SSH key found (agent or ~/.ssh/id_*) and no password set".to_string()
    }))
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
        let accepted = v
            .check_server_key(&PublicKeyOrCertificate::from(sample_key()))
            .await
            .unwrap();
        let recorded = observed.lock().unwrap_or_else(|e| e.into_inner()).clone();
        (accepted, recorded)
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn verifier_accepts_and_records_when_unpinned() {
        let (accepted, recorded) = block_on(check(None));
        assert!(accepted);
        assert!(recorded.is_some());
    }

    #[test]
    fn verifier_accepts_matching_pin() {
        let fp = sample_key().fingerprint(HashAlg::Sha256).to_string();
        let (accepted, recorded) = block_on(check(Some(fp.clone())));
        assert!(accepted);
        assert_eq!(recorded, Some(fp));
    }

    #[test]
    fn verifier_rejects_mismatched_pin() {
        let (accepted, recorded) = block_on(check(Some("SHA256:bogus".to_string())));
        assert!(!accepted);
        // The observed fingerprint is still recorded so the caller can surface it.
        assert!(recorded.is_some());
        assert_ne!(recorded.as_deref(), Some("SHA256:bogus"));
    }

    #[test]
    fn observed_keys_are_drained_once() {
        record_observed_host_key("test-drain-provider", "SHA256:abc");
        record_observed_host_key("test-drain-provider", "");
        let first = take_observed_host_keys();
        assert!(first.contains(&("test-drain-provider".to_string(), "SHA256:abc".to_string())));
        assert!(
            !take_observed_host_keys()
                .iter()
                .any(|(key, _)| key == "test-drain-provider")
        );
    }

    #[test]
    fn connect_rejects_missing_host_before_dialing() {
        let missing_host = SshConfig {
            user: "me".into(),
            ..SshConfig::default()
        };
        let err = block_on(connect("p", &missing_host, "pw")).err().unwrap();
        assert!(err.to_string().contains("host is empty"));
    }

    #[test]
    fn empty_user_falls_back_to_login_name() {
        let named = SshConfig {
            user: " ioan ".into(),
            ..SshConfig::default()
        };
        assert_eq!(ssh_user(&named).unwrap(), "ioan");
        if let Ok(login) = std::env::var("USER") {
            assert_eq!(ssh_user(&SshConfig::default()).unwrap(), login);
        }
    }
}
#[cfg(test)]
#[test]
#[ignore]
fn tmp_live_key_auth() {
    let cfg = SshConfig { host: "manus-mac-mini".into(), ..SshConfig::default() };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let out = rt.block_on(super::ssh_exec("tmp-live", &cfg, "", "echo key-auth-ok; whoami"));
    println!("RESULT: {:?}", out.map(|o| format!("{o:?}")).map_err(|e| e.to_string()));
}
