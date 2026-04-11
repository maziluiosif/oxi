//! OAuth login for GitHub Copilot (device flow) and OpenAI Codex (PKCE + localhost callback).

mod codex;
mod github;
mod store;

pub use codex::{ensure_codex_access_token, login_openai_codex};
pub use github::{ensure_copilot_token, get_copilot_api_base_url, login_github_copilot};
pub use store::{
    clear_codex, clear_copilot, load_oauth_store, oauth_config_path, save_oauth_store,
};

/// Messages from background OAuth threads to the UI (drain each frame).
#[derive(Debug)]
pub enum OAuthUiMsg {
    GitHubDevice { url: String, user_code: String },
    GitHubDone(Result<(), String>),
    CodexOpenBrowser { url: String },
    CodexDone(Result<(), String>),
}
