//! GitHub device flow + Copilot `/copilot_internal/v2/token` exchange (see `packages/ai/src/utils/oauth/github-copilot.ts`).

use super::store::{merge_copilot, save_oauth_store, CopilotOAuthRecord, OAuthStore};
use base64::Engine;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::Deserialize;

/// OAuth App client id (same as pi `packages/ai`).
const CLIENT_ID_B64: &str = "SXYxLmI1MDdhMDhjODdlY2ZlOTg=";

const COPILOT_HEADERS: [(&str, &str); 4] = [
    ("User-Agent", "GitHubCopilotChat/0.35.0"),
    ("Editor-Version", "vscode/1.107.0"),
    ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
    ("Copilot-Integration-Id", "vscode-chat"),
];

fn github_client_id() -> String {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(CLIENT_ID_B64.trim().as_bytes())
        .ok();
    raw.and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_else(|| "Iv1.b507a08c87ecfe98".to_string())
}

pub fn normalize_domain(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    url::Url::parse(&with_scheme)
        .ok()
        .map(|u| u.host_str().unwrap_or("").to_string())
        .filter(|h| !h.is_empty())
}

fn urls_for_domain(domain: &str) -> (String, String, String) {
    (
        format!("https://{domain}/login/device/code"),
        format!("https://{domain}/login/oauth/access_token"),
        format!("https://api.{domain}/copilot_internal/v2/token"),
    )
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: f64,
    expires_in: f64,
}

#[derive(Debug, Deserialize)]
struct TokenOk {
    access_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenErr {
    error: Option<String>,
    error_description: Option<String>,
    interval: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct CopilotTokenResponse {
    token: Option<String>,
    expires_at: Option<f64>,
}

async fn post_form(
    client: &reqwest::Client,
    url: &str,
    body: &[(&str, &str)],
) -> Result<String, String> {
    let mut params = std::collections::HashMap::new();
    for (k, v) in body {
        params.insert(k.to_string(), v.to_string());
    }
    let res = client
        .post(url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("User-Agent", "GitHubCopilotChat/0.35.0")
        .form(&params)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {}: {}", status, text));
    }
    Ok(text)
}

async fn start_device_flow(
    client: &reqwest::Client,
    domain: &str,
) -> Result<DeviceCodeResponse, String> {
    let (device_url, _, _) = urls_for_domain(domain);
    let cid = github_client_id();
    let text = post_form(
        client,
        &device_url,
        &[("client_id", cid.as_str()), ("scope", "read:user")],
    )
    .await?;
    serde_json::from_str(&text).map_err(|e| format!("device code JSON: {e}: {text}"))
}

pub async fn poll_github_access_token(
    client: &reqwest::Client,
    domain: &str,
    device_code: &str,
    interval_secs: f64,
    expires_in_secs: f64,
) -> Result<String, String> {
    let (_, access_url, _) = urls_for_domain(domain);
    let cid = github_client_id();
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(expires_in_secs.max(1.0) as u64);
    let mut interval_ms = (interval_secs * 1000.0).max(1000.0) as u64;
    let mut mult = 1.2_f64;

    while std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(
            ((interval_ms as f64) * mult).min(10_000.0) as u64,
        ))
        .await;

        let text = post_form(
            client,
            &access_url,
            &[
                ("client_id", cid.as_str()),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ],
        )
        .await;

        let text = match text {
            Ok(t) => t,
            Err(e) => return Err(e),
        };

        if let Ok(ok) = serde_json::from_str::<TokenOk>(&text) {
            if let Some(at) = ok.access_token {
                return Ok(at);
            }
        }
        if let Ok(err) = serde_json::from_str::<TokenErr>(&text) {
            if let Some(e) = err.error {
                if e == "authorization_pending" {
                    continue;
                }
                if e == "slow_down" {
                    interval_ms = err
                        .interval
                        .map(|x| (x * 1000.0) as u64)
                        .unwrap_or(interval_ms + 5000)
                        .max(1000);
                    mult = 1.4;
                    continue;
                }
                let suf = err
                    .error_description
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default();
                return Err(format!("device flow: {e}{suf}"));
            }
        }
    }
    Err("Device flow timed out".into())
}

