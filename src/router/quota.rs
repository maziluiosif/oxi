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
//! - **Antigravity (ACP)**: `agy --print /usage --output-format json` (what Antigravity CLI's
//!   `/usage` shows) using `agy`'s own login, opt-in.
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

impl UsageWindow {
    /// Whether this usage window applies to `model`.
    /// Scopes may list multiple `|`-separated tokens (e.g. `"claude|gpt"`). A `"gemini"`
    /// scope also applies to an unset or `"default"` model (Antigravity's default pool).
    pub fn applies_to(&self, model: &str) -> bool {
        let Some(scope) = self.model_scope.as_deref() else {
            return true;
        };
        let m = model.trim().to_ascii_lowercase();
        let is_default = m.is_empty() || m == "default";
        scope.split('|').any(|token| {
            let t = token.trim();
            !t.is_empty() && (m.contains(t) || (is_default && t == "gemini"))
        })
    }

    /// `5h 34% used (resets in 2h 10m)`; a window whose reset already passed reads as empty.
    pub fn summary(&self, now: u64) -> String {
        match self.resets_at {
            Some(t) if t <= now => format!("{} 0% used", self.label),
            Some(t) => format!(
                "{} {:.0}% used (resets in {})",
                self.label,
                self.used_pct,
                crate::router::decide::short_duration(t - now)
            ),
            None => format!("{} {:.0}% used", self.label, self.used_pct),
        }
    }
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
        let now = now_secs();
        self.windows
            .iter()
            .filter(|w| w.applies_to(model))
            // A window whose reset already passed is empty again.
            .map(|w| match w.resets_at {
                Some(t) if t <= now => 0.0,
                _ => w.used_pct,
            })
            .reduce(f64::max)
    }

    /// Whether `model` has no quota/credit left right now (0% remaining / limit reached).
    pub fn is_exhausted_for(&self, model: &str, now: u64) -> bool {
        if self.credits_left.is_some_and(|c| c <= 0.0) {
            return true;
        }
        let applicable_exhausted = self.windows.iter().any(|w| {
            w.applies_to(model)
                && w.resets_at.is_none_or(|t| t > now)
                && w.used_pct >= 99.0
        });
        if applicable_exhausted {
            return true;
        }
        if !self.limit_reached {
            return false;
        }
        // If `limit_reached` came from an exhausted window whose reset already passed,
        // treat the window as reset while waiting for the next probe.
        let had_applicable_exhausted = self
            .windows
            .iter()
            .any(|w| w.applies_to(model) && w.used_pct >= 99.0);
        if had_applicable_exhausted {
            return false;
        }
        !self.windows.iter().any(|w| w.used_pct >= 99.0)
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

#[cfg(test)]
pub fn clear_snapshot_for_tests(kind: LlmProviderKind) {
    with_board(|b| {
        b.snapshots.remove(&kind);
    });
}

pub fn snapshot(kind: LlmProviderKind) -> Option<QuotaSnapshot> {
    with_board(|b| b.snapshots.get(&kind).cloned())
}

/// Is `kind` (with `model`) known to be out of quota/credit right now (cooling down after a
/// quota error, flagged `limit_reached`, or at >= 99% of an active usage window)?
pub fn is_exhausted(kind: LlmProviderKind, model: &str) -> bool {
    cooldown_left(kind).is_some()
        || snapshot(kind).is_some_and(|s| s.is_exhausted_for(model, now_secs()))
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
            let r = match probe_claude_cli().await {
                Ok(snap) => Ok(snap),
                Err(cli_err) => match claude_code_token().await {
                    Ok(token) => probe_claude(&client, &token).await,
                    Err(_) => Err(cli_err),
                },
            };
            store(LlmProviderKind::ClaudeCodeAcp, r);
        }
    };
    let antigravity = async {
        if has(LlmProviderKind::AntigravityAcp) && settings.router.read_antigravity_usage {
            store(
                LlmProviderKind::AntigravityAcp,
                probe_antigravity_cli().await,
            );
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
    futures_util::join!(codex_oxi, codex_cli, claude, antigravity, openrouter);
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

/// Ask the `claude` CLI for its `/usage` data (the `get_usage` control request). Claude Code
/// reads its own login, so oxi never touches Claude Code's keychain item (and its macOS
/// password prompt). No prompt is sent, so it costs no tokens; hooks, MCP servers and session
/// files are skipped.
async fn probe_claude_cli() -> Result<QuotaSnapshot, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut cmd = tokio::process::Command::new("claude");
    crate::shell_env::apply_async(&mut cmd).await;
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--no-session-persistence",
        "--strict-mcp-config",
        "--setting-sources",
        "",
    ])
    .current_dir(std::env::temp_dir())
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null())
    .kill_on_drop(true);
    // No console on a Windows GUI process: without CREATE_NO_WINDOW every background quota
    // refresh (every few minutes) flashes a cmd window.
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start the claude CLI: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("claude CLI: no stdin")?;
    let stdout = child.stdout.take().ok_or("claude CLI: no stdout")?;
    stdin
        .write_all(
            concat!(
                r#"{"type":"control_request","request_id":"oxi-init","request":{"subtype":"initialize"}}"#,
                "\n",
                r#"{"type":"control_request","request_id":"oxi-usage","request":{"subtype":"get_usage"}}"#,
                "\n"
            )
            .as_bytes(),
        )
        .await
        .map_err(|e| e.to_string())?;
    let read = async {
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let r = &v["response"];
            if v["type"] != "control_response" || r["request_id"] != "oxi-usage" {
                continue;
            }
            if r["subtype"] != "success" {
                return Err(r["error"]
                    .as_str()
                    .unwrap_or("claude CLI: usage request failed")
                    .to_string());
            }
            let usage = &r["response"];
            if usage["rate_limits_available"] != true || usage["rate_limits"].is_null() {
                return Err("Claude Code is not signed in with a Claude subscription".into());
            }
            return Ok(parse_claude_usage(&usage["rate_limits"]));
        }
        Err("claude CLI exited without usage data".to_string())
    };
    let result = tokio::time::timeout(Duration::from_secs(20), read)
        .await
        .unwrap_or_else(|_| Err("claude CLI: usage request timed out".into()));
    drop(stdin);
    let _ = child.kill().await;
    let mut snap = result?;
    snap.source = "Claude usage (Claude Code CLI)".into();
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

