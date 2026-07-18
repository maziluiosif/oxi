//! Remote compute targets: SSH tunnels to model runtimes (Ollama / LM Studio) running on
//! another host reachable only over SSH.

mod ssh;
pub mod store;
mod tunnel;

pub use ssh::exec as ssh_exec;
pub use store::{load_ssh_credentials, save_ssh_credentials};
pub use tunnel::{TunnelError, TunnelManager};

use crate::settings::ProviderConfig;

/// Resolve the base URL to actually use for a provider's requests.
///
/// For [`crate::settings::ComputeLocation::Local`] this is just
/// [`ProviderConfig::effective_base_url`]. For `RemoteSsh`, this ensures an SSH tunnel is
/// up (connecting/reconnecting as needed) and returns a `127.0.0.1:<local port>` URL that
/// forwards to the runtime's port on the remote host.
///
/// Both Ollama and LM Studio (the two runtimes `RemoteSsh` targets in practice) expose
/// their OpenAI-compatible API under `/v1`, so the tunneled URL always uses that suffix
/// rather than trying to preserve a custom `base_url` path.
pub async fn resolve_base_url(
    config: &ProviderConfig,
    tunnels: &TunnelManager,
) -> Result<String, String> {
    let Some(ssh) = config.ssh_config() else {
        return Ok(config.effective_base_url());
    };
    let key = config.provider.slug();
    let creds = load_ssh_credentials();
    let password = creds
        .get(key)
        // Remote HF was formerly represented by Local HF + Remote SSH. Preserve that
        // credential during the provider split, including for chat/model-list tunnels.
        .or_else(|| {
            (config.provider == crate::settings::LlmProviderKind::RemoteHf)
                .then(|| creds.get(crate::settings::LlmProviderKind::LocalHf.slug()))
                .flatten()
        })
        .unwrap_or_default();
    let ok = tunnels
        .ensure_tunnel(key, ssh, password)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("http://127.0.0.1:{}/v1", ok.local_port))
}