pub async fn refresh_github_copilot_api_token(
    client: &reqwest::Client,
    github_access_token: &str,
    enterprise_domain: Option<&str>,
) -> Result<(String, i64), String> {
    let domain = enterprise_domain.unwrap_or("github.com");
    let (_, _, copilot_url) = urls_for_domain(domain);
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {github_access_token}"))
            .map_err(|e| e.to_string())?,
    );
    headers.insert("Accept", HeaderValue::from_static("application/json"));
    for (k, v) in COPILOT_HEADERS {
        headers.insert(
            reqwest::header::HeaderName::from_bytes(k.as_bytes()).map_err(|e| e.to_string())?,
            HeaderValue::from_str(v).map_err(|e| e.to_string())?,
        );
    }
    let res = client
        .get(&copilot_url)
        .headers(headers)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("Copilot token HTTP {}: {}", status, text));
    }
    let v: CopilotTokenResponse =
        serde_json::from_str(&text).map_err(|e| format!("{e}: {text}"))?;
    let token = v.token.ok_or_else(|| "missing token field".to_string())?;
    let exp = v
        .expires_at
        .ok_or_else(|| "missing expires_at".to_string())?;
    // Refresh 5 minutes early (same as TS)
    let expires_ms = (exp * 1000.0) as i64 - 5 * 60 * 1000;
    Ok((token, expires_ms))
}

/// Extract `https://api.*` base URL from Copilot token `proxy-ep=...` claim.
pub fn get_copilot_api_base_url(copilot_token: &str, enterprise_domain: Option<&str>) -> String {
    if let Some(idx) = copilot_token.find("proxy-ep=") {
        let rest = &copilot_token[idx + "proxy-ep=".len()..];
        let end = rest.find(';').unwrap_or(rest.len());
        let proxy_host = &rest[..end];
        let core = proxy_host.strip_prefix("proxy.").unwrap_or(proxy_host);
        let host = if core.starts_with("api.") {
            core.to_string()
        } else {
            format!("api.{core}")
        };
        return format!("https://{host}");
    }
    if let Some(d) = enterprise_domain {
        let d = d.trim();
        if !d.is_empty() {
            return format!("https://copilot-api.{d}");
        }
    }
    "https://api.individual.githubcopilot.com".to_string()
}

/// Full login: device flow + poll + Copilot token + save.
pub async fn login_github_copilot(
    enterprise_input: &str,
    tx: std::sync::mpsc::Sender<super::OAuthUiMsg>,
) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;

    let trimmed = enterprise_input.trim();
    let enterprise_domain: Option<String> = if trimmed.is_empty() {
        None
    } else {
        Some(
            normalize_domain(trimmed)
                .ok_or_else(|| "Invalid GitHub Enterprise URL/domain".to_string())?,
        )
    };
    let domain = enterprise_domain.as_deref().unwrap_or("github.com");

    let device = start_device_flow(&client, domain).await?;
    let _ = tx.send(super::OAuthUiMsg::GitHubDevice {
        url: device.verification_uri.clone(),
        user_code: device.user_code.clone(),
    });
    let _ = webbrowser::open(&device.verification_uri);

    let gh = poll_github_access_token(
        &client,
        domain,
        &device.device_code,
        device.interval,
        device.expires_in,
    )
    .await?;

    let (copilot_token, copilot_expires_ms) =
        refresh_github_copilot_api_token(&client, &gh, enterprise_domain.as_deref()).await?;

    let mut store = super::store::load_oauth_store();
    let rec = CopilotOAuthRecord {
        github_access_token: gh,
        copilot_token,
        copilot_expires_ms,
        enterprise_domain: enterprise_domain.clone(),
    };
    merge_copilot(&mut store, rec);
    save_oauth_store(&store).map_err(|e| e.to_string())?;
    Ok(())
}

/// Ensure Copilot token is valid; refresh using stored GitHub token if needed.
pub async fn ensure_copilot_token(
    client: &reqwest::Client,
    store: &mut OAuthStore,
) -> Result<String, String> {
    let Some(rec) = store.github_copilot.as_ref() else {
        return Err("Not signed in with GitHub Copilot OAuth.".into());
    };
    let now = chrono::Utc::now().timestamp_millis();
    if rec.copilot_expires_ms > now + 60_000 {
        return Ok(rec.copilot_token.clone());
    }
    let (copilot_token, copilot_expires_ms) = refresh_github_copilot_api_token(
        client,
        &rec.github_access_token,
        rec.enterprise_domain.as_deref(),
    )
    .await
    .map_err(|e| format!("Copilot token refresh: {e}"))?;
    if let Some(r) = store.github_copilot.as_mut() {
        r.copilot_token = copilot_token.clone();
        r.copilot_expires_ms = copilot_expires_ms;
    }
    save_oauth_store(store).map_err(|e| e.to_string())?;
    Ok(copilot_token)
}