/// Ask the `agy` CLI for its `/usage` quota summary (`agy --print /usage --output-format json`).
/// Antigravity reads its own login, so oxi never touches Google credentials directly, and `/usage`
/// runs without sending a model prompt.
async fn probe_antigravity_cli() -> Result<QuotaSnapshot, String> {
    let mut cmd = tokio::process::Command::new("agy");
    crate::shell_env::apply_async(&mut cmd).await;
    cmd.args(["--print", "/usage", "--output-format", "json"])
        .current_dir(std::env::temp_dir())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let out = tokio::time::timeout(Duration::from_secs(20), cmd.output())
        .await
        .map_err(|_| "agy CLI: /usage request timed out".to_string())?
        .map_err(|e| format!("could not start the agy CLI: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    match parse_antigravity_usage_output(&stdout) {
        Ok(mut snap) => {
            snap.source = "Antigravity usage (agy CLI)".into();
            Ok(snap)
        }
        Err(e) => {
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let msg = stderr
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .trim();
                if !msg.is_empty() {
                    return Err(format!("agy /usage failed: {msg}"));
                }
            }
            Err(e)
        }
    }
}

pub(crate) fn parse_antigravity_usage_output(stdout: &str) -> Result<QuotaSnapshot, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err("agy CLI returned no usage data".into());
    }
    let json = serde_json::from_str::<Value>(trimmed).ok().or_else(|| {
        trimmed
            .lines()
            .rev()
            .find_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
    });
    if let Some(v) = json {
        return parse_antigravity_usage(&v);
    }
    parse_antigravity_usage_text(trimmed)
}

