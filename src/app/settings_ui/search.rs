//! Settings search: a small hand-kept index of every setting, so "wrap", "sleep" or "token"
//! finds the page and card it lives on without the user knowing where oxi filed it.

use crate::settings::LlmProviderKind;

use super::super::state::SettingsTab;

/// Where a search hit leads.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsTarget {
    /// A page, scrolled to the card (or caption) titled `card` when there is one.
    Page {
        tab: SettingsTab,
        card: Option<&'static str>,
    },
    /// The Models & providers page showing `kind` (`None`: the provider form already open,
    /// see `go_to_settings_target`), scrolled to `card` when there is one.
    Provider {
        kind: Option<LlmProviderKind>,
        card: Option<&'static str>,
    },
}

pub(super) struct SettingsHit {
    pub label: &'static str,
    /// "Page › Card" breadcrumb shown under the label.
    pub page: &'static str,
    pub card: Option<&'static str>,
    pub target: SettingsTarget,
}

/// (page, section title as painted by `settings_card_header` — or a folded group's or a row's
/// title, which a search hit also reveals — setting label,
/// extra words people search for).
type Entry = (SettingsTab, &'static str, &'static str, &'static str);

const ENTRIES: &[Entry] = &[
    // Server
    (
        SettingsTab::Server,
        "Run the oxi server",
        "Start at login and keep running",
        "daemon background service launchagent systemd remote ssh",
    ),
    (
        SettingsTab::Server,
        "Phone access",
        "Allow phones to connect",
        "android mobile pair qr tailscale token",
    ),
    // Prompts
    (
        SettingsTab::Prompts,
        "System prompt",
        "System prompt",
        "instructions persona agent",
    ),
    (
        SettingsTab::Prompts,
        "Agent system prompt",
        "Include workspace AGENTS.md",
        "agents md rules context",
    ),
    (
        SettingsTab::Prompts,
        "Agent system prompt",
        "Include .oxi/rules and .cursor/rules",
        "rules cursor context",
    ),
    (
        SettingsTab::Prompts,
        "Commit message prompt",
        "Commit message prompt",
        "git generate",
    ),
    (
        SettingsTab::Prompts,
        "Commit messages & chat titles",
        "Name new chats with this model",
        "chat title naming",
    ),
    // Tools & safety
    (
        SettingsTab::Agent,
        "Enabled tools",
        "Enabled tools",
        "read grep find ls bash edit write delete web_search web_fetch codebase_search task todo scratchpad git",
    ),
    (
        SettingsTab::Agent,
        "MCP servers",
        "MCP servers",
        "model context protocol stdio http server",
    ),
    (
        SettingsTab::Agent,
        "Approvals",
        "Ask before file changes",
        "approval permission confirm write edit",
    ),
    (
        SettingsTab::Agent,
        "Approvals",
        "Ask before commands",
        "approval permission confirm bash command shell",
    ),
    (
        SettingsTab::Agent,
        "Approvals",
        "Always allowed commands",
        "allowlist bash whitelist",
    ),
    (
        SettingsTab::Agent,
        "Limits",
        "Max tool calls per run",
        "limit loop",
    ),
    (
        SettingsTab::Agent,
        "Limits",
        "Bash timeout cap (seconds)",
        "seconds limit",
    ),
    (
        SettingsTab::Agent,
        "Limits",
        "Max ACP agents running",
        "limit external agents",
    ),
    (
        SettingsTab::Agent,
        "Limits",
        "Stop idle ACP agents after (minutes)",
        "idle reaper minutes",
    ),
    (
        SettingsTab::Agent,
        "Web search",
        "Web search backend",
        "searxng duckduckgo internet",
    ),
    // GitHub
    (
        SettingsTab::GitHub,
        "Commit identity",
        "Author name and email",
        "git user commit identity",
    ),
    (
        SettingsTab::GitHub,
        "GitHub authentication",
        "Personal access token",
        "pat token login username account",
    ),
    (
        SettingsTab::GitHub,
        "System Git",
        "Use system Git for push, pull, and fetch",
        "git executable ssh remote libgit2 native engine",
    ),
    // Voice
    (
        SettingsTab::Voice,
        "Dictation",
        "Enable dictation",
        "microphone mic voice speech",
    ),
    (
        SettingsTab::Voice,
        "Dictation",
        "Keep model loaded after use",
        "memory whisper",
    ),
    (
        SettingsTab::Voice,
        "Dictation",
        "Dictation language",
        "language auto",
    ),
    (
        SettingsTab::Voice,
        "Whisper model",
        "Whisper model",
        "speech download model",
    ),
    // Editor
    (
        SettingsTab::Editor,
        "Text",
        "Editor font size",
        "zoom text code points",
    ),
    (SettingsTab::Editor, "Layout", "Word wrap", "wrap lines"),
    (
        SettingsTab::Editor,
        "Layout",
        "Indent wrapped lines",
        "wrap indent",
    ),
    (SettingsTab::Editor, "Layout", "Minimap", "overview"),
    (
        SettingsTab::Editor,
        "Layout",
        "Scroll past end",
        "scrolling",
    ),
    (
        SettingsTab::Editor,
        "Editing",
        "Auto complete",
        "completion suggestions",
    ),
    (
        SettingsTab::Editor,
        "Editing",
        "Highlight current line",
        "caret line",
    ),
    (
        SettingsTab::Editor,
        "Editing",
        "Draw white space",
        "whitespace tabs spaces invisibles",
    ),
    (
        SettingsTab::Editor,
        "On save",
        "Trim trailing white space",
        "whitespace save",
    ),
    (
        SettingsTab::Editor,
        "On save",
        "Ensure newline at end of file",
        "eof save",
    ),
    // Terminal
    (
        SettingsTab::Editor,
        "Terminal",
        "Shell",
        "terminal zsh bash powershell wsl cmd login",
    ),
    // Appearance
    (
        SettingsTab::Appearance,
        "Appearance",
        "Theme",
        "dark light color sublime mariana midnight",
    ),
    (
        SettingsTab::Appearance,
        "Appearance",
        "Interface font",
        "typeface inter",
    ),
    (
        SettingsTab::Appearance,
        "Appearance",
        "Code font",
        "monospace typeface jetbrains",
    ),
    (
        SettingsTab::Appearance,
        "Appearance",
        "Text size",
        "zoom density scale ui",
    ),
    (
        SettingsTab::Appearance,
        "Chat",
        "Tool calls",
        "minimal balanced expanded pills",
    ),
    (
        SettingsTab::Appearance,
        "Chat",
        "Chat width",
        "column message",
    ),
    (
        SettingsTab::Appearance,
        "While agents work",
        "Notify me in the background",
        "notification desktop alert",
    ),
    (
        SettingsTab::Appearance,
        "While agents work",
        "Keep the computer awake while an agent works",
        "sleep caffeinate power",
    ),
    (
        SettingsTab::Appearance,
        "While agents work",
        "Send follow-ups into the running response",
        "steer queue follow up interrupt mid-turn",
    ),
    // About
    (
        SettingsTab::About,
        "Updates",
        "Install updates automatically",
        "update version release",
    ),
    (
        SettingsTab::About,
        "Diagnostics",
        "Diagnostics",
        "activity log logs debug",
    ),
    (
        SettingsTab::About,
        "Keyboard shortcuts",
        "Keyboard shortcuts",
        "keybindings hotkeys keys",
    ),
];

/// Settings inside the provider forms and the Router page: (provider, card, label, keywords).
/// `None` is any provider's form; those cards are shared by every provider kind.
type ProviderEntry = (
    Option<LlmProviderKind>,
    &'static str,
    &'static str,
    &'static str,
);

const PROVIDER_ENTRIES: &[ProviderEntry] = &[
    (None, "Model", "Model id", "model name"),
    (None, "Model", "Context window", "tokens context length"),
    (
        None,
        "Model",
        "Thinking / reasoning level",
        "reasoning effort thinking",
    ),
    (None, "Connection", "Base URL", "endpoint url host server"),
    (
        None,
        "Connection",
        "API key / token",
        "api key secret credentials keychain",
    ),
    (
        None,
        "Connection",
        "Accept self-signed TLS certificates",
        "tls ssl certificate insecure https",
    ),
    (
        None,
        "Connection",
        "Agent command",
        "acp npx command launch",
    ),
    (
        None,
        "Compute target",
        "Compute target",
        "ssh remote host tunnel",
    ),
    (
        Some(LlmProviderKind::Router),
        "How it decides",
        "Default routing strategy",
        "router strategy jev classification",
    ),
    (
        Some(LlmProviderKind::Router),
        "How it decides",
        "Subscription reserve",
        "router quota",
    ),
    (
        Some(LlmProviderKind::Router),
        "How it decides",
        "Monthly pay-per-use budget",
        "router budget cost spend",
    ),
    (
        Some(LlmProviderKind::Router),
        "How it decides",
        "Continue with another provider when quota is reached",
        "router failover switch",
    ),
    (
        Some(LlmProviderKind::Router),
        "How it decides",
        "Wait for quota to reset when every provider is out",
        "router wait quota reset",
    ),
    (
        Some(LlmProviderKind::Router),
        "Usage & quota",
        "Read subscription usage",
        "router claude codex usage quota limits",
    ),
    (
        Some(LlmProviderKind::Router),
        "Candidates",
        "Router candidates",
        "router providers enabled",
    ),
];

pub(super) fn tab_title(tab: SettingsTab) -> &'static str {
    match tab {
        SettingsTab::Providers => "Models",
        SettingsTab::Prompts => "Prompts",
        SettingsTab::Agent => "Agent tools",
        SettingsTab::GitHub => "Git & GitHub",
        SettingsTab::Voice => "Voice",
        SettingsTab::Editor => "Editor & terminal",
        SettingsTab::Appearance => "General",
        SettingsTab::Server => "Server & phone",
        SettingsTab::About => "About",
    }
}

