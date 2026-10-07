//! OpenAI Codex (ChatGPT) OAuth — PKCE + `http://localhost:1455/auth/callback` (see `packages/ai/src/utils/oauth/openai-codex.ts`).

use base64::Engine;
use rand::TryRng;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::form_urlencoded;

use super::store::{CodexOAuthRecord, OAuthStore, merge_codex, save_oauth_store};

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
    rand::rng()
        .try_fill_bytes(&mut b)
        .expect("OS RNG unavailable");
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

fn generate_pkce() -> (String, String) {
    let mut v = [0u8; 32];
    rand::rng()
        .try_fill_bytes(&mut v)
        .expect("OS RNG unavailable");
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

/// How long the localhost callback waits for the browser sign-in before giving up and freeing
/// the port, so closing the tab doesn't leave the login stuck.
const CALLBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Bind the redirect listener. Done before the browser opens, so a busy port fails at once
/// instead of after the user has signed in.
async fn bind_callback_listener() -> Result<TcpListener, String> {
    TcpListener::bind("127.0.0.1:1455")
        .await
        .map_err(|e| format!("Bind 127.0.0.1:1455 failed (is another app using it?): {e}"))
}

/// Wait for `GET /auth/callback?code=...&state=...` and return the code. Other requests
/// (browser pre-connects, `/favicon.ico`, a stale tab's callback with an old `state`) are
/// answered and ignored; an `error=` callback for this login ends it.
async fn wait_localhost_callback(
    listener: TcpListener,
    expected_state: &str,
) -> Result<String, String> {
    tokio::time::timeout(CALLBACK_TIMEOUT, accept_callback(&listener, expected_state))
        .await
        .map_err(|_| "Timed out waiting for the browser sign-in. Try again.".to_string())?
}

async fn accept_callback(listener: &TcpListener, expected_state: &str) -> Result<String, String> {
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|e| format!("accept: {e}"))?;
        let Some(path) = read_request_path(&mut stream).await else {
            continue;
        };
        let Some(query) = path
            .strip_prefix("/auth/callback")
            .and_then(|rest| rest.strip_prefix('?'))
        else {
            respond(&mut stream, "404 Not Found", &err_html("not found")).await;
            continue;
        };
        let (mut code, mut state, mut error) = (None, None, None);
        for (k, v) in form_urlencoded::parse(query.as_bytes()) {
            match k.as_ref() {
                "code" => code = Some(v.into_owned()),
                "state" => state = Some(v.into_owned()),
                "error_description" => error = Some(v.into_owned()),
                "error" if error.is_none() => error = Some(v.into_owned()),
                _ => {}
            }
        }
        if state.as_deref() != Some(expected_state) {
            respond(
                &mut stream,
                "400 Bad Request",
                &err_html("this sign-in link is stale; finish the latest one"),
            )
            .await;
            continue;
        }
        match (code, error) {
            (Some(code), None) => {
                respond(&mut stream, "200 OK", success_html()).await;
                return Ok(code);
            }
            (_, error) => {
                let error = error.unwrap_or_else(|| "missing authorization code".to_string());
                respond(&mut stream, "400 Bad Request", &err_html(&error)).await;
                return Err(format!("OAuth sign-in failed: {error}"));
            }
        }
    }
}

/// The request target of an HTTP request on `stream`, or `None` when the client sent nothing
/// usable (a speculative pre-connect closes without a request).
async fn read_request_path(stream: &mut tokio::net::TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let read_line = async {
        while !buf.windows(2).any(|w| w == b"\r\n") && buf.len() < 8192 {
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        Some(())
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), read_line)
        .await
        .ok()??;
    let request = String::from_utf8_lossy(&buf);
    let first_line = request.lines().next()?;
    let mut parts = first_line.split_whitespace();
    (parts.next()? == "GET").then_some(())?;
    parts.next().map(str::to_string)
}

async fn respond(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    );
    let _ = stream.write_all(response.as_bytes()).await;
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
    let listener = bind_callback_listener().await?;
    let _ = tx.send(super::OAuthUiMsg::CodexOpenBrowser { url: url.clone() });
    let _ = webbrowser::open(&url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;

    let code = wait_localhost_callback(listener, &state).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(port: u16, path: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response).await;
        response
    }

    async fn listener() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    #[tokio::test]
    async fn callback_skips_preconnects_other_paths_and_stale_states() {
        let (listener, port) = listener().await;
        let waiter = tokio::spawn(wait_localhost_callback(listener, "good"));

        // A speculative pre-connect that never sends a request.
        drop(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap(),
        );
        assert!(get(port, "/favicon.ico").await.starts_with("HTTP/1.1 404"));
        assert!(
            get(port, "/auth/callback?code=old&state=stale")
                .await
                .starts_with("HTTP/1.1 400")
        );
        assert!(
            get(port, "/auth/callback?code=fresh&state=good")
                .await
                .starts_with("HTTP/1.1 200")
        );
        assert_eq!(waiter.await.unwrap().unwrap(), "fresh");
    }

    #[tokio::test]
    async fn callback_reports_a_denied_sign_in() {
        let (listener, port) = listener().await;
        let waiter = tokio::spawn(wait_localhost_callback(listener, "s"));
        let response = get(
            port,
            "/auth/callback?error=access_denied&error_description=User%20cancelled&state=s",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 400"));
        let error = waiter.await.unwrap().unwrap_err();
        assert!(error.contains("User cancelled"), "{error}");
    }
}
