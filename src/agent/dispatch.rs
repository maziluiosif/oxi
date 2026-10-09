//! Provider dispatch: resolve a provider's endpoint and credentials, then drive the matching
//! streaming loop (OpenAI Chat Completions, Azure, Anthropic Messages, Codex Responses).
//!
//! Shared by the main agent run ([`crate::agent::runner`]), one-shot completions
//! ([`crate::agent::complete`]) and sub-agents ([`crate::agent::subagent`]), so provider quirks
//! (OpenCode Go's two protocols, Codex OAuth, SSH tunnels) live in exactly one place.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;

use serde_json::Value;

use super::anthropic::run_anthropic_loop;
use super::approval::ApprovalGate;
use super::codex_responses::run_codex_responses_loop;
use super::events::AgentEvent;
use super::loop_ctx::LoopCtx;
use super::openai::{run_azure_chat_loop, run_chat_loop};
use super::runner::{
    azure_openai_api_version, configured_azure_openai_key, configured_custom_anthropic_key,
    configured_lmstudio_key, configured_ollama_key, configured_openai_key,
    configured_opencode_go_key, configured_openrouter_key, opencode_go_model_uses_anthropic,
    openrouter_extra_headers,
};
use super::tools::{
    MAX_TOOL_OUTPUT_CHARS, ToolEnv, ToolOutputCallback, ToolResult, run_tool, run_tool_with_output,
};
use crate::oauth::{ensure_codex_access_token, load_oauth_store};
use crate::settings::{LlmProviderKind, ProviderConfig};

pub(crate) struct DispatchParams<'a> {
    pub cfg: &'a ProviderConfig,
    pub client: &'a reqwest::Client,
    /// SSH tunnels for remote runtimes. `None` uses the configured base URL as-is.
    pub tunnels: Option<&'a crate::compute::TunnelManager>,
    pub cwd: &'a Path,
    pub env: &'a ToolEnv,
    pub tx: &'a Sender<AgentEvent>,
    pub cancel: &'a Arc<AtomicBool>,
    pub gate: &'a mut ApprovalGate,
    pub max_rounds: u32,
    pub effort_override: Option<&'a str>,
    pub context_char_budget: usize,
    pub tools_chars: usize,
}

/// HTTP client tuned for streaming: bound connect time and idle time between chunks rather than
/// the whole request (which would cut long turns off mid-stream), keep NAT/proxy paths alive.
pub(crate) fn streaming_client(
    cfg: &ProviderConfig,
    read_timeout_secs: u64,
) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(read_timeout_secs))
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .tls_danger_accept_invalid_certs(cfg.allows_self_signed_tls())
        .build()
        .map_err(|e| e.to_string())
}

