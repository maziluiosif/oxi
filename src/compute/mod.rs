//! Remote compute targets: SSH tunnels to model runtimes (Ollama / LM Studio) running on
//! another host, e.g. a Mac mini reachable only over SSH.

pub mod store;
mod tunnel;

pub use store::{load_ssh_credentials, save_ssh_credentials};
pub use tunnel::TunnelManager;

use crate::settings::ProviderProfile;

/// Resolve the base URL to actually use for a profile's requests.
///
/// For [`crate::settings::ComputeLocation::Local`] this is just
/// [`ProviderProfile::effective_base_url`]. For `RemoteSsh`, this ensures an SSH tunnel is
/// up (connecting/reconnecting as needed) and returns a `127.0.0.1:<local port>` URL that
/// forwards to the runtime's port on the remote host.
///
/// Both Ollama and LM Studio (the two runtimes `RemoteSsh` targets in practice) expose
/// their OpenAI-compatible API under `/v1`, so the tunneled URL always uses that suffix
/// rather than trying to preserve a custom `base_url` path.
pub async fn resolve_base_url(
    profile: &ProviderProfile,
    tunnels: &TunnelManager,
) -> Result<String, String> {
    let Some(cfg) = profile.ssh_config() else {
        return Ok(profile.effective_base_url());
    };
    let creds = load_ssh_credentials();
    let password = creds.get(&profile.id).unwrap_or_default();
    let local_port = tunnels.ensure_tunnel(&profile.id, cfg, password).await?;
    Ok(format!("http://127.0.0.1:{local_port}/v1"))
}
