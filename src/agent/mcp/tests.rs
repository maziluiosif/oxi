use super::*;
use crate::settings::{McpServerConfig, McpTransport};

#[test]
fn tool_names_are_sanitized() {
    assert_eq!(
        mcp_tool_name("my-server", "do.thing"),
        "mcp_my_server_do_thing"
    );
}

#[test]
fn env_lines_parse_and_skip_comments() {
    let cfg = McpServerConfig {
        env: "# token\nAPI_KEY = abc\n\nBAD LINE\nURL=http://x?a=b\n".into(),
        ..Default::default()
    };
    assert_eq!(
        cfg.env_pairs(),
        vec![
            ("API_KEY".to_string(), "abc".to_string()),
            ("URL".to_string(), "http://x?a=b".to_string())
        ]
    );
}

#[test]
fn tool_results_flatten_content_and_flag_errors() {
    let ok = json!({"content": [
        {"type": "text", "text": "hello"},
        {"type": "image", "mimeType": "image/png", "data": "AAAA"},
        {"type": "resource", "resource": {"uri": "file:///a", "text": "inner"}},
        {"type": "resource_link", "uri": "file:///b", "name": "b"}
    ]});
    let out = format_tool_result(&ok).unwrap();
    assert!(out.contains("hello"));
    assert!(out.contains("[image: image/png, 4 base64 chars]"));
    assert!(out.contains("inner"));
    assert!(out.contains("[resource link: file:///b] b"));

    let err = json!({"isError": true, "content": [{"type": "text", "text": "boom"}]});
    assert_eq!(format_tool_result(&err), Err("boom".into()));

    let structured = json!({"content": [], "structuredContent": {"n": 1}});
    assert_eq!(format_tool_result(&structured).unwrap(), r#"{"n":1}"#);
}

// ─── stdio: a small scripted server ─────────────────────────────────────────

const FAKE_SERVER: &str = r#"
import json, sys, time

def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    mid = msg.get("id")
    if method == "initialize":
        print("banner lines on stdout must not break the client", flush=True)
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}, "resources": {}},
            "serverInfo": {"name": "fake", "version": "1"}}})
    elif method == "tools/list":
        if (msg.get("params") or {}).get("cursor") == "page2":
            send({"jsonrpc": "2.0", "id": mid, "result": {"tools": [
                {"name": "slow", "inputSchema": {"type": "object"}},
                {"name": "quit", "inputSchema": {"type": "object"}}]}})
        else:
            send({"jsonrpc": "2.0", "id": mid, "result": {"nextCursor": "page2", "tools": [
                {"name": "echo", "description": "Echo text", "inputSchema": {"type": "object"}}]}})
    elif method == "tools/call":
        name = msg["params"]["name"]
        if name == "echo":
            # Ask the client something first; the client must answer before we reply.
            send({"jsonrpc": "2.0", "id": "srv-1", "method": "ping"})
            reply = json.loads(sys.stdin.readline())
            assert reply.get("id") == "srv-1" and "result" in reply
            send({"jsonrpc": "2.0", "id": mid, "result": {"content": [
                {"type": "text", "text": "echo:" + msg["params"]["arguments"]["text"]}]}})
        elif name == "slow":
            time.sleep(5)
            send({"jsonrpc": "2.0", "id": mid, "result": {"content": []}})
        elif name == "quit":
            sys.exit(0)
    elif method == "resources/list":
        send({"jsonrpc": "2.0", "id": mid, "result": {"resources": [
            {"uri": "mem://notes", "name": "notes", "mimeType": "text/plain"}]}})
    elif method == "resources/read":
        send({"jsonrpc": "2.0", "id": mid, "result": {"contents": [
            {"uri": msg["params"]["uri"], "text": "note body"}]}})
    elif mid is not None:
        send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "nope"}})
"#;

