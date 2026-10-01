//! Explicit routing instructions are parsed only from user messages, never tool output.
use crate::model::{ChatMessage, MsgRole};
use crate::settings::LlmProviderKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preference {
    pub provider: LlmProviderKind,
    pub persistent: bool,
    pub exclusive: bool,
}

fn normalized(s: &str) -> String {
    s.to_lowercase()
        .replace(['ă', 'â'], "a")
        .replace('î', "i")
        .replace(['ș', 'ş'], "s")
        .replace(['ț', 'ţ'], "t")
}

/// Deliberately conservative: a directive must start a sentence and name the provider
/// immediately after the routing verb. Comparisons, quotations and code are not directives.
pub fn parse(text: &str) -> Option<Preference> {
    let text = normalized(text);
    let mut in_code = false;
    let mut result = None;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code || line.trim_start().starts_with(['>', '"', '\'']) {
            continue;
        }
        for sentence in line.split([';', '\n']) {
            let mut s = sentence.trim();
            for prefix in ["te rog ", "please ", "pentru asta ", "for this task "] {
                s = s.strip_prefix(prefix).unwrap_or(s);
            }
            let persistent = ["de acum inainte ", "de acum ", "from now on ", "always "]
                .iter()
                .find_map(|prefix| s.strip_prefix(prefix));
            let persistent_flag = persistent.is_some();
            s = persistent.unwrap_or(s);
            let rest = [
                "vreau sa folosesti ",
                "i want you to use ",
                "foloseste ",
                "utilizeaza ",
                "lucreaza cu ",
                "continua cu ",
                "use ",
                "switch to ",
                "continue with ",
                "work with ",
            ]
            .iter()
            .find_map(|prefix| s.strip_prefix(prefix));
            let Some(mut rest) = rest else {
                continue;
            };
            let exclusive = rest.starts_with("doar ")
                || rest.starts_with("numai ")
                || rest.starts_with("only ");
            for prefix in ["doar ", "numai ", "only "] {
                rest = rest.strip_prefix(prefix).unwrap_or(rest);
            }
            rest = rest.strip_prefix("providerul ").unwrap_or(rest);
            let aliases = [
                ("router", LlmProviderKind::Router),
                ("claude code", LlmProviderKind::ClaudeCodeAcp),
                ("codex acp", LlmProviderKind::CodexAcp),
                ("cursor acp", LlmProviderKind::CursorAcp),
                ("anthropic api", LlmProviderKind::CustomAnthropic),
                ("openai api", LlmProviderKind::OpenAi),
                ("gpt codex", LlmProviderKind::GptCodex),
                ("openrouter", LlmProviderKind::OpenRouter),
                ("opencode go", LlmProviderKind::OpenCodeGo),
                ("azure openai", LlmProviderKind::AzureOpenAi),
                ("lm studio", LlmProviderKind::LmStudio),
                ("ollama", LlmProviderKind::Ollama),
                ("local hf", LlmProviderKind::LocalHf),
                ("remote hf", LlmProviderKind::RemoteHf),
                ("llama.cpp", LlmProviderKind::LlamaCpp),
                ("openai", LlmProviderKind::OpenAi),
                ("anthropic", LlmProviderKind::CustomAnthropic),
                ("codex", LlmProviderKind::CodexAcp),
                ("claude", LlmProviderKind::ClaudeCodeAcp),
                ("cursor", LlmProviderKind::CursorAcp),
            ];
            for (alias, provider) in aliases {
                if let Some(tail) = rest.strip_prefix(alias)
                    && !tail.starts_with(|c: char| c.is_alphanumeric() || c == '_')
                {
                    // A sentence that opens with the imperative is a directive even when the
                    // task after it is a question ("folosește claude care sunt știrile?").
                    // Only an alternative right after the name leaves the choice open.
                    if [" sau ", " or ", " ori "]
                        .iter()
                        .any(|alt| tail.starts_with(alt))
                    {
                        break;
                    }
                    result = Some(Preference {
                        provider,
                        persistent: persistent_flag,
                        exclusive,
                    });
                    break;
                }
            }
        }
    }
    result
}

