//! Heuristic task classification: how hard is the next turn, and how much effort does it need?
//!
//! Deliberately cheap and explainable: every signal that moved the decision is returned so the
//! chat can show *why* a model was chosen. Keywords cover English and Romanian.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Light,
    Standard,
    Heavy,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Light => "light",
            Tier::Standard => "standard",
            Tier::Heavy => "heavy",
        }
    }

    /// Minimum [`super::catalog::Quality`] a model needs for this tier.
    pub fn required_quality(self) -> u8 {
        match self {
            Tier::Light => 2,
            Tier::Standard => 3,
            Tier::Heavy => 4,
        }
    }

    pub fn effort(self) -> &'static str {
        match self {
            Tier::Light => "low",
            Tier::Standard => "medium",
            Tier::Heavy => "high",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskProfile {
    pub tier: Tier,
    /// Prompt size the next request will carry (history + new message), in tokens.
    pub est_input_tokens: u64,
    /// Expected output for the whole turn, in tokens.
    pub est_output_tokens: u64,
    pub has_images: bool,
    /// Human-readable reasons, most important first.
    pub signals: Vec<String>,
}

pub struct ClassifyInput<'a> {
    pub text: &'a str,
    pub has_images: bool,
    pub plan_mode: bool,
    /// Characters of earlier conversation that will be resent.
    pub history_chars: usize,
    pub chars_per_token: f32,
    /// Tool calls in the previous assistant turn.
    pub prior_tool_calls: usize,
    /// Tier the router picked for the previous turn of this chat.
    pub previous_tier: Option<Tier>,
}

const HEAVY_WORDS: &[&str] = &[
    "refactor",
    "architect",
    "implement",
    "migrat",
    "rewrite",
    "redesign",
    "debug",
    "race condition",
    "deadlock",
    "memory leak",
    "performance",
    "optimiz",
    "security",
    "vulnerab",
    "investigat",
    "root cause",
    "end-to-end",
    "from scratch",
    "whole codebase",
    "entire",
    // Romanian
    "implementeaz",
    "refactoriz",
    "arhitectur",
    "rescrie",
    "investigheaz",
    "optimizeaz",
    "performan",
    "securitat",
    "de la zero",
    "tot proiectul",
    "toate fișierele",
    "toate fisierele",
];

const STANDARD_WORDS: &[&str] = &[
    "fix", "bug", "add ", "test", "feature", "update", "change", "error", "repar", "adaug",
    "modific", "schimb", "eroare", "funcți", "functi",
];

const LIGHT_WORDS: &[&str] = &[
    "rename",
    "typo",
    "explain",
    "what is",
    "what's",
    "what does",
    "how do i",
    "translate",
    "summar",
    "commit message",
    "format",
    "thanks",
    "thank you",
    // Romanian
    "redenume",
    "explică",
    "explica",
    "ce este",
    "ce e ",
    "ce face",
    "cum fac",
    "tradu",
    "rezum",
    "mulțum",
    "multum",
    "mersi",
];

const CONTINUE_WORDS: &[&str] = &[
    "continue",
    "go on",
    "go ahead",
    "keep going",
    "proceed",
    "yes",
    "ok",
    "do it",
    "continuă",
    "continua",
    "mergi mai departe",
    "da",
    "fă-o",
    "fa-o",
    "fă ",
    "fa ",
    "hai",
];

