//! Subscription / credit state per provider, shared by the router and the settings UI.
//!
//! Sources, in order of trust:
//! - **GPT Codex (oxi OAuth)**: `GET chatgpt.com/backend-api/wham/usage` (what Codex CLI's
//!   `/status` shows) plus the `x-codex-primary-*` / `x-codex-secondary-*` headers on every
//!   Codex response, which keep the numbers fresh for free.
//! - **Codex (ACP)**: the same endpoint with Codex CLI's own token (`~/.codex/auth.json`), opt-in.
//! - **Claude Code (ACP)**: `GET api.anthropic.com/api/oauth/usage` (what Claude Code's `/usage`
//!   shows) with Claude Code's OAuth token, opt-in. The token is never refreshed here: refreshing
//!   rotates the refresh token and would sign Claude Code out.
//! - **OpenRouter**: `GET /api/v1/key` for credit left, `GET /api/v1/models` for live prices.
//! - Anything else: rate-limit / quota errors put the provider on a cooldown.
//!
//! The Codex and Claude endpoints are not public APIs; every parser is lenient and a failed
//! probe only leaves an error on the snapshot, it never blocks routing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::catalog::Price;
use crate::settings::{AppSettings, LlmProviderKind};

/// One usage window of a subscription ("5h", "7d", "7d Opus").
#[derive(Debug, Clone, PartialEq)]
pub struct UsageWindow {
    pub label: String,
    /// 0–100.
    pub used_pct: f64,
    /// Unix seconds.
    pub resets_at: Option<u64>,
    /// Lowercase model family this window is limited to (`opus`, `sonnet`), if any.
    pub model_scope: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuotaSnapshot {
    pub windows: Vec<UsageWindow>,
    /// Remaining credit in USD (OpenRouter key limit, Codex credits), when reported.
    pub credits_left: Option<f64>,
    /// Plan name when reported (`plus`, `pro`, `max`…).
    pub plan: Option<String>,
    /// The provider says the limit is hit right now.
    pub limit_reached: bool,
    /// Where the numbers came from, for the UI.
    pub source: String,
    /// Unix seconds of the last successful update.
    pub updated_at: u64,
    /// Last probe error; the previous numbers (if any) are kept alongside it.
    pub error: Option<String>,
}

impl QuotaSnapshot {
    /// Highest used percentage among the windows that apply to `model`.
    pub fn used_pct_for(&self, model: &str) -> Option<f64> {
        let m = model.to_ascii_lowercase();
        let now = now_secs();
        self.windows
            .iter()
            .filter(|w| {
                w.model_scope
                    .as_ref()
                    .is_none_or(|s| m.contains(s.as_str()))
            })
            // A window whose reset already passed is empty again.
            .map(|w| match w.resets_at {
                Some(t) if t <= now => 0.0,
                _ => w.used_pct,
            })
            .reduce(f64::max)
    }
}

#[derive(Default)]
struct Board {
    snapshots: HashMap<LlmProviderKind, QuotaSnapshot>,
    /// Provider → unix seconds until which the router should avoid it.
    cooldowns: HashMap<LlmProviderKind, u64>,
    /// OpenRouter live prices by model id.
    openrouter_prices: HashMap<String, Price>,
    /// Unix seconds of the last [`refresh`] start, to rate-limit probing.
    last_refresh: u64,
}

fn board() -> &'static Mutex<Board> {
    static BOARD: OnceLock<Mutex<Board>> = OnceLock::new();
    BOARD.get_or_init(|| Mutex::new(Board::default()))
}