fn fake_stdio_server(tag: &str) -> McpServerConfig {
    let dir = std::env::temp_dir().join(format!("oxi-mcp-test-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("server.py");
    std::fs::write(&script, FAKE_SERVER).unwrap();
    McpServerConfig {
        name: "fake".into(),
        transport: McpTransport::Stdio,
        command: crate::agent::test_python_executable()
            .to_string_lossy()
            .into_owned(),
        args: vec![script.to_string_lossy().into_owned()],
        timeout_secs: Some(1),
        ..Default::default()
    }
}

#[test]
fn stdio_server_lists_paginated_tools_and_answers_calls() {
    let mgr = McpManager::new();
    let cfg = fake_stdio_server("calls");
    mgr.sync_servers(std::slice::from_ref(&cfg));
    let status = &mgr.statuses()[0];
    assert!(status.connected, "{status:?}");
    assert_eq!(status.tools, 3);
    assert!(status.resources);

    let names: Vec<String> = mgr
        .tool_definitions()
        .iter()
        .map(|d| d["function"]["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        [
            "mcp_fake_echo",
            "mcp_fake_slow",
            "mcp_fake_quit",
            "mcp_fake_list_resources",
            "mcp_fake_read_resource"
        ]
    );

    assert_eq!(
        mgr.call_tool("mcp_fake_echo", &json!({"text": "hi"})),
        Ok("echo:hi".into())
    );
    let listed = mgr
        .call_tool("mcp_fake_list_resources", &json!({}))
        .unwrap();
    assert!(listed.contains("mem://notes"), "{listed}");
    assert_eq!(
        mgr.call_tool("mcp_fake_read_resource", &json!({"uri": "mem://notes"})),
        Ok("note body".into())
    );
}

#[test]
fn stdio_calls_time_out_and_dead_servers_are_restarted() {
    let mgr = McpManager::new();
    let cfg = fake_stdio_server("restart");
    mgr.sync_servers(std::slice::from_ref(&cfg));

    let err = mgr.call_tool("mcp_fake_quit", &json!({})).unwrap_err();
    assert!(err.contains("exited"), "{err}");
    // The next call transparently starts a fresh process.
    assert_eq!(
        mgr.call_tool("mcp_fake_echo", &json!({"text": "back"})),
        Ok("echo:back".into())
    );

    let started = std::time::Instant::now();
    let err = mgr.call_tool("mcp_fake_slow", &json!({})).unwrap_err();
    assert!(err.contains("timed out"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[test]
fn unchanged_servers_keep_their_connection_across_syncs() {
    let mgr = McpManager::new();
    let cfg = fake_stdio_server("sync");
    mgr.sync_servers(std::slice::from_ref(&cfg));
    let first = mgr.snapshot()[0].clone();
    mgr.sync_servers(std::slice::from_ref(&cfg));
    assert!(Arc::ptr_eq(&first, &mgr.snapshot()[0]));

    let mut changed = cfg.clone();
    changed.timeout_secs = Some(2);
    mgr.sync_servers(&[changed]);
    assert!(!Arc::ptr_eq(&first, &mgr.snapshot()[0]));

    mgr.sync_servers(&[]);
    assert!(mgr.statuses().is_empty());
}

#[test]
fn unreachable_stdio_server_reports_an_error() {
    let mgr = McpManager::new();
    mgr.sync_servers(&[McpServerConfig {
        name: "missing".into(),
        command: "oxi-definitely-not-a-real-binary".into(),
        ..Default::default()
    }]);
    let status = &mgr.statuses()[0];
    assert!(!status.connected);
    assert!(status.error.is_some());
}

// ─── Streamable HTTP ────────────────────────────────────────────────────────

struct HttpServer;

impl wiremock::Respond for HttpServer {
    fn respond(&self, req: &wiremock::Request) -> wiremock::ResponseTemplate {
        use wiremock::ResponseTemplate;
        let msg: Value = serde_json::from_slice(&req.body).unwrap();
        let header = |name: &str| {
            req.headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string()
        };
        if header("authorization") != "Bearer tok" {
            return ResponseTemplate::new(401).set_body_string("bad token");
        }
        let method = msg["method"].as_str().unwrap_or("");
        if method != "initialize" && header("mcp-session-id") != "sess-1" {
            return ResponseTemplate::new(400).set_body_string("missing session");
        }
        let id = msg["id"].clone();
        match method {
            "initialize" => ResponseTemplate::new(200)
                .insert_header("mcp-session-id", "sess-1")
                .set_body_json(json!({"jsonrpc": "2.0", "id": id, "result": {
                    "protocolVersion": "2025-06-18", "capabilities": {"tools": {}}}})),
            "notifications/initialized" => ResponseTemplate::new(202),
            "tools/list" => {
                assert_eq!(header("mcp-protocol-version"), "2025-06-18");
                // Answer over SSE, with an unrelated notification first.
                let body = format!(
                    "event: message\ndata: {}\n\nevent: message\ndata: {}\n\n",
                    json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {}}),
                    json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [
                        {"name": "add", "inputSchema": {"type": "object"}}]}})
                );
                ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
            }
            "tools/call" => {
                let a = msg["params"]["arguments"]["a"].as_i64().unwrap();
                let b = msg["params"]["arguments"]["b"].as_i64().unwrap();
                ResponseTemplate::new(200).set_body_json(json!({"jsonrpc": "2.0", "id": id,
                    "result": {"content": [{"type": "text", "text": (a + b).to_string()}]}}))
            }
            _ => ResponseTemplate::new(200).set_body_json(json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": "nope"}})),
        }
    }
}

fn http_cfg(url: String, token: &str) -> McpServerConfig {
    McpServerConfig {
        name: "remote".into(),
        transport: McpTransport::Http,
        url,
        bearer_token: token.into(),
        ..Default::default()
    }
}

#[test]
fn http_server_session_sse_and_calls() {
    let server = IO_RT.block_on(async {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/mcp"))
            .respond_with(HttpServer)
            .mount(&server)
            .await;
        server
    });
    let mgr = McpManager::new();
    mgr.sync_servers(&[http_cfg(format!("{}/mcp", server.uri()), "tok")]);
    let status = &mgr.statuses()[0];
    assert!(status.connected, "{status:?}");
    assert_eq!(status.tools, 1);
    assert_eq!(
        mgr.call_tool("mcp_remote_add", &json!({"a": 2, "b": 3})),
        Ok("5".into())
    );

    let bad = McpManager::new();
    bad.sync_servers(&[http_cfg(format!("{}/mcp", server.uri()), "wrong")]);
    let err = bad.statuses()[0].error.clone().unwrap();
    assert!(err.contains("401"), "{err}");
}