/// Lenient parser for `agy --print /usage --output-format json`. Handles:
/// - `{ command: { status, data: { groups: [{ family, buckets: [{ id, window, remaining_fraction,
///   reset_time, reset_in_seconds }] }] } } }` (or `data.groups` / `quota_summary.groups` / `groups`),
/// - `{ models: [{ name, remaining, resets_in }] }`,
/// - `{ response: "<rendered /usage text>" }` fallback.
pub(crate) fn parse_antigravity_usage(v: &Value) -> Result<QuotaSnapshot, String> {
    let now = now_secs();
    for status_node in [v.get("status"), v.pointer("/command/status")]
        .into_iter()
        .flatten()
    {
        if let Some(s) = status_node.as_str()
            && !s.is_empty()
            && !s.eq_ignore_ascii_case("success")
            && !s.eq_ignore_ascii_case("ok")
        {
            let detail = v["error"]
                .as_str()
                .or_else(|| v.pointer("/command/error").and_then(Value::as_str))
                .or_else(|| v["message"].as_str())
                .unwrap_or(s);
            return Err(format!("agy /usage failed: {detail}"));
        }
    }
    let plan = v
        .pointer("/command/data/plan")
        .or_else(|| v.pointer("/command/data/plan_tier"))
        .or_else(|| v.pointer("/command/data/tier"))
        .or_else(|| v.pointer("/data/plan"))
        .or_else(|| v.pointer("/data/plan_tier"))
        .or_else(|| v.get("plan"))
        .or_else(|| v.get("plan_tier"))
        .or_else(|| v.get("tier"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let mut windows = Vec::new();
    let groups = v
        .pointer("/command/data/groups")
        .or_else(|| v.pointer("/data/groups"))
        .or_else(|| v.pointer("/quota_summary/groups"))
        .or_else(|| v.pointer("/quotaSummary/groups"))
        .or_else(|| v.get("groups"))
        .and_then(Value::as_array);
    if let Some(groups) = groups {
        let multi_group = groups.len() > 1;
        for group in groups {
            let family = group["family"]
                .as_str()
                .or_else(|| group["name"].as_str())
                .or_else(|| group["label"].as_str())
                .unwrap_or("")
                .trim();
            let (prefix, scope) = antigravity_family_scope(family, multi_group);
            let buckets = group["buckets"]
                .as_array()
                .or_else(|| group["windows"].as_array());
            for bucket in buckets.into_iter().flatten() {
                let Some(used_pct) = bucket_used_pct(bucket) else {
                    continue;
                };
                let raw_win = bucket["window"]
                    .as_str()
                    .or_else(|| bucket["label"].as_str())
                    .or_else(|| bucket["id"].as_str())
                    .unwrap_or("");
                let win = normalize_antigravity_window(raw_win);
                let label = match (prefix.is_empty(), win.is_empty()) {
                    (true, true) => "quota".to_string(),
                    (true, false) => win,
                    (false, true) => prefix.clone(),
                    (false, false) => format!("{prefix} {win}"),
                };
                let resets_at = parse_reset_timestamp(bucket, now);
                windows.push(UsageWindow {
                    label,
                    used_pct,
                    resets_at,
                    model_scope: scope.clone(),
                });
            }
        }
    }

    if windows.is_empty()
        && let Some(models) = v
            .pointer("/command/data/models")
            .or_else(|| v.pointer("/data/models"))
            .or_else(|| v.get("models"))
            .and_then(Value::as_array)
    {
        for model in models {
            let Some(used_pct) = bucket_used_pct(model) else {
                continue;
            };
            let name = model["name"]
                .as_str()
                .or_else(|| model["display_name"].as_str())
                .or_else(|| model["id"].as_str())
                .unwrap_or("")
                .trim();
            let (prefix, scope) = antigravity_family_scope(name, true);
            let label = if prefix.is_empty() {
                if name.is_empty() {
                    "quota".to_string()
                } else {
                    name.to_string()
                }
            } else {
                prefix
            };
            let resets_at = parse_reset_timestamp(model, now);
            // Models in the same billing pool share one meter; merge identical labels keeping
            // the fullest reading and earliest reset.
            if let Some(existing) = windows
                .iter_mut()
                .find(|w| w.label == label && w.model_scope == scope)
            {
                existing.used_pct = existing.used_pct.max(used_pct);
                existing.resets_at = match (existing.resets_at, resets_at) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
            } else {
                windows.push(UsageWindow {
                    label,
                    used_pct,
                    resets_at,
                    model_scope: scope,
                });
            }
        }
    }

    if windows.is_empty()
        && let Some(response) = v["response"]
            .as_str()
            .or_else(|| v["result"].as_str())
            .filter(|s| !s.trim().is_empty())
    {
        let mut snap = parse_antigravity_usage_text(response)?;
        if snap.plan.is_none() {
            snap.plan = plan;
        }
        return Ok(snap);
    }

    if windows.is_empty() {
        return Err("Antigravity CLI returned no quota windows".into());
    }
    let limit_reached = antigravity_limit_reached(&windows);
    Ok(QuotaSnapshot {
        windows,
        plan,
        limit_reached,
        updated_at: now,
        ..Default::default()
    })
}

/// Fallback parser when `/usage` arrives as rendered text (e.g. `Gemini 3 Pro: 85% remaining (resets in 4h 30m)`).
fn parse_antigravity_usage_text(text: &str) -> Result<QuotaSnapshot, String> {
    let now = now_secs();
    let clean = strip_ansi(text);
    let mut windows = Vec::new();
    for line in clean.lines() {
        let line = line.trim();
        let Some(pct_idx) = line.find('%') else {
            continue;
        };
        let before = line[..pct_idx].trim_end();
        let num_start = before
            .rfind(|c: char| !(c.is_ascii_digit() || c == '.'))
            .map(|i| i + 1)
            .unwrap_or(0);
        let Ok(pct) = before[num_start..].parse::<f64>() else {
            continue;
        };
        let lower = line.to_ascii_lowercase();
        let used_pct = if lower.contains("remain") || lower.contains("left") {
            (100.0 - pct).clamp(0.0, 100.0)
        } else {
            pct.clamp(0.0, 100.0)
        };
        let head = before[..num_start]
            .trim()
            .trim_end_matches([':', '-', '·', '|'])
            .trim();
        let (prefix, scope) = antigravity_family_scope(head, true);
        let win = normalize_antigravity_window(head);
        let label = match (prefix.is_empty(), win.is_empty()) {
            (true, true) => {
                if head.is_empty() {
                    "quota".to_string()
                } else {
                    head.to_string()
                }
            }
            (true, false) => win,
            (false, true) => prefix,
            (false, false) => format!("{prefix} {win}"),
        };
        let resets_at = extract_reset_from_text(&lower, now);
        if let Some(existing) = windows
            .iter_mut()
            .find(|w: &&mut UsageWindow| w.label == label && w.model_scope == scope)
        {
            existing.used_pct = existing.used_pct.max(used_pct);
            existing.resets_at = existing.resets_at.or(resets_at);
        } else {
            windows.push(UsageWindow {
                label,
                used_pct,
                resets_at,
                model_scope: scope,
            });
        }
    }
    if windows.is_empty() {
        return Err("could not parse Antigravity /usage output".into());
    }
    let limit_reached = antigravity_limit_reached(&windows);
    Ok(QuotaSnapshot {
        windows,
        limit_reached,
        updated_at: now,
        ..Default::default()
    })
}

/// For multi-pool Antigravity snapshots, the provider as a whole is only at its limit when
/// an unscoped window hits 100%, or when every scoped pool has an exhausted window.
fn antigravity_limit_reached(windows: &[UsageWindow]) -> bool {
    if windows.is_empty() {
        return false;
    }
    if windows
        .iter()
        .any(|w| w.model_scope.is_none() && w.used_pct >= 100.0)
    {
        return true;
    }
    if windows.iter().any(|w| w.model_scope.is_none()) {
        return false;
    }
    let mut scopes: Vec<&str> = Vec::new();
    for w in windows {
        if let Some(s) = w.model_scope.as_deref()
            && !scopes.contains(&s)
        {
            scopes.push(s);
        }
    }
    !scopes.is_empty()
        && scopes.iter().all(|&scope| {
            windows
                .iter()
                .any(|w| w.model_scope.as_deref() == Some(scope) && w.used_pct >= 100.0)
        })
}

/// Short status-bar prefix + router `model_scope` for an Antigravity quota group.
/// In multi-group snapshots, each pool carries its own `model_scope` (`gemini`, `claude|gpt`,
/// `argon`) so exhausting Gemini does not block Claude/GPT models; `UsageWindow::applies_to`
/// still matches `"default"` / empty models to the `"gemini"` pool.
fn antigravity_family_scope(family: &str, multi_group: bool) -> (String, Option<String>) {
    let lower = family.to_ascii_lowercase();
    if lower.contains("claude") || lower.contains("gpt") || lower.contains("third") {
        let (label, scope) = if (lower.contains("claude") && lower.contains("gpt"))
            || lower.contains("third")
        {
            ("Claude/GPT", "claude|gpt")
        } else if lower.contains("claude") {
            ("Claude", "claude")
        } else {
            ("GPT", "gpt")
        };
        return (label.into(), Some(scope.into()));
    }
    if lower.contains("argon") {
        return ("Argon".into(), Some("argon".into()));
    }
    if lower.contains("gemini") {
        if multi_group {
            return ("Gemini".into(), Some("gemini".into()));
        }
        return (String::new(), None);
    }
    (String::new(), None)
}

fn normalize_antigravity_window(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("5h") || lower.contains("5 hour") {
        "5h".into()
    } else if lower.contains("7d") || lower.contains("7 day") || lower.contains("week") {
        "7d".into()
    } else if lower.contains("24h") || lower.contains("1 day") || lower.contains("daily") {
        "24h".into()
    } else if lower.contains("hour") || lower.contains("day") || lower.contains("min") {
        raw.trim().to_string()
    } else {
        String::new()
    }
}

fn bucket_used_pct(v: &Value) -> Option<f64> {
    if let Some(rem) = as_f64(&v["remaining_fraction"]).or_else(|| as_f64(&v["remainingFraction"]))
    {
        return Some(((1.0 - rem) * 100.0).clamp(0.0, 100.0));
    }
    if let Some(used) = as_f64(&v["used_fraction"]).or_else(|| as_f64(&v["usedFraction"])) {
        return Some((used * 100.0).clamp(0.0, 100.0));
    }
    if let Some(used) = as_f64(&v["used_percent"])
        .or_else(|| as_f64(&v["usedPercent"]))
        .or_else(|| as_f64(&v["utilization"]))
    {
        return Some(used.clamp(0.0, 100.0));
    }
    if let Some(rem) = as_percent(&v["remaining_percent"])
        .or_else(|| as_percent(&v["remainingPercent"]))
        .or_else(|| as_percent(&v["remaining"]))
    {
        return Some((100.0 - rem).clamp(0.0, 100.0));
    }
    None
}

fn as_percent(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| {
        v.as_str()
            .map(str::trim)
            .and_then(|s| s.strip_suffix('%').unwrap_or(s).trim().parse::<f64>().ok())
    })
}

fn parse_reset_timestamp(v: &Value, now: u64) -> Option<u64> {
    for key in ["reset_time", "resetTime", "resets_at", "resetsAt"] {
        let node = &v[key];
        if let Some(s) = node.as_str()
            && let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s)
        {
            return Some(dt.timestamp().max(0) as u64);
        }
        if let Some(ts) = as_f64(node) {
            return Some(ts.max(0.0) as u64);
        }
    }
    for key in [
        "reset_in_seconds",
        "resetInSeconds",
        "reset_after_seconds",
        "resetAfterSeconds",
    ] {
        if let Some(secs) = as_f64(&v[key]) {
            return Some(now + secs.max(0.0) as u64);
        }
    }
    for key in ["resets_in", "resetsIn"] {
        if let Some(s) = v[key].as_str()
            && let Some(secs) = parse_duration_secs(s)
        {
            return Some(now + secs);
        }
    }
    None
}