fn with_board<R>(f: impl FnOnce(&mut Board) -> R) -> R {
    let mut guard = board().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
pub fn set_snapshot_for_tests(kind: LlmProviderKind, snap: QuotaSnapshot) {
    with_board(|b| {
        b.snapshots.insert(kind, snap);
    });
}

pub fn snapshot(kind: LlmProviderKind) -> Option<QuotaSnapshot> {
    with_board(|b| b.snapshots.get(&kind).cloned())
}

pub fn openrouter_price(model: &str) -> Option<Price> {
    with_board(|b| b.openrouter_prices.get(model.trim()).copied())
}

/// Seconds left on a provider's cooldown, if it is cooling down.
pub fn cooldown_left(kind: LlmProviderKind) -> Option<u64> {
    let now = now_secs();
    with_board(|b| b.cooldowns.get(&kind).copied())
        .filter(|until| *until > now)
        .map(|until| until - now)
}

/// Keep the router away from `kind` for a while after a rate-limit / quota failure. Uses the
/// reported reset time when the snapshot has one for an exhausted window.
pub fn start_cooldown(kind: LlmProviderKind, default: Duration) {
    let now = now_secs();
    with_board(|b| {
        let reset = b.snapshots.get(&kind).and_then(|s| {
            s.windows
                .iter()
                .filter(|w| w.used_pct >= 99.0)
                .filter_map(|w| w.resets_at)
                .filter(|t| *t > now)
                .min()
        });
        let until = reset.unwrap_or(now + default.as_secs());
        b.cooldowns.insert(kind, until);
    });
}

pub fn clear_cooldown(kind: LlmProviderKind) {
    with_board(|b| {
        b.cooldowns.remove(&kind);
    });
}

fn store(kind: LlmProviderKind, result: Result<QuotaSnapshot, String>) {
    with_board(|b| match result {
        Ok(snap) => {
            if !snap.limit_reached {
                b.cooldowns.remove(&kind);
            }
            b.snapshots.insert(kind, snap);
        }
        Err(e) => {
            b.snapshots.entry(kind).or_default().error = Some(e);
        }
    });
}

/// Does `error` (an agent run failure) mean the provider is out of quota or rate limited?
pub fn is_quota_error(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    [
        "http 429",
        "http 402",
        "rate limit",
        "rate_limit",
        "quota exceeded",
        "quota exhausted",
        "quota reached",
        "out of quota",
        "quota_exceeded",
        "exceeded your current quota",
        "quota limit",
        "out of credits",
        "usage limit",
        "usage_limit",
        "limit reached",
        "hit your limit",
        "insufficient_quota",
        "insufficient credits",
    ]
    .iter()
    .any(|needle| e.contains(needle))
}

// ── Response headers ─────────────────────────────────────────────────────────

/// Pick up quota headers from any provider response. Codex sends its usage windows on every
/// response, so the router sees the numbers move without polling.
pub fn observe_headers(headers: &reqwest::header::HeaderMap) {
    if let Some(snap) = parse_codex_headers(headers) {
        with_board(|b| {
            let entry = b.snapshots.entry(LlmProviderKind::GptCodex).or_default();
            // Headers carry the windows only; keep plan / credits from the last full probe.
            entry.windows = snap.windows;
            if snap.credits_left.is_some() {
                entry.credits_left = snap.credits_left;
            }
            entry.limit_reached = false;
            entry.source = snap.source;
            entry.updated_at = snap.updated_at;
            entry.error = None;
        });
    }
}

fn parse_codex_headers(headers: &reqwest::header::HeaderMap) -> Option<QuotaSnapshot> {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let num = |name: &str| get(name).and_then(|s| s.parse::<f64>().ok());
    let mut windows = Vec::new();
    for which in ["primary", "secondary"] {
        let Some(used) = num(&format!("x-codex-{which}-used-percent")) else {
            continue;
        };
        let minutes = num(&format!("x-codex-{which}-window-minutes"));
        let resets_at = num(&format!("x-codex-{which}-reset-at"))
            .map(|t| t as u64)
            .or_else(|| {
                num(&format!("x-codex-{which}-reset-after-seconds")).map(|s| now_secs() + s as u64)
            });
        windows.push(UsageWindow {
            label: minutes
                .map(|m| window_label((m * 60.0) as u64))
                .unwrap_or_else(|| which.to_string()),
            used_pct: used,
            resets_at,
            model_scope: None,
        });
    }
    if windows.is_empty() {
        return None;
    }
    let unlimited = get("x-codex-credits-unlimited") == Some("true");
    let credits_left = if unlimited {
        None
    } else {
        num("x-codex-credits-balance")
    };
    Some(QuotaSnapshot {
        windows,
        credits_left,
        source: "Codex response headers".into(),
        updated_at: now_secs(),
        ..Default::default()
    })
}

fn window_label(seconds: u64) -> String {
    match seconds {
        0 => "window".into(),
        s if s % 86_400 == 0 => format!("{}d", s / 86_400),
        s if s % 3_600 == 0 => format!("{}h", s / 3_600),
        s => format!("{}m", s / 60),
    }
}

// ── Probes ───────────────────────────────────────────────────────────────────

/// Seconds between background refreshes. Routing and manual refreshes bypass this interval.
const AUTO_REFRESH_SECS: u64 = 300;

static REFRESHING: AtomicBool = AtomicBool::new(false);

/// A probe round is in flight (the settings page shows a spinner).
pub fn is_refreshing() -> bool {
    REFRESHING.load(Ordering::Relaxed)
}

struct RefreshGuard;

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        REFRESHING.store(false, Ordering::Relaxed);
    }
}

