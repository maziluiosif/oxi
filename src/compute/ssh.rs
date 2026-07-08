//! Small SSH command execution helper used by oxi-managed remote runtimes.

use std::sync::{Arc, Mutex};

use russh::ChannelMsg;
use russh::client::{self, AuthResult};
use russh::keys::{HashAlg, PublicKey};

use crate::settings::SshConfig;

use super::TunnelError;

#[derive(Debug, Clone)]
pub struct RemoteOutput {
    pub status: u32,
    pub stdout: String,
    pub stderr: String,
    #[allow(dead_code)]
    pub host_key_fingerprint: String,
}

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

pub async fn exec(config: &SshConfig, password: &str, command: &str) -> Result<RemoteOutput, TunnelError> {
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

    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| TunnelError::Other(format!("SSH open session failed: {e}")))?;
    channel
        .exec(true, command)
        .await
        .map_err(|e| TunnelError::Other(format!("SSH exec failed: {e}")))?;

    let mut status = None;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
            ChannelMsg::ExtendedData { data, .. } => stderr.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
            _ => {}
        }
    }
    Ok(RemoteOutput {
        status: status.unwrap_or(255),
        stdout: String::from_utf8_lossy(&stdout).to_string(),
        stderr: String::from_utf8_lossy(&stderr).to_string(),
        host_key_fingerprint,
    })
}