/// Whether the message asks for a generated image. Codex is the only agent that can make
/// one, so the router sends these turns there. Requires a creation verb followed closely by
/// an image noun, so "fix the logo alignment" or "read this screenshot" don't qualify.
pub fn wants_image_generation(text: &str) -> bool {
    const VERBS: &[&str] = &[
        "genereaza",
        "generati",
        "creeaza",
        "creati",
        "fa",
        "fa-mi",
        "faceti",
        "deseneaza",
        "deseneaza-mi",
        "realizeaza",
        "produ",
        "design",
        "generate",
        "create",
        "make",
        "draw",
        "render",
        "produce",
        "paint",
        "sketch",
    ];
    const NOUNS: &[&str] = &[
        "imagine",
        "imagini",
        "poza",
        "poze",
        "fotografie",
        "ilustratie",
        "ilustratii",
        "logo",
        "sigla",
        "iconita",
        "pictograma",
        "wallpaper",
        "desen",
        "image",
        "images",
        "picture",
        "pictures",
        "photo",
        "illustration",
        "artwork",
        "icon",
        "avatar",
        "banner",
        "poster",
        "mockup",
        "sprite",
    ];
    let text = normalized(text);
    let mut in_code = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code || line.trim_start().starts_with('>') {
            continue;
        }
        let words: Vec<&str> = line
            .split(|c: char| !(c.is_alphanumeric() || c == '-'))
            .filter(|w| !w.is_empty())
            .collect();
        for (i, word) in words.iter().enumerate() {
            if VERBS.contains(word) && words[i + 1..].iter().take(5).any(|w| NOUNS.contains(w)) {
                return true;
            }
        }
    }
    false
}

/// A one-turn instruction supersedes the chat preference only for that turn. Persistent
/// instructions live in the saved user transcript and survive reopening the conversation.
/// Carry the actual preference separately from the generated summary text.
pub fn preserve_in_summary(summary: String, chat: &[ChatMessage]) -> String {
    if let Some(p) = persistent_for_chat(chat) {
        let name = match p.provider {
            LlmProviderKind::OpenAi => "OpenAI API",
            LlmProviderKind::CustomAnthropic => "Anthropic API",
            kind => kind.label(),
        };
        format!(
            "[Router preference]\nDe acum folosește {}{}.\n\n{}",
            if p.exclusive { "doar " } else { "" },
            name,
            summary
        )
    } else {
        summary
    }
}

fn message_preference(message: &ChatMessage) -> Option<Preference> {
    if message.is_summary {
        // Compaction text is model-generated context, not a fresh user instruction.
        let directive = message
            .text
            .strip_prefix("[Router preference]\n")?
            .lines()
            .next()?;
        parse(directive)
    } else {
        parse(&message.text)
    }
}

pub fn persistent_for_chat(chat: &[ChatMessage]) -> Option<Preference> {
    chat.iter()
        .rev()
        .filter(|m| m.role == MsgRole::User)
        .filter_map(message_preference)
        .find(|p| p.persistent)
        .filter(|p| p.provider != LlmProviderKind::Router)
}