/// Refresh every probe that applies to the configured providers. `force` ignores the
/// auto-refresh interval (routing decisions and the Settings "Refresh" button).
pub async fn refresh(settings: &AppSettings, force: bool) {
    let now = now_secs();
    let due = with_board(|b| {
        if force || now.saturating_sub(b.last_refresh) >= AUTO_REFRESH_SECS {
            b.last_refresh = now;
            true
        } else {
            false
        }
    });
    if !due {
        return;
    }
    REFRESHING.store(true, Ordering::Relaxed);
    let _guard = RefreshGuard;
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    let oauth = crate::oauth::load_oauth_store();
    let configured = settings.configured_provider_kinds(&oauth);
    let has = |k: LlmProviderKind| configured.contains(&k);
    let codex_oxi = async {
        if has(LlmProviderKind::GptCodex) && oauth.openai_codex.is_some() {
            let mut oauth = oauth.clone();
            let r = match crate::oauth::ensure_codex_access_token(&client, &mut oauth).await {
                Ok((token, account)) => probe_codex(&client, &token, &account, "oxi sign-in").await,
                Err(e) => Err(e),
            };
            store(LlmProviderKind::GptCodex, r);
        }
    };
    let codex_cli = async {
        if has(LlmProviderKind::CodexAcp) && settings.router.read_codex_cli_usage {
            let r = match codex_cli_token() {
                Ok((token, account)) => {
                    probe_codex(&client, &token, &account, "Codex CLI login").await
                }
                Err(e) => Err(e),
            };
            store(LlmProviderKind::CodexAcp, r);
        }
    };
    let claude = async {
        if has(LlmProviderKind::ClaudeCodeAcp) && settings.router.read_claude_code_usage {
            let r = match claude_code_token().await {
                Ok(token) => probe_claude(&client, &token).await,
                Err(e) => Err(e),
            };
            store(LlmProviderKind::ClaudeCodeAcp, r);
        }
    };
    let openrouter = async {
        if has(LlmProviderKind::OpenRouter) {
            let cfg = settings.provider(LlmProviderKind::OpenRouter);
            let base = cfg.effective_base_url();
            if let Ok(key) = crate::agent::configured_openrouter_key(cfg) {
                store(
                    LlmProviderKind::OpenRouter,
                    probe_openrouter_key(&client, &base, &key).await,
                );
            }
            let need_prices = with_board(|b| b.openrouter_prices.is_empty());
            if need_prices && let Ok(prices) = fetch_openrouter_prices(&client, &base).await {
                with_board(|b| b.openrouter_prices = prices);
            }
        }
    };
    futures_util::join!(codex_oxi, codex_cli, claude, openrouter);
}

async fn get_json(req: reqwest::RequestBuilder) -> Result<Value, String> {
    let res = req.send().await.map_err(|e| e.to_string())?;
    let status = res.status();
    let body = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        let hint = match status.as_u16() {
            401 | 403 => " (token expired or not accepted)",
            404 => " (endpoint not available)",
            _ => "",
        };
        return Err(format!("HTTP {}{hint}", status.as_u16()));
    }
    serde_json::from_str(&body).map_err(|e| format!("unexpected response: {e}"))
}

async fn probe_codex(
    client: &reqwest::Client,
    token: &str,
    account_id: &str,
    via: &str,
) -> Result<QuotaSnapshot, String> {
    let mut req = client
        .get("https://chatgpt.com/backend-api/wham/usage")
        .bearer_auth(token)
        .header("originator", "oxi")
        .header("accept", "application/json");
    if !account_id.is_empty() {
        req = req.header("chatgpt-account-id", account_id);
    }
    let v = get_json(req).await?;
    let mut snap = parse_codex_usage(&v);
    snap.source = format!("ChatGPT usage ({via})");
    Ok(snap)
}

