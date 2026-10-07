//! oxi's MCP servers in the shape ACP `session/new` / `session/load` / `session/resume` take,
//! so the agent can use the same servers as oxi's own agent loop.

use serde_json::{Value, json};

use crate::settings::{McpServerConfig, McpTransport};

/// The servers worth handing to an agent: enabled and filled in.
pub(super) fn usable(servers: &[McpServerConfig]) -> Vec<McpServerConfig> {
    servers.iter().filter(|s| s.is_usable()).cloned().collect()
}

/// ACP `McpServer` entries for `servers`. Every agent takes stdio servers; HTTP ones only go to
/// agents advertising `mcpCapabilities.http`.
pub(super) fn to_acp(servers: &[McpServerConfig], agent_caps: &Value) -> Value {
    let http = agent_caps["mcpCapabilities"]["http"].as_bool() == Some(true);
    let entries = servers
        .iter()
        .filter(|s| s.is_usable())
        .filter_map(|s| match s.transport {
            McpTransport::Stdio => {
                let env: Vec<Value> = s
                    .env_pairs()
                    .into_iter()
                    .map(|(name, value)| json!({ "name": name, "value": value }))
                    .collect();
                let (command, args) = stdio_launch(s.command.trim(), &s.args);
                Some(json!({ "name": s.name.trim(), "command": command, "args": args, "env": env }))
            }
            McpTransport::Http if http => {
                let token = s.bearer_token.trim();
                let headers: Vec<Value> = if token.is_empty() {
                    Vec::new()
                } else {
                    vec![json!({ "name": "Authorization", "value": format!("Bearer {token}") })]
                };
                Some(json!({
                    "type": "http",
                    "name": s.name.trim(),
                    "url": s.url.trim(),
                    "headers": headers,
                }))
            }
            McpTransport::Http => {
                log::warn!(
                    "ACP agent does not take HTTP MCP servers; skipping `{}`",
                    s.name.trim()
                );
                None
            }
        })
        .collect();
    Value::Array(entries)
}

/// The agent spawns stdio servers itself without a shell. On Windows `npx`/`uvx` are `.cmd`
/// shims that only resolve through `cmd`, so route them the way oxi's own MCP client does.
fn stdio_launch(command: &str, args: &[String]) -> (String, Vec<String>) {
    if cfg!(windows) {
        let mut wrapped = vec!["/D".to_string(), "/S".to_string(), "/C".to_string()];
        wrapped.push(command.to_string());
        wrapped.extend(args.iter().cloned());
        ("cmd".to_string(), wrapped)
    } else {
        (command.to_string(), args.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.into(),
            command: "npx".into(),
            args: vec!["-y".into(), "server-fs".into()],
            env: "TOKEN=abc\n# comment\n".into(),
            ..Default::default()
        }
    }

    fn http(name: &str, token: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.into(),
            transport: McpTransport::Http,
            url: "https://example.com/mcp".into(),
            bearer_token: token.into(),
            ..Default::default()
        }
    }

    #[test]
    fn stdio_and_http_servers_map_to_acp_entries() {
        let servers = [stdio("fs"), http("remote", "secret")];
        let caps = json!({ "mcpCapabilities": { "http": true } });
        let out = to_acp(&servers, &caps);
        let entries = out.as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["name"], "fs");
        assert_eq!(
            entries[0]["env"],
            json!([{ "name": "TOKEN", "value": "abc" }])
        );
        if !cfg!(windows) {
            assert_eq!(entries[0]["command"], "npx");
            assert_eq!(entries[0]["args"], json!(["-y", "server-fs"]));
        }
        assert_eq!(entries[1]["type"], "http");
        assert_eq!(
            entries[1]["headers"],
            json!([{ "name": "Authorization", "value": "Bearer secret" }])
        );
    }

    #[test]
    fn http_servers_need_the_agent_capability_and_unusable_ones_are_dropped() {
        let disabled = McpServerConfig {
            enabled: false,
            ..stdio("off")
        };
        let servers = [stdio("fs"), http("remote", ""), disabled];
        let out = to_acp(&servers, &json!({}));
        assert_eq!(out.as_array().unwrap().len(), 1);
        assert_eq!(usable(&servers).len(), 2);

        let caps = json!({ "mcpCapabilities": { "http": true } });
        let out = to_acp(&[http("remote", "")], &caps);
        assert_eq!(out[0]["headers"], json!([]));
    }
}