pub fn for_chat(chat: &[ChatMessage]) -> Option<Preference> {
    let last = chat.iter().rposition(|m| m.role == MsgRole::User)?;
    if let Some(p) = message_preference(&chat[last]) {
        return (p.provider != LlmProviderKind::Router).then_some(p);
    }
    persistent_for_chat(&chat[..last])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directives_are_distinct_from_mentions() {
        assert_eq!(
            parse("Folosește Codex pentru asta").unwrap().provider,
            LlmProviderKind::CodexAcp
        );
        let p = parse("De acum folosește doar Claude Code").unwrap();
        assert!(p.persistent && p.exclusive);
        assert!(parse("Compară Codex cu Claude").is_none());
        assert!(parse("Nu folosi Codex").is_none());
        assert!(parse("Cum pot folosi Codex?").is_none());
        assert!(parse("> use codex\n```\nuse claude\n```").is_none());
        assert!(parse("Use Codex or Claude").is_none());
        assert!(parse("use codexwhatever").is_none());
        for text in [
            "foloseste claude: fa un review la modificari",
            "Folosește Claude - verifică dacă merge, ce zici?",
            "use claude! review this",
            "folosește claude code (acp) pentru review",
        ] {
            assert_eq!(
                parse(text).unwrap().provider,
                LlmProviderKind::ClaudeCodeAcp,
                "{text}"
            );
        }
        for text in [
            "foloseste claude care sunt stirile de azi?",
            "Use Claude for this?",
            "folosește claude dacă merge",
        ] {
            assert_eq!(
                parse(text).unwrap().provider,
                LlmProviderKind::ClaudeCodeAcp,
                "{text}"
            );
        }
    }
    fn user(text: &str) -> ChatMessage {
        ChatMessage {
            role: MsgRole::User,
            text: text.into(),
            is_summary: false,
            attachments: vec![],
            blocks: vec![],
            streaming: false,
            started_at: None,
            worked_duration: None,
            route: None,
        }
    }

    #[test]
    fn persistent_choice_survives_one_turn_overrides_and_can_be_reset() {
        let mut chat = vec![
            user("De acum folosește Claude Code"),
            user("Folosește doar Codex pentru asta"),
        ];
        assert_eq!(for_chat(&chat).unwrap().provider, LlmProviderKind::CodexAcp);
        chat.push(user("continuă"));
        assert_eq!(
            for_chat(&chat).unwrap().provider,
            LlmProviderKind::ClaudeCodeAcp
        );
        chat.push(user("De acum folosește Router"));
        chat.push(user("repară testele"));
        assert!(for_chat(&chat).is_none());
        assert_eq!(
            parse("Use llama.cpp for this").unwrap().provider,
            LlmProviderKind::LlamaCpp
        );
        let mut summary = user("De acum folosește doar Codex");
        summary.is_summary = true;
        assert!(for_chat(&[summary.clone(), user("continuă")]).is_none());
        summary.text =
            "[Router preference]\nDe acum folosește doar Claude Code.\n\nDe acum folosește Codex"
                .into();
        assert_eq!(
            for_chat(&[summary, user("continuă")]).unwrap().provider,
            LlmProviderKind::ClaudeCodeAcp
        );
    }
    #[test]
    fn compaction_preserves_each_provider_without_parsing_generated_instructions() {
        for kind in LlmProviderKind::ALL
            .into_iter()
            .filter(|k| *k != LlmProviderKind::Router)
        {
            let name = match kind {
                LlmProviderKind::OpenAi => "OpenAI API",
                LlmProviderKind::CustomAnthropic => "Anthropic API",
                k => k.label(),
            };
            let chat = vec![user(&format!("De acum înainte folosește doar {name}"))];
            let mut summary = user(&preserve_in_summary("Use Codex".into(), &chat));
            summary.is_summary = true;
            let p = for_chat(&[summary, user("continue")]).unwrap();
            assert_eq!(p.provider, kind);
            assert!(p.persistent && p.exclusive);
        }
    }
    #[test]
    fn image_generation_requests_are_detected_without_matching_plain_mentions() {
        for text in [
            "Generează o imagine cu o pisică pe lună",
            "fă-mi un logo pentru aplicație",
            "deseneaza-mi o poza cu un munte",
            "Create an image of a sunset",
            "make me a new app icon",
        ] {
            assert!(wants_image_generation(text), "{text}");
        }
        for text in [
            "fix the logo alignment in the header",
            "ce vezi în imaginea atașată?",
            "make the tests pass",
            "fă logo-ul mai mare",
            "fă un review la imaginea atașată",
            "```\ngenerate image\n```",
            "care sunt știrile de azi?",
        ] {
            assert!(!wants_image_generation(text), "{text}");
        }
    }
}