pub fn classify(input: &ClassifyInput<'_>) -> TaskProfile {
    let text = input.text.trim();
    let lower = text.to_lowercase();
    let chars = text.chars().count();
    let cpt = input.chars_per_token.max(1.0);
    let est_input_tokens = ((input.history_chars + text.len()) as f32 / cpt) as u64 + 4_000;

    let mut signals: Vec<String> = Vec::new();
    let mut heavy = 0i32;

    // A short "continue / yes, do it" keeps the previous turn's difficulty: the work is the
    // same task, only the prompt is short.
    let is_continuation = chars <= 40
        && CONTINUE_WORDS.iter().any(|w| {
            lower
                .strip_prefix(w.trim_end())
                .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric()))
        });
    if is_continuation && let Some(prev) = input.previous_tier {
        return TaskProfile {
            tier: prev,
            est_input_tokens,
            est_output_tokens: output_estimate(prev),
            has_images: input.has_images,
            signals: vec![format!("continues the previous {} task", prev.label())],
        };
    }

    let heavy_hits: Vec<&str> = HEAVY_WORDS
        .iter()
        .copied()
        .filter(|w| lower.contains(w))
        .collect();
    if !heavy_hits.is_empty() {
        heavy += 2 + (heavy_hits.len() as i32 - 1).min(2);
        signals.push(format!(
            "asks to {}",
            heavy_hits
                .iter()
                .take(3)
                .map(|w| w.trim())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if input.plan_mode {
        heavy += 2;
        signals.push("plan mode".into());
    }
    if chars > 1_500 {
        heavy += 2;
        signals.push(format!("long prompt ({chars} chars)"));
    } else if chars > 500 {
        heavy += 1;
        signals.push(format!("detailed prompt ({chars} chars)"));
    }
    let has_code = text.contains("```")
        || lower.contains("panicked at")
        || lower.contains("traceback")
        || lower.contains("error[e")
        || lower.contains("exception");
    if has_code {
        heavy += 1;
        signals.push("includes code / a stack trace".into());
    }
    if input.prior_tool_calls >= 8 {
        heavy += 1;
        signals.push(format!(
            "previous turn needed {} tool calls",
            input.prior_tool_calls
        ));
    }
    if est_input_tokens > 80_000 {
        heavy += 1;
        signals.push(format!(
            "large context (~{}k tokens)",
            est_input_tokens / 1000
        ));
    }
    let numbered_steps = text
        .lines()
        .filter(|l| {
            let l = l.trim_start();
            l.starts_with("- ")
                || l.starts_with("* ")
                || l.split_once('.')
                    .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })
        .count();
    if numbered_steps >= 3 {
        heavy += 1;
        signals.push(format!("{numbered_steps} listed requirements"));
    }

    let light_hit = LIGHT_WORDS.iter().find(|w| lower.contains(*w));
    let standard_hit = STANDARD_WORDS.iter().any(|w| lower.contains(w));
    let tier = if heavy >= 3 {
        Tier::Heavy
    } else if heavy == 0 && chars <= 200 && !standard_hit && (light_hit.is_some() || chars <= 60) {
        match light_hit {
            Some(w) => signals.push(format!("simple request (\"{}\")", w.trim())),
            None => signals.push("short message".into()),
        }
        Tier::Light
    } else {
        if signals.is_empty() {
            signals.push("regular coding request".into());
        }
        Tier::Standard
    };

    TaskProfile {
        tier,
        est_input_tokens,
        est_output_tokens: output_estimate(tier),
        has_images: input.has_images,
        signals,
    }
}

/// Rough output for a whole turn including tool-call arguments.
pub(super) fn output_estimate(tier: Tier) -> u64 {
    match tier {
        Tier::Light => 800,
        Tier::Standard => 4_000,
        Tier::Heavy => 12_000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> TaskProfile {
        classify(&ClassifyInput {
            text,
            has_images: false,
            plan_mode: false,
            history_chars: 0,
            chars_per_token: 4.0,
            prior_tool_calls: 0,
            previous_tier: None,
        })
    }

    #[test]
    fn tiers_follow_the_request() {
        assert_eq!(run("ce este un trait?").tier, Tier::Light);
        assert_eq!(run("thanks!").tier, Tier::Light);
        assert_eq!(
            run("fix the failing test in parser.rs").tier,
            Tier::Standard
        );
        assert_eq!(
            run("refactor the session store and implement a migration for the old format").tier,
            Tier::Heavy
        );
        assert_eq!(
            run("hai sa implementam toate astea, refactorizeaza modulul de provideri").tier,
            Tier::Heavy
        );
    }

    #[test]
    fn continuation_keeps_previous_tier() {
        let p = classify(&ClassifyInput {
            text: "da, continuă",
            has_images: false,
            plan_mode: false,
            history_chars: 0,
            chars_per_token: 4.0,
            prior_tool_calls: 0,
            previous_tier: Some(Tier::Heavy),
        });
        assert_eq!(p.tier, Tier::Heavy);
    }

    #[test]
    fn plan_mode_and_long_prompt_are_heavy() {
        let p = classify(&ClassifyInput {
            text: &"x ".repeat(900),
            has_images: false,
            plan_mode: true,
            history_chars: 0,
            chars_per_token: 4.0,
            prior_tool_calls: 0,
            previous_tier: None,
        });
        assert_eq!(p.tier, Tier::Heavy);
        assert!(p.signals.iter().any(|s| s == "plan mode"));
    }
}
