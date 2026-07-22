//! Shared provider grouping and presentation metadata used by settings pages.

use crate::settings::LlmProviderKind;

/// Provider groups keep selectors skimmable.
pub(super) const PROVIDER_GROUPS: &[(&str, &[LlmProviderKind])] = &[
    (
        "Local / self-hosted",
        &[
            LlmProviderKind::LocalHf,
            LlmProviderKind::RemoteHf,
            LlmProviderKind::Ollama,
            LlmProviderKind::LmStudio,
        ],
    ),
    (
        "Hosted APIs",
        &[
            LlmProviderKind::OpenAi,
            LlmProviderKind::OpenRouter,
            LlmProviderKind::AzureOpenAi,
            LlmProviderKind::CustomAnthropic,
            LlmProviderKind::GptCodex,
            LlmProviderKind::OpenCodeGo,
        ],
    ),
    ("External agents", &[LlmProviderKind::ClaudeCodeAcp]),
];

pub(super) fn provider_blurb(kind: LlmProviderKind) -> &'static str {
    match kind {
        LlmProviderKind::LocalHf => {
            "Download GGUF models and run them locally via oxi-managed llama-server."
        }
        LlmProviderKind::RemoteHf => {
            "Download and run GGUF models on an SSH host via oxi-managed llama-server."
        }
        LlmProviderKind::Ollama => {
            "Talk to a local or LAN Ollama server (OpenAI-compatible /v1 API)."
        }
        LlmProviderKind::LmStudio => "Talk to a local or LAN LM Studio server (OpenAI-compatible).",
        LlmProviderKind::OpenAi => "OpenAI Chat Completions API.",
        LlmProviderKind::OpenRouter => "OpenRouter multi-model router.",
        LlmProviderKind::AzureOpenAi => "Azure OpenAI deployment endpoint.",
        LlmProviderKind::CustomAnthropic => "Any Anthropic Messages-compatible endpoint.",
        LlmProviderKind::GptCodex => "ChatGPT / Codex via OAuth or OpenAI API-key fallback.",
        LlmProviderKind::OpenCodeGo => "OpenCode Go subscription endpoint.",
        LlmProviderKind::ClaudeCodeAcp => {
            "Drive Claude Code as an external agent over the Agent Client Protocol."
        }
    }
}