fn extract_reset_from_text(lower: &str, now: u64) -> Option<u64> {
    let idx = lower.find("reset")?;
    parse_duration_secs(&lower[idx..]).map(|secs| now + secs)
}

/// Parse compact human durations like `"4h 59m"`, `"2d 3h"`, `"105h 49m"`, `"45s"`.
fn parse_duration_secs(s: &str) -> Option<u64> {
    let mut total: u64 = 0;
    let mut found = false;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let Ok(n) = s[start..i].parse::<u64>() else {
            continue;
        };
        let unit_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let mult = match &s[unit_start..i] {
            "d" | "day" | "days" => 86_400,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            _ => continue,
        };
        total = total.saturating_add(n.saturating_mul(mult));
        found = true;
    }
    found.then_some(total)
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
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
    // Disabled: reading Claude Code's keychain item showed a macOS password prompt.
    #[cfg(any())]
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

    /// Runs the real `claude` CLI (needs a Claude subscription login), so it's ignored by
    /// default: `cargo test -- --ignored claude_cli_reports_usage`.
    #[tokio::test]
    #[ignore]
    async fn claude_cli_reports_usage() {
        let snap = probe_claude_cli().await.unwrap();
        assert!(snap.windows.iter().any(|w| w.label == "5h"), "{snap:?}");
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

    #[test]
    fn parses_antigravity_groups_payload() {
        let v = json!({
            "command": {
                "status": "SUCCESS",
                "data": {
                    "plan_tier": "pro",
                    "groups": [
                        {
                            "family": "Gemini Models",
                            "buckets": [
                                {
                                    "id": "gemini-5h",
                                    "window": "5 hours",
                                    "remaining_fraction": 0.75,
                                    "reset_time": "2099-01-01T00:00:00Z"
                                },
                                {
                                    "id": "gemini-weekly",
                                    "window": "7 days",
                                    "remaining_fraction": 0.60,
                                    "reset_in_seconds": 3600
                                }
                            ]
                        },
                        {
                            "family": "Claude and GPT models",
                            "buckets": [
                                {
                                    "id": "claude-weekly",
                                    "window": "7 days",
                                    "remaining_fraction": 0.20,
                                    "reset_time": "2099-01-03T00:00:00Z"
                                }
                            ]
                        }
                    ]
                }
            }
        });
        let s = parse_antigravity_usage(&v).unwrap();
        assert_eq!(s.plan.as_deref(), Some("pro"));
        assert_eq!(s.windows.len(), 3);
        assert_eq!(s.windows[0].label, "Gemini 5h");
        assert!((s.windows[0].used_pct - 25.0).abs() < 1e-6);
        assert_eq!(s.windows[0].model_scope.as_deref(), Some("gemini"));
        assert_eq!(s.windows[1].label, "Gemini 7d");
        assert!((s.windows[1].used_pct - 40.0).abs() < 1e-6);
        assert_eq!(s.windows[2].label, "Claude/GPT 7d");
        assert!((s.windows[2].used_pct - 80.0).abs() < 1e-6);
        assert_eq!(s.windows[2].model_scope.as_deref(), Some("claude|gpt"));
        assert!(!s.limit_reached);
        assert!((s.used_pct_for("gemini-4-neon").unwrap() - 40.0).abs() < 1e-6);
        assert!((s.used_pct_for("claude-sonnet-4-6").unwrap() - 80.0).abs() < 1e-6);
        assert!((s.used_pct_for("gpt-5").unwrap() - 80.0).abs() < 1e-6);
        assert!((s.used_pct_for("default").unwrap() - 40.0).abs() < 1e-6);
    }

    #[test]
    fn antigravity_dual_pools_isolate_exhaustion() {
        let v = json!({
            "command": {
                "status": "SUCCESS",
                "data": {
                    "plan_tier": "pro",
                    "groups": [
                        {
                            "family": "Gemini Models",
                            "buckets": [
                                {
                                    "id": "gemini-5h",
                                    "window": "5 hours",
                                    "remaining_fraction": 0.0,
                                    "reset_time": "2099-01-01T00:00:00Z"
                                },
                                {
                                    "id": "gemini-weekly",
                                    "window": "7 days",
                                    "remaining_fraction": 0.50,
                                    "reset_time": "2099-01-03T00:00:00Z"
                                }
                            ]
                        },
                        {
                            "family": "Claude and GPT models",
                            "buckets": [
                                {
                                    "id": "claude-5h",
                                    "window": "5 hours",
                                    "remaining_fraction": 1.0,
                                    "reset_time": "2099-01-01T00:00:00Z"
                                },
                                {
                                    "id": "claude-weekly",
                                    "window": "7 days",
                                    "remaining_fraction": 1.0,
                                    "reset_time": "2099-01-03T00:00:00Z"
                                }
                            ]
                        }
                    ]
                }
            }
        });
        let now = 1_000_000;
        let s = parse_antigravity_usage(&v).unwrap();
        assert!(!s.limit_reached);
        assert!(s.is_exhausted_for("gemini-4-neon", now));
        assert!(s.is_exhausted_for("default", now));
        assert!(s.is_exhausted_for("", now));
        assert!(!s.is_exhausted_for("claude-sonnet-4-6", now));
        assert!(!s.is_exhausted_for("gpt-5", now));
        assert!((s.used_pct_for("claude-sonnet-4-6").unwrap() - 0.0).abs() < 1e-6);
        assert!((s.used_pct_for("gpt-5").unwrap() - 0.0).abs() < 1e-6);

        // When every pool is at 100%, the provider as a whole is limit_reached.
        let both_full = json!({
            "command": {
                "status": "SUCCESS",
                "data": {
                    "groups": [
                        {
                            "family": "Gemini Models",
                            "buckets": [{
                                "window": "5 hours",
                                "remaining_fraction": 0.0,
                                "reset_time": "2099-01-01T00:00:00Z"
                            }]
                        },
                        {
                            "family": "Claude and GPT models",
                            "buckets": [{
                                "window": "5 hours",
                                "remaining_fraction": 0.0,
                                "reset_time": "2099-01-01T00:00:00Z"
                            }]
                        }
                    ]
                }
            }
        });
        let s = parse_antigravity_usage(&both_full).unwrap();
        assert!(s.limit_reached);
        assert!(s.is_exhausted_for("gemini-4-neon", now));
        assert!(s.is_exhausted_for("claude-sonnet-4-6", now));
        assert!(s.is_exhausted_for("gpt-5", now));
    }

    #[test]
    fn parses_antigravity_models_and_text_fallbacks() {
        let models = json!({
            "status": "success",
            "models": [
                {"name": "Claude Sonnet 4.5", "remaining": "N/A", "resets_in": "105h 49m"},
                {"name": "Gemini 3 Flash", "remaining": "90%", "resets_in": "4h 59m"},
                {"name": "Gemini 3 Pro (High)", "remaining": "65%", "resets_in": "2h 10m"}
            ]
        });
        let s = parse_antigravity_usage(&models).unwrap();
        assert_eq!(s.windows.len(), 1);
        assert_eq!(s.windows[0].label, "Gemini");
        assert!((s.windows[0].used_pct - 35.0).abs() < 1e-6);
        assert!(s.windows[0].resets_at.is_some());

        let text_wrapped = json!({
            "status": "SUCCESS",
            "response": "\x1b[1mGemini 5h:\x1b[0m 80% remaining (resets in 3h 15m)\nClaude 7d: 50% used (resets in 2d 1h)"
        });
        let s = parse_antigravity_usage(&text_wrapped).unwrap();
        assert_eq!(s.windows.len(), 2);
        assert!((s.windows[0].used_pct - 20.0).abs() < 1e-6);
        assert!((s.windows[1].used_pct - 50.0).abs() < 1e-6);

        let err = json!({"status": "ERROR", "error": "not signed in"});
        assert!(parse_antigravity_usage(&err).is_err());
    }

    #[test]
    fn exhausted_checks_active_windows_scopes_resets_and_credits() {
        let now = 1_000_000;
        let full = QuotaSnapshot {
            windows: vec![UsageWindow {
                label: "5h".into(),
                used_pct: 100.0,
                resets_at: Some(now + 600),
                model_scope: None,
            }],
            limit_reached: true,
            ..Default::default()
        };
        assert!(full.is_exhausted_for("opus", now));
        // Once the exhausted window's reset passes, it is no longer exhausted.
        assert!(!full.is_exhausted_for("opus", now + 600));

        // Scoped window exhausts only matching models.
        let opus_full = QuotaSnapshot {
            windows: vec![
                UsageWindow {
                    label: "5h".into(),
                    used_pct: 20.0,
                    resets_at: Some(now + 600),
                    model_scope: None,
                },
                UsageWindow {
                    label: "7d Opus".into(),
                    used_pct: 100.0,
                    resets_at: Some(now + 3600),
                    model_scope: Some("opus".into()),
                },
            ],
            limit_reached: false,
            ..Default::default()
        };
        assert!(opus_full.is_exhausted_for("claude-opus", now));
        assert!(!opus_full.is_exhausted_for("claude-sonnet", now));

        // Zero credits left also counts as exhausted.
        let no_credits = QuotaSnapshot {
            credits_left: Some(0.0),
            ..Default::default()
        };
        assert!(no_credits.is_exhausted_for("any", now));
    }

    /// Runs the real `agy` CLI (needs an Antigravity login), so it's ignored by default:
    /// `cargo test -- --ignored antigravity_cli_reports_usage`.
    #[tokio::test]
    #[ignore]
    async fn antigravity_cli_reports_usage() {
        let snap = probe_antigravity_cli().await.unwrap();
        assert!(!snap.windows.is_empty(), "{snap:?}");
    }
}
