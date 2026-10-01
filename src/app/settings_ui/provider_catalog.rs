//! Shared provider grouping and presentation metadata used by settings and chat.

use crate::settings::LlmProviderKind;

/// Provider groups keep selectors skimmable.
pub(in crate::app) const PROVIDER_GROUPS: &[(&str, &[LlmProviderKind])] = &[
    ("Automatic", &[LlmProviderKind::Router]),
    (
        "Local / self-hosted",
        &[
            LlmProviderKind::LocalHf,
            LlmProviderKind::RemoteHf,
            LlmProviderKind::Ollama,
            LlmProviderKind::LmStudio,
            LlmProviderKind::LlamaCpp,
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
    (
        "External agents (ACP)",
        &[
            LlmProviderKind::ClaudeCodeAcp,
            LlmProviderKind::CursorAcp,
            LlmProviderKind::CodexAcp,
        ],
    ),
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
        LlmProviderKind::LlamaCpp => {
            "Talk to a llama.cpp llama-server (or vLLM, TabbyAPI, …) that you run yourself."
        }
        LlmProviderKind::OpenAi => "Any OpenAI-compatible Chat Completions endpoint.",
        LlmProviderKind::OpenRouter => "OpenRouter multi-model router.",
        LlmProviderKind::AzureOpenAi => "Azure OpenAI deployment endpoint.",
        LlmProviderKind::CustomAnthropic => "Any Anthropic Messages-compatible endpoint.",
        LlmProviderKind::GptCodex => "ChatGPT / Codex via OAuth or OpenAI API-key fallback.",
        LlmProviderKind::OpenCodeGo => "OpenCode Go subscription endpoint.",
        LlmProviderKind::ClaudeCodeAcp => {
            "Drive Claude Code as an external agent over the Agent Client Protocol."
        }
        LlmProviderKind::CursorAcp => {
            "Drive the installed Cursor CLI through its built-in Agent Client Protocol server."
        }
        LlmProviderKind::CodexAcp => {
            "Drive Codex CLI through the official Agent Client Protocol adapter."
        }
        LlmProviderKind::Router => {
            "Picks a provider, model and effort per turn from the ones you configured, using subscription quota, spend and task difficulty."
        }
    }
}