/// `wham/usage`: `{ plan_type, rate_limit: { limit_reached, primary_window: { used_percent,
/// limit_window_seconds, reset_after_seconds, reset_at }, secondary_window }, credits: {
/// has_credits, unlimited, balance } }`.
pub(crate) fn parse_codex_usage(v: &Value) -> QuotaSnapshot {
    let now = now_secs();
    let mut windows = Vec::new();
    let rl = &v["rate_limit"];
    for key in ["primary_window", "secondary_window"] {
        let w = &rl[key];
        let Some(used) = as_f64(&w["used_percent"]) else {
            continue;
        };
        let resets_at = as_f64(&w["reset_at"])
            .map(|t| t as u64)
            .or_else(|| as_f64(&w["reset_after_seconds"]).map(|s| now + s as u64));
        let label = as_f64(&w["limit_window_seconds"])
            .map(|s| window_label(s as u64))
            .unwrap_or_else(|| key.trim_end_matches("_window").to_string());
        windows.push(UsageWindow {
            label,
            used_pct: used,
            resets_at,
            model_scope: None,
        });
    }
    let credits = &v["credits"];
    let unlimited = credits["unlimited"].as_bool().unwrap_or(false);
    QuotaSnapshot {
        windows,
        credits_left: (!unlimited)
            .then(|| as_f64(&credits["balance"]))
            .flatten()
            .filter(|_| credits["has_credits"].as_bool().unwrap_or(true)),
        plan: v["plan_type"].as_str().map(str::to_string),
        limit_reached: rl["limit_reached"].as_bool().unwrap_or(false),
        source: String::new(),
        updated_at: now,
        error: None,
    }
}

async fn probe_claude(client: &reqwest::Client, token: &str) -> Result<QuotaSnapshot, String> {
    let req = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("content-type", "application/json");
    let v = get_json(req).await?;
    let mut snap = parse_claude_usage(&v);
    snap.source = "Claude usage (Claude Code login)".into();
    Ok(snap)
}

/// `/api/oauth/usage`: `{ five_hour: { utilization, resets_at }, seven_day: {…},
/// seven_day_opus: {…} | null, seven_day_sonnet: {…} | null, extra_usage: {…} }` with
/// `utilization` in percent and `resets_at` as RFC 3339.
pub(crate) fn parse_claude_usage(v: &Value) -> QuotaSnapshot {
    let mut windows = Vec::new();
    for (key, label, scope) in [
        ("five_hour", "5h", None),
        ("seven_day", "7d", None),
        ("seven_day_opus", "7d Opus", Some("opus")),
        ("seven_day_sonnet", "7d Sonnet", Some("sonnet")),
    ] {
        let w = &v[key];
        let Some(used) = as_f64(&w["utilization"]) else {
            continue;
        };
        let resets_at = w["resets_at"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.timestamp().max(0) as u64)
            .or_else(|| as_f64(&w["resets_at"]).map(|t| t as u64));
        windows.push(UsageWindow {
            label: label.into(),
            used_pct: used,
            resets_at,
            model_scope: scope.map(str::to_string),
        });
    }
    let limit_reached = windows
        .iter()
        .any(|w| w.model_scope.is_none() && w.used_pct >= 100.0);
    QuotaSnapshot {
        windows,
        limit_reached,
        updated_at: now_secs(),
        ..Default::default()
    }
}

async fn probe_openrouter_key(
    client: &reqwest::Client,
    base: &str,
    key: &str,
) -> Result<QuotaSnapshot, String> {
    let url = format!("{}/key", base.trim_end_matches('/'));
    let v = get_json(client.get(url).bearer_auth(key)).await?;
    let d = &v["data"];
    let credits_left = as_f64(&d["limit_remaining"]);
    Ok(QuotaSnapshot {
        credits_left,
        limit_reached: credits_left.is_some_and(|c| c <= 0.0),
        plan: d["is_free_tier"]
            .as_bool()
            .filter(|free| *free)
            .map(|_| "free tier".to_string()),
        source: "OpenRouter /key".into(),
        updated_at: now_secs(),
        ..Default::default()
    })
}

async fn fetch_openrouter_prices(
    client: &reqwest::Client,
    base: &str,
) -> Result<HashMap<String, Price>, String> {
    let url = format!("{}/models", base.trim_end_matches('/'));
    let v = get_json(client.get(url)).await?;
    let mut out = HashMap::new();
    for m in v["data"].as_array().into_iter().flatten() {
        let (Some(id), Some(input), Some(output)) = (
            m["id"].as_str(),
            as_f64(&m["pricing"]["prompt"]),
            as_f64(&m["pricing"]["completion"]),
        ) else {
            continue;
        };
        // Per-token strings → per million.
        out.insert(
            id.to_string(),
            Price {
                input: input * 1e6,
                output: output * 1e6,
            },
        );
    }
    Ok(out)
}

/// Numbers arrive as JSON numbers or numeric strings depending on the endpoint.
fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

// ── Other apps' credentials (opt-in, read-only) ───────────────────────────────

