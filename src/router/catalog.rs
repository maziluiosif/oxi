//! What the router knows about a model from its id alone: a rough quality tier and list price.
//!
//! Provider model lists don't advertise either, so this is a family-pattern table in the
//! spirit of [`crate::agent::models::context_window_for_model`]. Quality is a coarse 1–5
//! ladder used only to compare candidates; prices are USD per million tokens and only matter
//! for pay-per-use providers. OpenRouter's live `/models` prices override the table when
//! known (see [`crate::router::quota`]).

/// Coarse capability ladder: 5 = frontier, 4 = strong, 3 = capable, 2 = small, 1 = tiny.
pub type Quality = u8;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    /// USD per million uncached input tokens.
    pub input: f64,
    /// USD per million output tokens.
    pub output: f64,
}

/// Normalized id: lowercase, without a vendor prefix (`anthropic/claude-…` → `claude-…`).
fn normalize(model: &str) -> String {
    let m = model.trim().to_ascii_lowercase();
    m.rsplit('/').next().unwrap_or(&m).to_string()
}

/// `(needle, quality, (input, output) USD per million tokens)`.
type Family = (&'static str, Quality, Option<(f64, f64)>);

/// First match wins, so specific variants precede their family.
const FAMILIES: &[Family] = &[
    // Anthropic
    ("opus", 5, Some((5.0, 25.0))),
    ("sonnet", 4, Some((3.0, 15.0))),
    ("haiku", 3, Some((1.0, 5.0))),
    // OpenAI
    ("gpt-5-nano", 2, Some((0.05, 0.4))),
    ("gpt-5-mini", 3, Some((0.25, 2.0))),
    ("codex-mini", 3, Some((0.25, 2.0))),
    ("-mini", 3, Some((0.25, 2.0))),
    ("-nano", 2, Some((0.05, 0.4))),
    ("gpt-5", 5, Some((1.25, 10.0))),
    ("o3", 4, Some((2.0, 8.0))),
    ("o4", 4, Some((1.1, 4.4))),
    ("gpt-4.1", 3, Some((2.0, 8.0))),
    ("gpt-4o", 3, Some((2.5, 10.0))),
    ("gpt-oss-120b", 3, Some((0.1, 0.5))),
    ("gpt-oss", 2, Some((0.05, 0.2))),
    // Google
    ("gemini-3-pro", 5, Some((2.0, 12.0))),
    ("gemini-2.5-pro", 4, Some((1.25, 10.0))),
    ("flash-lite", 2, Some((0.1, 0.4))),
    ("flash", 3, Some((0.3, 2.5))),
    // Open-weight frontier families (typical hosted prices)
    ("kimi", 4, Some((0.6, 2.5))),
    ("glm", 4, Some((0.6, 2.2))),
    ("deepseek", 4, Some((0.3, 1.2))),
    ("qwen3-coder-480b", 4, Some((0.4, 1.6))),
    ("qwen3-coder-plus", 4, Some((0.4, 1.6))),
    ("minimax", 4, Some((0.3, 1.2))),
    ("grok-4", 5, Some((3.0, 15.0))),
    ("grok-code", 3, Some((0.2, 1.5))),
    ("grok", 4, Some((3.0, 15.0))),
    ("mistral-large", 4, Some((2.0, 6.0))),
    ("devstral", 3, Some((0.1, 0.3))),
    ("codestral", 3, Some((0.3, 0.9))),
];

/// Estimated quality of a model id.
///
/// Cursor/Codex ACP report `default`/`auto`: they pick a strong model themselves, so those
/// count as strong. Local ids without a family match fall back to the parameter count in
/// the name (`qwen2.5-coder:7b` → small).
pub fn quality(model: &str) -> Quality {
    let m = normalize(model);
    if m.is_empty() || m == "default" || m == "auto" {
        return 4;
    }
    if let Some(q) = FAMILIES
        .iter()
        .find(|(needle, _, _)| m.contains(needle))
        .map(|(_, q, _)| *q)
    {
        // A small distill keeps its family name ("deepseek-r1-distill-qwen-7b"), so let a
        // small parameter count cap the family's tier.
        return match param_billions(&m) {
            Some(b) if b < 20.0 => q.min(2),
            Some(b) if b < 60.0 => q.min(3),
            _ => q,
        };
    }
    match param_billions(&m) {
        Some(b) if b >= 200.0 => 4,
        Some(b) if b >= 25.0 => 3,
        Some(b) if b >= 6.0 => 2,
        Some(_) => 1,
        None => 3,
    }
}

/// List price for a model id, if its family is known.
pub fn price(model: &str) -> Option<Price> {
    let m = normalize(model);
    FAMILIES
        .iter()
        .find(|(needle, _, _)| m.contains(needle))
        .and_then(|(_, _, p)| *p)
        .map(|(input, output)| Price { input, output })
}

/// Parameter count in billions parsed from ids like `qwen3:14b`, `llama-3.1-70b-instruct`,
/// or `qwen3-30b-a3b` (the first `<n>b` group wins, which is the total size).
fn param_billions(m: &str) -> Option<f64> {
    let bytes = m.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'b' || i == 0 {
            continue;
        }
        // The `b` must end the token (followed by a separator or the end).
        if bytes.get(i + 1).is_some_and(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        let mut start = i;
        while start > 0 && (bytes[start - 1].is_ascii_digit() || bytes[start - 1] == b'.') {
            start -= 1;
        }
        if start == i {
            continue;
        }
        // The number must start a token too, so `gpt4b` or `v2b` don't count.
        if start > 0 && bytes[start - 1].is_ascii_alphabetic() {
            continue;
        }
        if let Ok(n) = m[start..i].parse::<f64>() {
            return Some(n);
        }
    }
    None
}

/// Estimated USD for one request.
pub fn request_cost(price: Price, input_tokens: u64, output_tokens: u64) -> f64 {
    (input_tokens as f64 * price.input + output_tokens as f64 * price.output) / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_rank_as_expected() {
        assert_eq!(quality("claude-opus-4-5"), 5);
        assert_eq!(quality("anthropic/claude-sonnet-4.5"), 4);
        assert_eq!(quality("haiku"), 3);
        assert_eq!(quality("gpt-5.2-codex"), 5);
        assert_eq!(quality("gpt-5-mini"), 3);
        assert_eq!(quality("default"), 4);
    }

    #[test]
    fn local_models_rank_by_size() {
        assert_eq!(quality("qwen2.5-coder:7b"), 2);
        assert_eq!(quality("qwen3-30b-a3b"), 3);
        assert_eq!(quality("llama3.2:1b"), 1);
        assert_eq!(quality("deepseek-r1-distill-qwen-7b"), 2);
        assert_eq!(quality("local-model"), 3);
    }

    #[test]
    fn prices_follow_family() {
        assert_eq!(
            price("claude-sonnet-4-5"),
            Some(Price {
                input: 3.0,
                output: 15.0
            })
        );
        assert!(price("qwen2.5-coder:7b").is_none());
        let cost = request_cost(price("claude-opus-4-5").unwrap(), 1_000_000, 100_000);
        assert!((cost - 7.5).abs() < 1e-9);
    }
}