/// Run the provider loop for `p.cfg` over `messages` until the model stops calling tools (or
/// `max_rounds` is reached). `messages` accumulates the provider-native history.
pub(crate) async fn run_provider_loop(
    p: DispatchParams<'_>,
    messages: &mut Vec<Value>,
    tools: &[Value],
) -> Result<(), String> {
    let DispatchParams {
        cfg,
        client,
        tunnels,
        cwd,
        env,
        tx,
        cancel,
        gate,
        max_rounds,
        effort_override,
        context_char_budget,
        tools_chars,
    } = p;
    let model = cfg.model_id.clone();
    let local_base = async || match tunnels {
        Some(t) => crate::compute::resolve_base_url(cfg, t).await,
        None => Ok(cfg.effective_base_url()),
    };
    macro_rules! ctx {
        ($base:expr, $model:expr) => {
            &mut LoopCtx {
                client,
                base_url: $base,
                model: $model,
                cwd,
                env,
                tx,
                cancel,
                gate,
                max_rounds,
                effort_override,
                context_char_budget,
                tools_chars,
            }
        };
    }

    match cfg.provider {
        LlmProviderKind::GptCodex => {
            let mut oauth = load_oauth_store();
            if oauth.openai_codex.is_some() {
                let creds = ensure_codex_access_token(client, &mut oauth).await?;
                let base = if cfg.base_url.trim().is_empty() {
                    "https://chatgpt.com/backend-api".to_string()
                } else {
                    cfg.effective_base_url()
                };
                run_codex_responses_loop(ctx!(&base, &model), &creds.0, &creds.1, messages, tools)
                    .await
            } else {
                let key = configured_openai_key(cfg)?;
                let base = cfg.effective_base_url();
                run_chat_loop(ctx!(&base, &model), &key, &[], messages, tools).await
            }
        }
        LlmProviderKind::OpenAi => {
            let key = configured_openai_key(cfg)?;
            let base = cfg.effective_base_url();
            run_chat_loop(ctx!(&base, &model), &key, &[], messages, tools).await
        }
        LlmProviderKind::OpenRouter => {
            let key = configured_openrouter_key(cfg)?;
            let base = cfg.effective_base_url();
            let headers = openrouter_extra_headers(cfg);
            run_chat_loop(ctx!(&base, &model), &key, &headers, messages, tools).await
        }
        LlmProviderKind::AzureOpenAi => {
            let key = configured_azure_openai_key(cfg)?;
            let base = cfg.effective_base_url();
            let api_version = azure_openai_api_version();
            run_azure_chat_loop(ctx!(&base, &model), &key, &api_version, messages, tools).await
        }
        LlmProviderKind::CustomAnthropic => {
            let key = configured_custom_anthropic_key(cfg)?;
            let base = cfg.effective_base_url();
            run_anthropic_loop(ctx!(&base, &model), &key, &[], messages, tools).await
        }
        LlmProviderKind::LmStudio
        | LlmProviderKind::LlamaCpp
        | LlmProviderKind::LocalHf
        | LlmProviderKind::RemoteHf => {
            let key = configured_lmstudio_key(cfg);
            let base = local_base().await?;
            run_chat_loop(ctx!(&base, &model), &key, &[], messages, tools).await
        }
        LlmProviderKind::Ollama => {
            let key = configured_ollama_key(cfg);
            let base = local_base().await?;
            run_chat_loop(ctx!(&base, &model), &key, &[], messages, tools).await
        }
        LlmProviderKind::OpenCodeGo => {
            let key = configured_opencode_go_key(cfg)?;
            let base = cfg.effective_base_url();
            let model = model
                .strip_prefix("opencode-go/")
                .unwrap_or(&model)
                .to_string();
            if opencode_go_model_uses_anthropic(&model) {
                // OpenCode Go exposes Anthropic-compatible models at
                // https://opencode.ai/zen/go/v1/messages. `run_anthropic_loop`
                // appends `/v1/messages`, so pass the base without `/v1`.
                let anthropic_base = base.trim_end_matches("/v1").to_string();
                run_anthropic_loop(ctx!(&anthropic_base, &model), &key, &[], messages, tools).await
            } else {
                // OpenCode Go exposes OpenAI-compatible models at
                // https://opencode.ai/zen/go/v1/chat/completions. `run_chat_loop`
                // appends `/chat/completions`, so include `/v1` in the base.
                let chat_base = if base.trim_end_matches('/').ends_with("/v1") {
                    base
                } else {
                    format!("{}/v1", base.trim_end_matches('/'))
                };
                run_chat_loop(ctx!(&chat_base, &model), &key, &[], messages, tools).await
            }
        }
        LlmProviderKind::ClaudeCodeAcp | LlmProviderKind::CursorAcp | LlmProviderKind::CodexAcp => {
            Err(
                "ACP agents run their own agent loop; oxi cannot drive them as a plain model."
                    .to_string(),
            )
        }
        LlmProviderKind::Router => Err(
            "Router: no HTTP provider is available for this request (ACP agents can't run one-shot completions)."
                .to_string(),
        ),
    }
}

/// Run one call of a parallel read-only batch on a blocking thread. Each call reports its own
/// output and end the moment it finishes, so a slow `task` sub-agent does not hold back the pills
/// of faster siblings; sub-agents also stream their progress into the pill while they work.
/// The caller still appends the results to the conversation in call order.
/// Ask for approval, then run a mutating tool in place (in call order). Both steps block —
/// the approval wait on the user, `bash` for up to its timeout — so they run under
/// [`crate::runtime::block_in_place`] instead of stalling a runtime worker.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_gated_tool(
    gate: &mut ApprovalGate,
    tx: &Sender<AgentEvent>,
    cancel: &Arc<AtomicBool>,
    cwd: &Path,
    id: &str,
    name: &str,
    args: &Value,
    env: &ToolEnv,
) -> ToolResult {
    crate::runtime::block_in_place(|| match gate.request(tx, cancel, name, args) {
        Ok(()) if name.eq_ignore_ascii_case("bash") => {
            let event_tx = tx.clone();
            let id = id.to_string();
            let callback: ToolOutputCallback = Arc::new(move |text| {
                let truncated = text.chars().count() >= MAX_TOOL_OUTPUT_CHARS;
                let _ = event_tx.send(AgentEvent::ToolOutput {
                    tool_call_id: id.clone(),
                    text,
                    truncated,
                });
            });
            run_tool_with_output(cwd, name, args, env, Some(callback))
        }
        Ok(()) => run_tool(cwd, name, args, env),
        Err(reason) => ToolResult {
            output: reason,
            is_error: true,
            diff: None,
            full_output_path: None,
        },
    })
}