fn codex_cli_token() -> Result<(String, String), String> {
    let home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
        .ok_or("no home directory")?;
    let raw = std::fs::read_to_string(home.join("auth.json"))
        .map_err(|_| "Codex CLI is not signed in (no ~/.codex/auth.json)".to_string())?;
    let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let token = v["tokens"]["access_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or("Codex CLI uses an API key, not a ChatGPT login")?;
    let account = v["tokens"]["account_id"].as_str().unwrap_or_default();
    Ok((token.to_string(), account.to_string()))
}

async fn claude_code_token() -> Result<String, String> {
    let raw = claude_code_credentials_json().await?;
    let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let o = &v["claudeAiOauth"];
    let token = o["accessToken"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or("Claude Code is not signed in with a Claude subscription")?;
    if let Some(expires_ms) = o["expiresAt"].as_f64()
        && (expires_ms / 1000.0) as u64 <= now_secs()
    {
        return Err("Claude Code's token expired — run Claude Code once to refresh it".into());
    }
    Ok(token.to_string())
}

async fn claude_code_credentials_json() -> Result<String, String> {
    let file = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))
        .map(|d| d.join(".credentials.json"));
    #[cfg(target_os = "macos")]
    {
        // Claude Code keeps its login in the login keychain on macOS. The first read shows
        // the system's "allow access" prompt, which is why this is opt-in.
        let out = tokio::process::Command::new("/usr/bin/security")
            .args([
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ])
            .output()
            .await;
        if let Ok(out) = out
            && out.status.success()
        {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return Ok(s);
            }
        }
    }
    file.and_then(|p| std::fs::read_to_string(p).ok())
        .ok_or_else(|| "Claude Code login not found".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_codex_usage_payload() {
        let v = json!({
            "plan_type": "pro",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {"used_percent": 32, "limit_window_seconds": 18000,
                                   "reset_after_seconds": 600, "reset_at": 2000000000},
                "secondary_window": {"used_percent": 61.5, "limit_window_seconds": 604800,
                                     "reset_after_seconds": 9000}
            },
            "credits": {"has_credits": true, "unlimited": false, "balance": "4.20"}
        });
        let s = parse_codex_usage(&v);
        assert_eq!(s.plan.as_deref(), Some("pro"));
        assert_eq!(s.windows.len(), 2);
        assert_eq!(s.windows[0].label, "5h");
        assert_eq!(s.windows[0].resets_at, Some(2_000_000_000));
        assert_eq!(s.windows[1].label, "7d");
        assert_eq!(s.credits_left, Some(4.2));
        assert_eq!(s.used_pct_for("gpt-5"), Some(61.5));
    }

    #[test]
    fn parses_claude_usage_payload_with_model_windows() {
        let v = json!({
            "five_hour": {"utilization": 12.0, "resets_at": "2099-01-01T00:00:00+00:00"},
            "seven_day": {"utilization": 40.0, "resets_at": "2099-01-03T00:00:00Z"},
            "seven_day_opus": {"utilization": 90.0, "resets_at": "2099-01-03T00:00:00Z"},
            "seven_day_sonnet": null,
            "extra_usage": {"is_enabled": false}
        });
        let s = parse_claude_usage(&v);
        assert_eq!(s.windows.len(), 3);
        assert!(!s.limit_reached);
        assert_eq!(s.used_pct_for("opus"), Some(90.0));
        assert_eq!(s.used_pct_for("sonnet"), Some(40.0));
    }

    #[test]
    fn expired_window_counts_as_empty() {
        let s = QuotaSnapshot {
            windows: vec![UsageWindow {
                label: "5h".into(),
                used_pct: 100.0,
                resets_at: Some(1),
                model_scope: None,
            }],
            ..Default::default()
        };
        assert_eq!(s.used_pct_for("x"), Some(0.0));
    }

    #[test]
    fn codex_headers_become_windows() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("x-codex-primary-used-percent", "25".parse().unwrap());
        h.insert("x-codex-primary-window-minutes", "300".parse().unwrap());
        h.insert("x-codex-secondary-used-percent", "70".parse().unwrap());
        h.insert("x-codex-secondary-window-minutes", "10080".parse().unwrap());
        let s = parse_codex_headers(&h).unwrap();
        assert_eq!(s.windows[0].label, "5h");
        assert_eq!(s.windows[1].label, "7d");
        assert!(parse_codex_headers(&reqwest::header::HeaderMap::new()).is_none());
    }

    #[test]
    fn recognizes_quota_errors() {
        assert!(is_quota_error(
            "HTTP 429: slow down. Rate limited — wait a moment and retry."
        ));
        assert!(is_quota_error("Claude AI usage limit reached|1760000000"));
        assert!(!is_quota_error("HTTP 404: model not found"));
        assert!(!is_quota_error("Could not read credits configuration"));
        assert!(!is_quota_error("Invalid quota settings"));
    }
}
