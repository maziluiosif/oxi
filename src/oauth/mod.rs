//! OAuth login for OpenAI Codex (PKCE + localhost callback).

mod codex;
mod store;

pub use codex::{ensure_codex_access_token, login_openai_codex};
pub use store::{OAuthStore, clear_codex, load_oauth_store, save_oauth_store};

/// Messages from background OAuth threads to the UI (drain each frame).
#[derive(Debug)]
pub enum OAuthUiMsg {
    CodexOpenBrowser { url: String },
    CodexDone(Result<(), String>),
}
