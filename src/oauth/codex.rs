//! OpenAI Codex (ChatGPT) OAuth — PKCE + `http://localhost:1455/auth/callback` (see `packages/ai/src/utils/oauth/openai-codex.ts`).

use base64::Engine;
use rand::RngCore;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::form_urlencoded;

use super::store::{merge_codex, save_oauth_store, CodexOAuthRecord, OAuthStore};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const SCOPE: &str = "openid profile email offline_access";
const JWT_AUTH: &str = "https://api.openai.com/auth";

fn base64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random_state() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

fn generate_pkce() -> (String, String) {
    let mut v = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut v);
    let verifier = base64url(&v);
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = base64url(hasher.finalize().as_ref());
    (verifier, challenge)
}

fn success_html() -> &'static str {
    "<!DOCTYPE html><html><body><p>Authentication completed. You can close this window.</p></body></html>"
}

fn err_html(msg: &str) -> String {
    format!(
        "<!DOCTYPE html><html><body><p>OAuth error: {}</p></body></html>",
        html_escape(msg)
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Wait for one GET /auth/callback?code=...&state=...
pub async fn wait_localhost_callback(expected_state: &str) -> Result<String, String> {
    let listener = TcpListener::bind("127.0.0.1:1455")
        .await
        .map_err(|e| format!("Bind 127.0.0.1:1455 failed (is another app using it?): {e}"))?;

    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|e| format!("accept: {e}"))?;
    drop(listener);

    let mut buf = vec![0u8; 8192];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| format!("read: {e}"))?;
    let req = String::from_utf8_lossy(&buf[..n]);

    let first_line = req.lines().next().unwrap_or("");
    let path = first_line.split_whitespace().nth(1).unwrap_or("");

    let (code, state) = if let Some(q) = path.find('?') {
        let query = &path[q + 1..];
        let mut code_v = None;
        let mut state_v = None;
        for (k, v) in form_urlencoded::parse(query.as_bytes()) {
            match k.as_ref() {
                "code" => code_v = Some(v.into_owned()),
                "state" => state_v = Some(v.into_owned()),
                _ => {}
            }
        }
        (code_v, state_v)
    } else {
        (None, None)
    };

    let ok = state.as_deref() == Some(expected_state) && code.is_some();
    let response = if ok {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            success_html().len(),
            success_html()
        )
    } else {
        let body = err_html("state mismatch or missing code");
        format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    };
    let _ = stream.write_all(response.as_bytes()).await;

    if !ok {
        return Err("OAuth callback: missing code or state mismatch".into());
    }
    code.ok_or_else(|| "OAuth callback: missing authorization code".to_string())
}

#[derive(Deserialize)]
struct TokenJson {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

async fn exchange_authorization_code(
    client: &reqwest::Client,
    code: &str,
    verifier: &str,
) -> Result<TokenJson, String> {
    let body = [
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", REDIRECT_URI),
    ];
    let res = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("token exchange HTTP {}: {}", status, text));
    }
    serde_json::from_str(&text).map_err(|e| format!("token JSON: {e}: {text}"))
}

async fn refresh_openai_codex_token(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<TokenJson, String> {
    let body = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", CLIENT_ID),
    ];
    let res = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("refresh HTTP {}: {}", status, text));
    }
    serde_json::from_str(&text).map_err(|e| format!("refresh JSON: {e}: {text}"))
}

fn extract_account_id(access_token: &str) -> Result<String, String> {
    let parts: Vec<&str> = access_token.split('.').collect();
    if parts.len() != 3 {
        return Err("Invalid JWT shape".into());
    }
    let payload = parts[1];
    let pad = (4 - payload.len() % 4) % 4;
    let padded = format!("{}{}", payload, "=".repeat(pad));
    let bytes = base64::engine::general_purpose::URL_SAFE
        .decode(padded.as_bytes())
        .map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let id = v
        .get(JWT_AUTH)
        .and_then(|x| x.get("chatgpt_account_id"))
        .and_then(|x| x.as_str());
    id.map(|s| s.to_string())
        .ok_or_else(|| "Missing chatgpt_account_id in token".into())
}

pub fn build_authorize_url(challenge: &str, state: &str) -> String {
    let mut u = match url::Url::parse(AUTHORIZE_URL) {
        Ok(url) => url,
        Err(_) => return AUTHORIZE_URL.to_string(),
    };
    u.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", "oxi");
    u.to_string()
}

/// Run full login: open browser, localhost callback, token save.
pub async fn login_openai_codex(
    tx: std::sync::mpsc::Sender<super::OAuthUiMsg>,
) -> Result<(), String> {
    let (verifier, challenge) = generate_pkce();
    let state = random_state();
    let url = build_authorize_url(&challenge, &state);
    let _ = tx.send(super::OAuthUiMsg::CodexOpenBrowser { url: url.clone() });
    let _ = webbrowser::open(&url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;

    let code = wait_localhost_callback(&state).await?;
    let tok = exchange_authorization_code(&client, &code, &verifier).await?;
    let account_id = extract_account_id(&tok.access_token)?;
    let expires_ms = chrono::Utc::now().timestamp_millis() + tok.expires_in * 1000;

    let mut store = super::store::load_oauth_store();
    let rec = CodexOAuthRecord {
        refresh_token: tok.refresh_token,
        access_token: tok.access_token,
        expires_ms,
        account_id,
    };
    merge_codex(&mut store, rec);
    save_oauth_store(&store).map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn ensure_codex_access_token(
    client: &reqwest::Client,
    store: &mut OAuthStore,
) -> Result<(String, String), String> {
    let Some(rec) = store.openai_codex.as_ref() else {
        return Err("Not signed in with ChatGPT (Codex) OAuth.".into());
    };
    let now = chrono::Utc::now().timestamp_millis();
    if rec.expires_ms > now + 60_000 {
        return Ok((rec.access_token.clone(), rec.account_id.clone()));
    }
    let tok = refresh_openai_codex_token(client, &rec.refresh_token)
        .await
        .map_err(|e| format!("Codex token refresh: {e}"))?;
    let account_id = extract_account_id(&tok.access_token)?;
    let expires_ms = chrono::Utc::now().timestamp_millis() + tok.expires_in * 1000;
    if let Some(r) = store.openai_codex.as_mut() {
        r.access_token = tok.access_token.clone();
        r.refresh_token = tok.refresh_token;
        r.expires_ms = expires_ms;
        r.account_id = account_id.clone();
    }
    save_oauth_store(store).map_err(|e| e.to_string())?;
    Ok((tok.access_token, account_id))
}
