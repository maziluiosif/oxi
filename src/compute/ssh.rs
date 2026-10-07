//! Small SSH command execution helper used by oxi-managed remote runtimes.

use russh::ChannelMsg;

use crate::settings::SshConfig;

use super::TunnelError;
use super::connect::connect;

#[derive(Debug, Clone)]
pub struct RemoteOutput {
    pub status: u32,
    pub stdout: String,
    pub stderr: String,
}

/// Run `command` on `config`'s host. `key` is the provider slug the host key is pinned under,
/// so a first-use key seen here gets pinned like one seen by a tunnel.
pub async fn exec(
    key: &str,
    config: &SshConfig,
    password: &str,
    command: &str,
) -> Result<RemoteOutput, TunnelError> {
    let session = connect(key, config, password).await?;
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
    })
}