pub(crate) fn spawn_readonly_tool(
    cwd: &Path,
    id: &str,
    name: &str,
    args: &Value,
    env: &ToolEnv,
    tx: &Sender<AgentEvent>,
) -> tokio::task::JoinHandle<ToolResult> {
    let cwd = cwd.to_path_buf();
    let id = id.to_string();
    let name = name.to_string();
    let args = args.clone();
    let env = env.clone();
    let tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        let on_output = (name == "task").then(|| {
            let (tx, id) = (tx.clone(), id.clone());
            Arc::new(move |text: String| {
                let _ = tx.send(AgentEvent::ToolOutput {
                    tool_call_id: id.clone(),
                    text,
                    truncated: false,
                });
            }) as ToolOutputCallback
        });
        let result = run_tool_with_output(&cwd, &name, &args, &env, on_output);
        let _ = tx.send(AgentEvent::ToolOutput {
            tool_call_id: id.clone(),
            text: result.output.clone(),
            truncated: result.output.len() >= MAX_TOOL_OUTPUT_CHARS,
        });
        let _ = tx.send(AgentEvent::ToolEnd {
            tool_call_id: id,
            is_error: Some(result.is_error),
            full_output_path: result.full_output_path.clone(),
            diff: result.diff.clone(),
        });
        result
    })
}

/// Parse a model's tool-call `arguments`. Small local models often wrap the JSON in a code fence,
/// add prose around it, or double-encode it as a string; those are repaired. Anything else comes
/// back as an error naming what was received, so the model can retry the call instead of the tool
/// running with empty arguments and failing with a misleading "missing path".
pub(crate) fn parse_tool_args(name: &str, raw: &str) -> Result<Value, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    let first_err = match serde_json::from_str::<Value>(trimmed) {
        Ok(v @ Value::Object(_)) => return Ok(v),
        Ok(Value::String(inner)) => match serde_json::from_str::<Value>(inner.trim()) {
            Ok(v @ Value::Object(_)) => return Ok(v),
            _ => "expected a JSON object, got a string".to_string(),
        },
        Ok(_) => "expected a JSON object".to_string(),
        Err(e) => e.to_string(),
    };
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}'))
        && start < end
        && let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(&trimmed[start..=end])
    {
        return Ok(v);
    }
    const MAX_ECHO: usize = 500;
    let echo: String = trimmed.chars().take(MAX_ECHO).collect();
    let ellipsis = if trimmed.chars().count() > MAX_ECHO {
        "…"
    } else {
        ""
    };
    Err(format!(
        "The arguments for `{name}` were not valid JSON ({first_err}). Received: {echo}{ellipsis}\n\
         Call `{name}` again with a single JSON object that matches its parameters."
    ))
}

/// The `arguments` to record for a tool call in the assistant turn replayed next round:
/// unchanged when they parsed as sent, the repaired JSON when [`parse_tool_args`] fixed them, `{}`
/// when they were unusable (the tool result already echoes what was received). llama-server and
/// Ollama reject a request whose history holds tool-call arguments that don't parse.
pub(crate) fn replay_tool_args(raw: &str, parsed: &Result<Value, String>) -> String {
    match parsed {
        Ok(v) if serde_json::from_str::<Value>(raw).is_ok_and(|r| r == *v) => raw.to_string(),
        Ok(v) => v.to_string(),
        Err(_) => "{}".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_tool_args, replay_tool_args};
    use serde_json::json;

    #[test]
    fn parse_tool_args_accepts_valid_and_empty() {
        assert_eq!(
            parse_tool_args("read", r#"{"path":"a.rs"}"#),
            Ok(json!({"path": "a.rs"}))
        );
        assert_eq!(parse_tool_args("ls", "  "), Ok(json!({})));
    }

    #[test]
    fn parse_tool_args_repairs_fences_prose_and_double_encoding() {
        let fenced = "```json\n{\"path\": \"src/main.rs\"}\n```";
        assert_eq!(
            parse_tool_args("read", fenced),
            Ok(json!({"path": "src/main.rs"}))
        );
        let prose = "Sure, reading it: {\"path\": \"x\"} hope that helps";
        assert_eq!(parse_tool_args("read", prose), Ok(json!({"path": "x"})));
        let doubled = r#""{\"path\": \"y\"}""#;
        assert_eq!(parse_tool_args("read", doubled), Ok(json!({"path": "y"})));
    }

    #[test]
    fn parse_tool_args_reports_garbage_with_what_was_sent() {
        let err = parse_tool_args("edit", r#"{"path": "a.rs", "old": "x"#).unwrap_err();
        assert!(err.contains("`edit`"), "{err}");
        assert!(err.contains(r#"Received: {"path": "a.rs""#), "{err}");
        assert!(parse_tool_args("bash", "[1, 2]").is_err());
        let long = "x".repeat(2_000);
        assert!(parse_tool_args("bash", &long).unwrap_err().contains('…'));
    }

    #[test]
    fn replayed_arguments_are_always_valid_json() {
        let raw = r#"{"path": "a.rs"}"#;
        assert_eq!(replay_tool_args(raw, &parse_tool_args("read", raw)), raw);
        let fenced = "```json\n{\"path\": \"a.rs\"}\n```";
        assert_eq!(
            replay_tool_args(fenced, &parse_tool_args("read", fenced)),
            r#"{"path":"a.rs"}"#
        );
        let cut = r#"{"path": "a.rs"#;
        assert_eq!(replay_tool_args(cut, &parse_tool_args("read", cut)), "{}");
        assert_eq!(replay_tool_args("", &parse_tool_args("ls", "")), "{}");
    }
}