/// Every hit whose words contain all of the query's words, best first: a label that starts
/// with the query beats one that merely mentions it, which beats a keyword-only match.
pub(super) fn search(query: &str) -> Vec<SettingsHit> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let matches = |haystack: &str| terms.iter().all(|t| haystack.contains(t.as_str()));

    let mut hits: Vec<(u8, SettingsHit)> = Vec::new();
    for &(tab, card, label, keywords) in ENTRIES {
        let page = tab_title(tab);
        let haystack = format!("{label} {card} {page} {keywords}").to_lowercase();
        if !matches(&haystack) {
            continue;
        }
        hits.push((
            rank(label, &terms),
            SettingsHit {
                label,
                page,
                card: (card != label).then_some(card),
                target: SettingsTarget::Page {
                    tab,
                    card: Some(card),
                },
            },
        ));
    }
    for kind in LlmProviderKind::ALL {
        let label = kind.label();
        let haystack = format!("{label} provider model {}", kind_keywords(kind)).to_lowercase();
        if matches(&haystack) {
            hits.push((
                rank(label, &terms),
                SettingsHit {
                    label,
                    page: tab_title(SettingsTab::Providers),
                    card: None,
                    target: SettingsTarget::Provider {
                        kind: Some(kind),
                        card: None,
                    },
                },
            ));
        }
    }
    for &(kind, card, label, keywords) in PROVIDER_ENTRIES {
        let owner = kind.map_or("provider", |kind| kind.label());
        let haystack = format!("{label} {card} {owner} {keywords}").to_lowercase();
        if matches(&haystack) {
            hits.push((
                rank(label, &terms),
                SettingsHit {
                    label,
                    page: kind.map_or(tab_title(SettingsTab::Providers), |kind| kind.label()),
                    card: (card != label).then_some(card),
                    target: SettingsTarget::Provider {
                        kind,
                        card: Some(card),
                    },
                },
            ));
        }
    }
    // Page names themselves, so "voice" or "about" lists the page even past its settings.
    for tab in [
        SettingsTab::Providers,
        SettingsTab::Prompts,
        SettingsTab::Agent,
        SettingsTab::GitHub,
        SettingsTab::Voice,
        SettingsTab::Editor,
        SettingsTab::Appearance,
        SettingsTab::Server,
        SettingsTab::About,
    ] {
        let page = tab_title(tab);
        if matches(&page.to_lowercase()) {
            hits.push((
                0,
                SettingsHit {
                    label: page,
                    page: "Page",
                    card: None,
                    target: SettingsTarget::Page { tab, card: None },
                },
            ));
        }
    }
    hits.sort_by_key(|(rank, _)| *rank);
    hits.into_iter().map(|(_, hit)| hit).collect()
}

