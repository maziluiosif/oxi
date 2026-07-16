//! Shared HTTP resilience for the streaming provider loops: retryable-status
//! classification, exponential backoff, and a cancel-aware `send` wrapper.
//!
//! Two failure classes are retried so a single hiccup does not kill a whole
//! agent run:
//! - request-level: connect errors / timeouts and 408/429/5xx responses,
//!   retried inside [`send_with_retry`] before anything streams;
//! - stream-level: a connection dropped mid-SSE, retried by the caller
//!   re-sending the current round (bounded by [`MAX_STREAM_RETRIES`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Attempts per HTTP request (first try + retries) in [`send_with_retry`].
const MAX_SEND_ATTEMPTS: u32 = 5;

/// Times a provider loop may re-send one round after its stream died mid-way.
pub(super) const MAX_STREAM_RETRIES: u32 = 3;

/// Statuses worth retrying: timeouts, rate limits, and server-side failures
/// (529 is Anthropic "overloaded").
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504 | 529)
}

/// Turn an HTTP status + body into a short, actionable error for the chat UI.
pub(super) fn format_http_error(status: reqwest::StatusCode, body: &str) -> String {
    let code = status.as_u16();
    let snippet = body.trim();
    let snippet = if snippet.len() > 280 {
        let cut = snippet
            .char_indices()
            .take_while(|(i, _)| *i < 280)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(280);
        format!("{}…", &snippet[..cut])
    } else {
        snippet.to_string()
    };
    let hint = match code {
        401 | 403 => " Check your API key / OAuth sign-in in Settings → Providers.",
        404 => " Check the base URL and model id.",
        408 => " The request timed out — try again.",
        429 => " Rate limited — wait a moment and retry.",
        500 | 502 | 503 | 504 | 529 => " Provider is temporarily unavailable — retry shortly.",
        _ => {
            let lower = snippet.to_lowercase();
            if lower.contains("context") || (lower.contains("maximum") && lower.contains("token")) {
                " Context may be too large — try /compact or start a new chat."
            } else {
                ""
            }
        }
    };
    if snippet.is_empty() {
        format!("HTTP {code}.{hint}")
    } else {
        format!("HTTP {code}: {snippet}{hint}")
    }
}

/// `Retry-After` in seconds if the server sent one (capped at 60s).
fn retry_after(res: &reqwest::Response) -> Option<Duration> {
    let secs = res
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(Duration::from_secs(secs.min(60)))
}

/// Exponential backoff with jitter: ~1s, 2s, 4s, 8s (attempt is 1-based).
pub(super) fn backoff_delay(attempt: u32) -> Duration {
    let base_ms = 1000u64.saturating_mul(1 << attempt.saturating_sub(1).min(4));
    let jitter = (rand::random::<u64>() % 250) as u64;
    Duration::from_millis(base_ms + jitter)
}

/// Sleep in short slices so cancellation stays responsive. Returns `false`
/// if cancelled while waiting.
pub(super) async fn sleep_cancellable(total: Duration, cancel: &Arc<AtomicBool>) -> bool {
    let mut remaining = total;
    let slice = Duration::from_millis(100);
    while remaining > Duration::ZERO {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        let step = remaining.min(slice);
        tokio::time::sleep(step).await;
        remaining = remaining.saturating_sub(step);
    }
    !cancel.load(Ordering::SeqCst)
}

/// Send a request, retrying transient failures (connect/timeout errors and
/// retryable statuses) with exponential backoff. Honors `Retry-After` and the
/// cancel flag. Returns the successful response, or the last error formatted
/// like the loops' previous inline handling (`"HTTP {status}: {body}"`).
pub(super) async fn send_with_retry(
    builder: reqwest::RequestBuilder,
    cancel: &Arc<AtomicBool>,
) -> Result<reqwest::Response, String> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        if cancel.load(Ordering::SeqCst) {
            return Err("Cancelled".into());
        }
        let this_try = builder
            .try_clone()
            .ok_or_else(|| "internal: request not cloneable for retry".to_string())?;
        let (err, wait) = match this_try.send().await {
            Ok(res) if res.status().is_success() => return Ok(res),
            Ok(res) if is_retryable_status(res.status()) => {
                let status = res.status();
                let wait = retry_after(&res);
                let body = res.text().await.unwrap_or_default();
                (format_http_error(status, &body), wait)
            }
            Ok(res) => {
                let status = res.status();
                let body = res.text().await.unwrap_or_default();
                return Err(format_http_error(status, &body));
            }
            Err(e) => (e.to_string(), None),
        };
        if attempt >= MAX_SEND_ATTEMPTS {
            return Err(err);
        }
        eprintln!("[oxi] request failed (attempt {attempt}/{MAX_SEND_ATTEMPTS}), retrying: {err}");
        let delay = wait.unwrap_or_else(|| backoff_delay(attempt));
        if !sleep_cancellable(delay, cancel).await {
            return Err("Cancelled".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_statuses() {
        for code in [408u16, 429, 500, 502, 503, 504, 529] {
            assert!(is_retryable_status(
                reqwest::StatusCode::from_u16(code).unwrap()
            ));
        }
    }

    #[test]
    fn non_retryable_statuses() {
        for code in [400u16, 401, 403, 404, 422] {
            assert!(!is_retryable_status(
                reqwest::StatusCode::from_u16(code).unwrap()
            ));
        }
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        assert!(backoff_delay(1) >= Duration::from_millis(1000));
        assert!(backoff_delay(1) < Duration::from_millis(1500));
        assert!(backoff_delay(3) >= Duration::from_millis(4000));
        // Attempts beyond the cap keep the max base (16s) instead of overflowing.
        assert!(backoff_delay(30) < Duration::from_secs(17));
    }

    #[test]
    fn format_http_error_adds_auth_hint() {
        let msg = format_http_error(reqwest::StatusCode::UNAUTHORIZED, "invalid_api_key");
        assert!(msg.contains("401"));
        assert!(msg.contains("API key"));
    }

    #[test]
    fn format_http_error_adds_rate_limit_hint() {
        let msg = format_http_error(reqwest::StatusCode::TOO_MANY_REQUESTS, "slow down");
        assert!(msg.contains("Rate limited"));
    }

    #[tokio::test]
    async fn sleep_cancellable_honors_cancel() {
        let cancel = Arc::new(AtomicBool::new(true));
        assert!(!sleep_cancellable(Duration::from_secs(5), &cancel).await);
    }
}