fn rank(label: &str, terms: &[String]) -> u8 {
    let label = label.to_lowercase();
    let first = terms.first().map_or("", String::as_str);
    if label.starts_with(first) {
        1
    } else if label.split_whitespace().any(|w| w.starts_with(first)) {
        2
    } else if terms.iter().all(|t| label.contains(t.as_str())) {
        3
    } else {
        4
    }
}

/// Words a provider goes by that its label doesn't spell out.
fn kind_keywords(kind: LlmProviderKind) -> &'static str {
    match kind {
        LlmProviderKind::ClaudeCodeAcp => "anthropic acp agent subscription",
        LlmProviderKind::CodexAcp | LlmProviderKind::GptCodex => "openai chatgpt gpt",
        LlmProviderKind::Router => "auto routing quota",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_settings_by_label_keyword_and_page() {
        let first = |q: &str| search(q).into_iter().next().map(|hit| hit.label);
        assert_eq!(first("wrap"), Some("Word wrap"));
        assert_eq!(
            first("sleep"),
            Some("Keep the computer awake while an agent works")
        );
        assert_eq!(first("voice"), Some("Voice"));
        assert!(search("claude").iter().any(|hit| matches!(
            hit.target,
            SettingsTarget::Provider {
                kind: Some(LlmProviderKind::ClaudeCodeAcp),
                card: None,
            }
        )));
        // Fields inside the provider forms and the Router page are searchable too.
        for (query, card) in [
            ("api key", "Connection"),
            ("base url", "Connection"),
            ("tls", "Connection"),
            ("budget", "How it decides"),
            ("failover", "How it decides"),
        ] {
            assert!(
                search(query).iter().any(|hit| matches!(
                    hit.target,
                    SettingsTarget::Provider { card: Some(c), .. } if c == card
                )),
                "{query}"
            );
        }
        assert!(search("   ").is_empty());
        assert!(search("no such setting zzz").is_empty());
    }
}
