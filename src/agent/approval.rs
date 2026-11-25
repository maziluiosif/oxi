//! User approval gate for shell and built-in filesystem-changing tools.
//!
//! When approval is enabled, the agent thread sends [`AgentEvent::ApprovalRequest`] and blocks
//! until the UI returns an [`ApprovalDecision`] over a back-channel. Read-only tools
//! (`read` / `grep` / `find` / `ls`) never require approval.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use serde_json::Value;

use super::events::AgentEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Run this tool call.
    Approve,
    /// Run this `bash` call and, from now on, every simple command starting with this prefix
    /// (the UI also saves it to the settings allowlist for later runs).
    AllowPrefix(String),
    /// Run this call and, from now on, every call of this (non-`bash`) tool in the workspace
    /// (the UI also saves it to the settings for later runs).
    AllowTool(String),
    /// Run this and auto-approve every remaining tool in the current run.
    ApproveRest,
    /// Refuse this tool call; the model is told the user denied it.
    Deny,
    /// The `elicitation/create` response to a question the agent asked.
    Elicitation(serde_json::Value),
}

/// Extract the shell command string from native (`command`) or ACP (`CommandLine`, e.g.
/// Antigravity `run_command`) tool arguments.
pub(crate) fn bash_command_arg(args: &Value) -> Option<&str> {
    args.get("command")
        .or_else(|| args.get("CommandLine"))
        .and_then(Value::as_str)
}

/// Build a short human-readable summary of a pending tool call for the approval prompt.
pub fn approval_summary(name: &str, args: &Option<Value>) -> String {
    let Some(args) = args else {
        return name.to_string();
    };
    let fields: &[&str] = match name {
        "bash" => &["command", "CommandLine"],
        "write" | "edit" | "delete" | "mkdir" => &["path", "Path", "file_path", "filePath"],
        "move" => &["from", "From"],
        "scratchpad" => &["mode"],
        _ => &[],
    };
    fields
        .iter()
        .find_map(|field| args.get(*field).and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Approval switches for tool categories that can mutate the workspace or run shell commands,
/// and for each of oxi's own tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalPolicy {
    pub write_edit: bool,
    pub bash: bool,
    /// Settings > Agent > "Ask before oxi tools".
    pub oxi_tools: crate::settings::OxiToolConfirm,
}

impl ApprovalPolicy {
    /// The given switches, with oxi's own tools at their defaults.
    #[cfg(test)]
    pub fn new(write_edit: bool, bash: bool) -> Self {
        Self {
            write_edit,
            bash,
            oxi_tools: crate::settings::OxiToolConfirm::defaults(write_edit),
        }
    }

    pub fn from_settings(settings: &crate::settings::AppSettings) -> Self {
        Self {
            write_edit: settings.require_write_edit_approval,
            bash: settings.require_bash_approval,
            oxi_tools: settings.oxi_tool_confirm(),
        }
    }

    pub fn disabled() -> Self {
        Self {
            write_edit: false,
            bash: false,
            oxi_tools: crate::settings::OxiToolConfirm::none(),
        }
    }

    pub fn requires_approval(self, name: &str) -> bool {
        // Only there when the user switched computer use on, which is the approval.
        if crate::computer_use::is_tool(name) {
            return false;
        }
        if let Some(asks) = self.oxi_tools.asks(name) {
            return asks;
        }
        match crate::agent::tools::tool_side_effect(name) {
            crate::agent::tools::ToolSideEffect::ReadOnly => false,
            crate::agent::tools::ToolSideEffect::WorkspaceMutation => self.write_edit,
            crate::agent::tools::ToolSideEffect::Shell => self.bash,
            crate::agent::tools::ToolSideEffect::UnknownExternal => true,
        }
    }
}

/// Whether `command` may skip the `bash` prompt because it starts with an allowlisted prefix.
///
/// Prefixes match whole words (`git status` matches `git status -s`, not `git statusx`). Only
/// simple commands qualify: anything with shell operators, redirection, substitution, globbing
/// or escapes (`git status; rm -rf ~`, `ls $(…)`, `cat a > b`) always asks, so an allowed prefix
/// can't smuggle a second command.
pub fn bash_command_allowlisted(command: &str, allowlist: &[String]) -> bool {
    let Some(words) = simple_command_words(command) else {
        return false;
    };
    allowlist.iter().any(|entry| {
        let prefix: Vec<&str> = entry.split_whitespace().collect();
        !prefix.is_empty() && words.len() >= prefix.len() && words[..prefix.len()] == prefix[..]
    })
}

/// Whether `name` runs without asking because the user allowed the tool in this workspace.
/// `bash` never does: its commands are allowed by prefix instead.
pub fn tool_allowed(name: &str, allowed: &[String]) -> bool {
    !name.eq_ignore_ascii_case("bash") && allowed.iter().any(|t| t == name)
}

/// Allowlist entry to offer for `command`: the program plus its subcommand when there is one
/// (`cargo test`, `git status`, `npm run build`), or `None` when the command isn't simple.
pub fn suggest_bash_allow_prefix(command: &str) -> Option<String> {
    let words = simple_command_words(command)?;
    let is_subcommand = |w: &&str| {
        w.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
            && !w.starts_with('-')
    };
    let mut prefix: Vec<&str> = vec![words[0]];
    if let Some(sub) = words.get(1).copied().filter(is_subcommand) {
        prefix.push(sub);
        // Script runners take the script name as a second subcommand.
        if matches!(words[0], "npm" | "pnpm" | "yarn" | "bun")
            && sub == "run"
            && let Some(script) = words.get(2).copied().filter(is_subcommand)
        {
            prefix.push(script);
        }
    }
    Some(prefix.join(" "))
}

/// Whitespace-split words of a command with no shell syntax beyond plain arguments, or `None`.
fn simple_command_words(command: &str) -> Option<Vec<&str>> {
    const SHELL_SYNTAX: &[char] = &[
        ';', '&', '|', '<', '>', '`', '$', '(', ')', '{', '}', '\\', '\n', '\r', '*', '?', '[',
        '~', '!', '#', '"', '\'', '=',
    ];
    if command.contains(SHELL_SYNTAX) {
        return None;
    }
    let words: Vec<&str> = command.split_whitespace().collect();
    (!words.is_empty()).then_some(words)
}

/// Whether `command` is a strictly read-only inspection shell command (or pipeline / `&&` / `||`
/// chain of read-only commands) safe to run in plan mode or by a read-only orchestrator worker.
///
/// Rejects newlines, command/process substitution, output redirections (other than `/dev/null`
/// and `2>&1`), backgrounding, environment assignments, and any command or flag that can modify
/// files or execute arbitrary subcommands.
pub fn bash_command_is_read_only(command: &str) -> bool {
    let Some(segments) = parse_read_only_segments(command) else {
        return false;
    };
    !segments.is_empty() && segments.iter().all(|seg| segment_is_read_only(seg))
}

fn parse_read_only_segments(command: &str) -> Option<Vec<Vec<String>>> {
    if command.trim().is_empty() || command.contains(['\n', '\r']) {
        return None;
    }
    let chars: Vec<char> = command.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut segments: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut token = String::new();
    let mut has_token = false;

    let flush_token = |current: &mut Vec<String>, token: &mut String, has_token: &mut bool| {
        if *has_token {
            current.push(std::mem::take(token));
            *has_token = false;
        }
    };

    while i < len {
        let c = chars[i];
        match c {
            ' ' | '\t' => {
                flush_token(&mut current, &mut token, &mut has_token);
                i += 1;
            }
            '\'' => {
                has_token = true;
                i += 1;
                let mut closed = false;
                while i < len {
                    if chars[i] == '\'' {
                        closed = true;
                        i += 1;
                        break;
                    }
                    token.push(chars[i]);
                    i += 1;
                }
                if !closed {
                    return None;
                }
            }
            '"' => {
                has_token = true;
                i += 1;
                let mut closed = false;
                while i < len {
                    match chars[i] {
                        '"' => {
                            closed = true;
                            i += 1;
                            break;
                        }
                        '`' => return None,
                        '$' if i + 1 < len && chars[i + 1] == '(' => return None,
                        '\\' if i + 1 < len => {
                            token.push(chars[i + 1]);
                            i += 2;
                        }
                        ch => {
                            token.push(ch);
                            i += 1;
                        }
                    }
                }
                if !closed {
                    return None;
                }
            }
            '\\' => {
                if i + 1 >= len {
                    return None;
                }
                has_token = true;
                token.push(chars[i + 1]);
                i += 2;
            }
            '`' | '(' | ')' => return None,
            '$' if i + 1 < len && chars[i + 1] == '(' => return None,
            '<' => return None,
            '>' => {
                // Only allow safe discard/stderr redirects: `>/dev/null`, `1>/dev/null`,
                // `2>/dev/null`, `2>&1`, `&>/dev/null`.
                let fd_prefix = if has_token && (token == "1" || token == "2") {
                    token.clear();
                    has_token = false;
                    true
                } else {
                    flush_token(&mut current, &mut token, &mut has_token);
                    false
                };
                if i + 1 < len && chars[i + 1] == '>' {
                    return None;
                }
                i += 1;
                while i < len && matches!(chars[i], ' ' | '\t') {
                    i += 1;
                }
                if fd_prefix && chars[i..].starts_with(&['&', '1']) {
                    i += 2;
                    if i < len && !matches!(chars[i], ' ' | '\t' | '|' | '&' | ';') {
                        return None;
                    }
                } else if chars[i..].starts_with(&['/', 'd', 'e', 'v', '/', 'n', 'u', 'l', 'l']) {
                    i += 9;
                    if i < len && !matches!(chars[i], ' ' | '\t' | '|' | '&' | ';') {
                        return None;
                    }
                } else {
                    return None;
                }
            }
            '&' => {
                if i + 1 < len && chars[i + 1] == '&' {
                    flush_token(&mut current, &mut token, &mut has_token);
                    if current.is_empty() {
                        return None;
                    }
                    segments.push(std::mem::take(&mut current));
                    i += 2;
                } else if i + 1 < len && chars[i + 1] == '>' {
                    flush_token(&mut current, &mut token, &mut has_token);
                    i += 2;
                    while i < len && matches!(chars[i], ' ' | '\t') {
                        i += 1;
                    }
                    if chars[i..].starts_with(&['/', 'd', 'e', 'v', '/', 'n', 'u', 'l', 'l']) {
                        i += 9;
                        if i < len && !matches!(chars[i], ' ' | '\t' | '|' | '&' | ';') {
                            return None;
                        }
                    } else {
                        return None;
                    }
                } else {
                    return None;
                }
            }
            '|' => {
                flush_token(&mut current, &mut token, &mut has_token);
                if current.is_empty() {
                    return None;
                }
                segments.push(std::mem::take(&mut current));
                i += if i + 1 < len && chars[i + 1] == '|' {
                    2
                } else {
                    1
                };
            }
            ';' => {
                flush_token(&mut current, &mut token, &mut has_token);
                if current.is_empty() {
                    return None;
                }
                segments.push(std::mem::take(&mut current));
                i += 1;
            }
            _ => {
                has_token = true;
                token.push(c);
                i += 1;
            }
        }
    }

    flush_token(&mut current, &mut token, &mut has_token);
    if current.is_empty() {
        return None;
    }
    segments.push(current);
    Some(segments)
}

fn normalized_program(program: &str) -> Option<&str> {
    if program.is_empty() || program.contains('=') {
        return None;
    }
    if !program.contains('/') {
        return Some(program);
    }
    for prefix in ["/bin/", "/usr/bin/", "/usr/local/bin/", "/opt/homebrew/bin/"] {
        if let Some(name) = program.strip_prefix(prefix)
            && !name.is_empty()
            && !name.contains('/')
        {
            return Some(name);
        }
    }
    None
}

fn segment_is_read_only(words: &[String]) -> bool {
    let Some((first, args)) = words.split_first() else {
        return false;
    };
    let Some(program) = normalized_program(first) else {
        return false;
    };
    match program {
        "ls" | "ll" | "dir" | "tree" | "pwd" | "which" | "file" | "stat" | "wc" | "head"
        | "tail" | "cat" | "nl" | "tac" | "rev" | "cut" | "tr" | "uniq" | "diff" | "cmp"
        | "comm" | "basename" | "dirname" | "realpath" | "readlink" | "du" | "df" | "uname"
        | "echo" | "printf" | "true" | "false" | "bat" | "jq" | "grep" | "egrep" | "fgrep"
        | "ag" | "ack" => true,
        "rg" => !args.iter().any(|a| {
            matches!(a.as_str(), "--pre" | "--pre-glob")
                || a.starts_with("--pre=")
                || a.starts_with("--pre-glob=")
        }),
        "fd" | "fdfind" => !args.iter().any(|a| {
            matches!(a.as_str(), "-x" | "-X" | "--exec" | "--exec-batch")
                || a.starts_with("--exec=")
                || a.starts_with("--exec-batch=")
        }),
        "find" => !args.iter().any(|a| {
            matches!(
                a.as_str(),
                "-delete"
                    | "-exec"
                    | "-execdir"
                    | "-ok"
                    | "-okdir"
                    | "-fprint"
                    | "-fprint0"
                    | "-fprintf"
                    | "-fls"
            )
        }),
        "sort" => !args.iter().any(|a| {
            a == "-o"
                || a == "--output"
                || a.starts_with("--output=")
                || (a.starts_with('-') && !a.starts_with("--") && a[1..].contains('o'))
        }),
        "sed" => sed_is_read_only_print(args),
        "command" => matches!(args.first().map(String::as_str), Some("-v" | "-V")),
        "git" => git_is_read_only(args),
        _ => false,
    }
}

/// Allow strictly `sed -n '<range>p' <files...>` (e.g. `sed -n '1,80p' src/main.rs`).
fn sed_is_read_only_print(args: &[String]) -> bool {
    let mut i = 0;
    let mut quiet = false;
    while i < args.len() && args[i].starts_with('-') {
        match args[i].as_str() {
            "-n" | "--quiet" | "--silent" => {
                quiet = true;
                i += 1;
            }
            _ => return false,
        }
    }
    if !quiet {
        return false;
    }
    let Some(script) = args.get(i) else {
        return false;
    };
    let Some(range) = script.strip_suffix('p') else {
        return false;
    };
    !range.is_empty()
        && range
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ',' | '$' | ';'))
}

fn git_is_read_only(args: &[String]) -> bool {
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-C" => {
                if i + 1 >= args.len() {
                    return false;
                }
                i += 2;
            }
            "--no-pager" | "-P" | "--literal-pathspecs" | "--no-optional-locks" => {
                i += 1;
            }
            s if s.starts_with('-') => return false,
            _ => break,
        }
    }
    let Some(sub) = args.get(i).map(String::as_str) else {
        return false;
    };
    let rest = &args[i + 1..];
    match sub {
        "status" | "log" | "show" | "diff" | "grep" | "ls-files" | "ls-tree" | "rev-parse"
        | "rev-list" | "describe" | "blame" | "shortlog" | "cat-file" | "merge-base"
        | "name-rev" | "show-ref" | "for-each-ref" | "symbolic-ref" | "check-ignore"
        | "count-objects" => true,
        "branch" => {
            if rest.is_empty() {
                return true;
            }
            !rest.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "-d" | "-D"
                        | "-m"
                        | "-M"
                        | "-c"
                        | "-C"
                        | "-f"
                        | "--delete"
                        | "--move"
                        | "--copy"
                        | "--force"
                        | "--set-upstream-to"
                        | "--unset-upstream"
                        | "--edit-description"
                ) || a.starts_with("--set-upstream-to=")
            }) && rest.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "-a" | "--all"
                        | "-r"
                        | "--remotes"
                        | "-v"
                        | "-vv"
                        | "--verbose"
                        | "--list"
                        | "--show-current"
                        | "--contains"
                        | "--no-contains"
                        | "--merged"
                        | "--no-merged"
                ) || a.starts_with("--format=")
                    || a.starts_with("--sort=")
            })
        }
        "tag" => {
            rest.is_empty()
                || rest.iter().any(|a| matches!(a.as_str(), "-l" | "--list"))
                    && !rest.iter().any(|a| {
                        matches!(a.as_str(), "-d" | "--delete" | "-a" | "-s" | "-f" | "-m")
                    })
        }
        "remote" => {
            rest.is_empty()
                || matches!(
                    rest.first().map(String::as_str),
                    Some("-v" | "--verbose" | "show" | "get-url")
                )
        }
        "stash" => matches!(rest.first().map(String::as_str), Some("list" | "show")),
        "worktree" => matches!(rest.first().map(String::as_str), Some("list")),
        "config" => {
            rest.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "--get" | "--get-all" | "--list" | "-l" | "--show-origin" | "--show-scope"
                )
            }) && !rest.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "--unset" | "--unset-all" | "--add" | "--replace-all" | "--edit" | "-e"
                )
            })
        }
        _ => false,
    }
}

/// Tool result for anything that could change the workspace while plan mode is on.
pub const PLAN_MODE_REFUSAL: &str = "Plan mode is on: only read-only tools may run. Finish investigating and present the plan; the user will switch to implementation when they approve it.";

/// Whether plan mode lets `name` run: read-only tools only (MCP tools have unknown effects).
pub fn allowed_in_plan_mode(name: &str) -> bool {
    crate::agent::tools::tool_side_effect(name) == crate::agent::tools::ToolSideEffect::ReadOnly
}

/// Mediates user approval for mutating tool calls within a single agent run.
pub struct ApprovalGate {
    policy: ApprovalPolicy,
    auto_approve: bool,
    /// Refuse every non-read-only tool without asking (see [`PLAN_MODE_REFUSAL`]).
    plan_mode: bool,
    /// `bash` command prefixes that run without asking (see [`bash_command_allowlisted`]).
    bash_allowlist: Vec<String>,
    /// Tools the user allowed in this workspace.
    allowed_tools: Vec<String>,
    rx: Receiver<ApprovalDecision>,
}

impl ApprovalGate {
    pub fn new(policy: ApprovalPolicy, rx: Receiver<ApprovalDecision>) -> Self {
        Self {
            policy,
            auto_approve: false,
            plan_mode: false,
            bash_allowlist: Vec::new(),
            allowed_tools: Vec::new(),
            rx,
        }
    }

    pub fn with_allowed_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = tools;
        self
    }

    pub fn with_bash_allowlist(mut self, allowlist: Vec<String>) -> Self {
        self.bash_allowlist = allowlist;
        self
    }

    fn allowlisted(&self, name: &str, args: &Value) -> bool {
        tool_allowed(name, &self.allowed_tools)
            || name.eq_ignore_ascii_case("bash")
                && bash_command_arg(args)
                    .is_some_and(|command| bash_command_allowlisted(command, &self.bash_allowlist))
    }

    pub fn with_plan_mode(mut self, plan_mode: bool) -> Self {
        self.plan_mode = plan_mode;
        self
    }

    /// Block until the user decides. Returns `Ok(())` to proceed or `Err(reason)` to refuse,
    /// where `reason` is fed back to the model as the tool result. Cancellation is honored via
    /// `cancel` while waiting (polled so a stuck approval can't wedge the run).
    pub fn request(
        &mut self,
        tx: &Sender<AgentEvent>,
        cancel: &Arc<AtomicBool>,
        name: &str,
        args: &Value,
    ) -> Result<(), String> {
        if self.plan_mode {
            if allowed_in_plan_mode(name)
                || (name.eq_ignore_ascii_case("bash")
                    && bash_command_arg(args).is_some_and(bash_command_is_read_only))
            {
                return Ok(());
            }
            return Err(PLAN_MODE_REFUSAL.to_string());
        }
        if crate::evidence::tools::NAMES.contains(&name)
            && let Some(command) = crate::evidence::tools::command_args(name, args)
        {
            return self.request(tx, cancel, "bash", &command);
        }
        if self.auto_approve || !self.policy.requires_approval(name) || self.allowlisted(name, args)
        {
            return Ok(());
        }
        let _ = tx.send(AgentEvent::ApprovalRequest {
            name: name.to_string(),
            args: Some(args.clone()),
        });
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled before approval.".to_string());
            }
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(ApprovalDecision::Approve) => return Ok(()),
                Ok(ApprovalDecision::AllowPrefix(prefix)) => {
                    self.bash_allowlist.push(prefix);
                    return Ok(());
                }
                Ok(ApprovalDecision::AllowTool(tool)) => {
                    self.allowed_tools.push(tool);
                    return Ok(());
                }
                Ok(ApprovalDecision::ApproveRest) => {
                    self.auto_approve = true;
                    return Ok(());
                }
                Ok(ApprovalDecision::Deny | ApprovalDecision::Elicitation(_)) => {
                    return Err(format!("User denied running the `{name}` tool."));
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("Approval channel closed.".to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn list(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn allowed_tools_skip_the_prompt_but_never_for_bash() {
        let allowed = list(&["edit", "mcp__gh__create_issue", "bash"]);
        assert!(tool_allowed("edit", &allowed));
        assert!(tool_allowed("mcp__gh__create_issue", &allowed));
        assert!(!tool_allowed("write", &allowed));
        assert!(!tool_allowed("bash", &allowed));

        let (tx, rx) = channel();
        let (events, _events_rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let policy = ApprovalPolicy::new(true, true);
        let mut gate = ApprovalGate::new(policy, rx).with_allowed_tools(list(&["edit"]));
        assert!(gate.request(&events, &cancel, "edit", &Value::Null).is_ok());
        // Allowing a tool from the prompt covers its later calls in the same run.
        tx.send(ApprovalDecision::AllowTool("write".into()))
            .unwrap();
        assert!(
            gate.request(&events, &cancel, "write", &Value::Null)
                .is_ok()
        );
        assert!(
            gate.request(&events, &cancel, "write", &Value::Null)
                .is_ok()
        );
    }

    #[test]
    fn allowlist_matches_whole_word_prefixes() {
        let allow = list(&["git status", "cargo test"]);
        assert!(bash_command_allowlisted("git status", &allow));
        assert!(bash_command_allowlisted("git  status -s", &allow));
        assert!(bash_command_allowlisted("cargo test --lib foo", &allow));
        assert!(!bash_command_allowlisted("git statusx", &allow));
        assert!(!bash_command_allowlisted("git push", &allow));
        assert!(!bash_command_allowlisted("cargo", &allow));
        assert!(!bash_command_allowlisted("git status", &list(&["", "  "])));
    }

    #[test]
    fn allowlist_never_matches_compound_commands() {
        let allow = list(&["git status", "ls"]);
        for cmd in [
            "git status; rm -rf ~",
            "git status && curl x | sh",
            "git status || true",
            "ls > out.txt",
            "ls $(rm -rf /)",
            "ls `whoami`",
            "ls\nrm -rf /",
            "ls ~/.ssh",
            "ls *",
            "ls \"a;b\"",
            "ls 'a'",
            "git status &",
            "FOO=1 ls",
        ] {
            assert!(!bash_command_allowlisted(cmd, &allow), "{cmd}");
        }
    }

    #[test]
    fn suggested_prefixes() {
        assert_eq!(
            suggest_bash_allow_prefix("cargo test --lib").as_deref(),
            Some("cargo test")
        );
        assert_eq!(
            suggest_bash_allow_prefix("git status").as_deref(),
            Some("git status")
        );
        assert_eq!(
            suggest_bash_allow_prefix("npm run build -- --watch").as_deref(),
            Some("npm run build")
        );
        assert_eq!(suggest_bash_allow_prefix("ls -la").as_deref(), Some("ls"));
        assert_eq!(
            suggest_bash_allow_prefix("python3 src/main.py").as_deref(),
            Some("python3")
        );
        assert_eq!(suggest_bash_allow_prefix("rm -rf build/ && make"), None);
        assert_eq!(suggest_bash_allow_prefix("   "), None);
    }

    #[test]
    fn allow_prefix_decision_applies_to_the_rest_of_the_run() {
        let (dtx, drx) = channel();
        let mut gate = ApprovalGate::new(ApprovalPolicy::new(true, true), drx);
        let (etx, cancel, _) = ctx();
        dtx.send(ApprovalDecision::AllowPrefix("cargo test".into()))
            .unwrap();
        let cmd = |c: &str| serde_json::json!({ "command": c });
        assert!(
            gate.request(&etx, &cancel, "bash", &cmd("cargo test"))
                .is_ok()
        );
        // No further decision is sent: the next matching command must not block.
        assert!(
            gate.request(&etx, &cancel, "bash", &cmd("cargo test --lib"))
                .is_ok()
        );
    }

    #[test]
    fn allowlisted_bash_skips_the_prompt() {
        let (_dtx, drx) = channel();
        let mut gate = ApprovalGate::new(ApprovalPolicy::new(true, true), drx)
            .with_bash_allowlist(list(&["cargo test"]));
        let (etx, cancel, _) = ctx();
        // No decision is ever sent: an allowlisted command must not block.
        assert!(
            gate.request(
                &etx,
                &cancel,
                "bash",
                &serde_json::json!({"command": "cargo test -q"})
            )
            .is_ok()
        );
        // Other tools and compound commands still ask; with the sender alive but silent, a
        // cancelled run proves the gate waited instead of approving.
        cancel.store(true, Ordering::SeqCst);
        assert!(
            gate.request(
                &etx,
                &cancel,
                "bash",
                &serde_json::json!({"command": "cargo test; rm x"})
            )
            .is_err()
        );
        assert!(
            gate.request(
                &etx,
                &cancel,
                "write",
                &serde_json::json!({"command": "cargo test"})
            )
            .is_err()
        );
    }

    fn ctx() -> (Sender<AgentEvent>, Arc<AtomicBool>, Value) {
        let (etx, _erx) = channel();
        (etx, Arc::new(AtomicBool::new(false)), serde_json::json!({}))
    }

    #[test]
    fn readonly_tools_never_require_approval() {
        for t in ["read", "grep", "find", "ls"] {
            assert!(!ApprovalPolicy::new(true, true).requires_approval(t));
        }
    }

    #[test]
    fn mutating_tools_require_approval() {
        for t in [
            "bash",
            "write",
            "edit",
            "delete",
            "move",
            "mkdir",
            "scratchpad",
            "mcp_github_create_issue",
        ] {
            assert!(ApprovalPolicy::new(true, true).requires_approval(t));
        }
    }

    #[test]
    fn disabled_gate_always_proceeds() {
        let (_dtx, drx) = channel();
        let mut gate = ApprovalGate::new(ApprovalPolicy::disabled(), drx);
        let (etx, cancel, args) = ctx();
        assert!(gate.request(&etx, &cancel, "bash", &args).is_ok());
    }

    #[test]
    fn readonly_tool_bypasses_enabled_gate() {
        let (_dtx, drx) = channel();
        let mut gate = ApprovalGate::new(ApprovalPolicy::new(true, true), drx);
        let (etx, cancel, args) = ctx();
        // No decision is ever sent; a read tool must not block.
        assert!(gate.request(&etx, &cancel, "read", &args).is_ok());
    }

    #[test]
    fn approve_rest_auto_approves_subsequent_calls() {
        let (dtx, drx) = channel();
        let mut gate = ApprovalGate::new(ApprovalPolicy::new(true, true), drx);
        let (etx, cancel, args) = ctx();
        dtx.send(ApprovalDecision::ApproveRest).unwrap();
        assert!(gate.request(&etx, &cancel, "bash", &args).is_ok());
        // No second decision queued — auto-approve must short-circuit.
        assert!(gate.request(&etx, &cancel, "write", &args).is_ok());
    }

    #[test]
    fn deny_returns_error() {
        let (dtx, drx) = channel();
        let mut gate = ApprovalGate::new(ApprovalPolicy::new(true, true), drx);
        let (etx, cancel, args) = ctx();
        dtx.send(ApprovalDecision::Deny).unwrap();
        assert!(gate.request(&etx, &cancel, "bash", &args).is_err());
    }

    #[test]
    fn plan_mode_refuses_mutations_without_asking() {
        let (_dtx, drx) = channel();
        // Approval switched off entirely: plan mode must still refuse.
        let mut gate = ApprovalGate::new(ApprovalPolicy::disabled(), drx).with_plan_mode(true);
        let (etx, cancel, args) = ctx();
        for tool in [
            "write",
            "edit",
            "scratchpad",
            "bash",
            "diagnostics",
            "mcp_x_y",
        ] {
            assert_eq!(
                gate.request(&etx, &cancel, tool, &args),
                Err(PLAN_MODE_REFUSAL.to_string()),
                "{tool}"
            );
        }
        for tool in ["read", "grep", "todo_write", "task"] {
            assert!(gate.request(&etx, &cancel, tool, &args).is_ok(), "{tool}");
        }
        // Strictly read-only inspection commands may run in plan mode so agents without
        // dedicated grep/glob tools (e.g. Antigravity) can still search the workspace.
        assert!(
            gate.request(
                &etx,
                &cancel,
                "bash",
                &serde_json::json!({"CommandLine": "rg -n 'foo' src/ | head -20"})
            )
            .is_ok()
        );
        assert_eq!(
            gate.request(
                &etx,
                &cancel,
                "bash",
                &serde_json::json!({"CommandLine": "rm -rf target"})
            ),
            Err(PLAN_MODE_REFUSAL.to_string())
        );
    }

    #[test]
    fn read_only_bash_classifier_allows_safe_inspection_and_rejects_mutations() {
        for safe in [
            "ls -la /Users/manu/Projects/oxi",
            "rg -n \"spawn_workers|worker_status\" /Users/manu/Projects/oxi --glob '!target/**' -l",
            "ls -la /Users/manu/Projects/oxi && rg -i \"orchestrat\" --files-with-matches src 2>/dev/null || git grep -n \"orchestrat\"",
            "git status -s && git diff --stat",
            "git -C /tmp/repo log --oneline -20 | head -10",
            "git branch --show-current",
            "git worktree list",
            "find src -name '*.rs' | sort | wc -l",
            "sed -n '1,80p' src/main.rs",
            "cat src/lib.rs 2>&1 | grep -n mod",
            "/usr/bin/grep -rn foo src/",
            "command -v rg",
        ] {
            assert!(bash_command_is_read_only(safe), "should allow: {safe}");
        }
        for unsafe_cmd in [
            "",
            "   ",
            "rm -rf src/",
            "touch foo.txt",
            "ls > out.txt",
            "cat a >> b",
            "rg foo src/ > /tmp/out",
            "ls $(whoami)",
            "ls `whoami`",
            "ls\nrm -rf /",
            "git commit -m hi",
            "git checkout main",
            "git branch -D old",
            "git branch new-branch",
            "git -c core.pager=evil log",
            "find . -delete",
            "find . -exec rm {} +",
            "fd -x rm",
            "rg --pre sh foo",
            "sed -i 's/a/b/' file",
            "sed 's/a/b/e' file",
            "sort -o file file",
            "sort -ro file file",
            "cargo test",
            "python3 -c 'print(1)'",
            "PATH=/tmp ls",
            "ls &",
            "ls | ",
        ] {
            assert!(
                !bash_command_is_read_only(unsafe_cmd),
                "should reject: {unsafe_cmd}"
            );
        }
    }

    #[test]
    fn cancel_while_waiting_returns_error() {
        let (_dtx, drx) = channel();
        let mut gate = ApprovalGate::new(ApprovalPolicy::new(true, true), drx);
        let (etx, _erx) = channel();
        let cancel = Arc::new(AtomicBool::new(true)); // already cancelled
        let args = serde_json::json!({});
        assert!(gate.request(&etx, &cancel, "bash", &args).is_err());
    }

    #[test]
    fn from_settings_respects_per_oxi_tool_toggles() {
        let mut settings = crate::settings::AppSettings::default();
        let default_policy = ApprovalPolicy::from_settings(&settings);
        assert!(default_policy.requires_approval("spawn_workers"));
        assert!(default_policy.requires_approval("merge_worker"));
        assert!(!default_policy.requires_approval("team"));
        assert!(!default_policy.requires_approval("todo_write"));
        assert!(!default_policy.requires_approval("worker_status"));

        settings
            .oxi_tool_confirmation
            .insert("spawn_workers".into(), false);
        settings
            .oxi_tool_confirmation
            .insert("merge_worker".into(), false);
        settings.oxi_tool_confirmation.insert("team".into(), true);
        settings
            .oxi_tool_confirmation
            .insert("todo_write".into(), true);
        settings
            .oxi_tool_confirmation
            .insert("worker_status".into(), true);

        let policy = ApprovalPolicy::from_settings(&settings);
        assert!(!policy.requires_approval("spawn_workers"));
        assert!(!policy.requires_approval("merge_worker"));
        assert!(policy.requires_approval("team"));
        assert!(policy.requires_approval("todo_write"));
        assert!(policy.requires_approval("worker_status"));
        // Untouched oxi tools keep their defaults.
        assert!(policy.requires_approval("stop_worker"));
        assert!(!policy.requires_approval("worker_diff"));

        // Verify ApprovalGate honors toggled-off tools without blocking and prompts for toggled-on.
        let (_dtx, drx) = channel();
        let mut gate = ApprovalGate::new(policy, drx);
        let (etx, cancel, args) = ctx();
        assert!(gate.request(&etx, &cancel, "spawn_workers", &args).is_ok());
        assert!(gate.request(&etx, &cancel, "merge_worker", &args).is_ok());
        cancel.store(true, Ordering::SeqCst);
        assert!(gate.request(&etx, &cancel, "team", &args).is_err());
        assert!(gate.request(&etx, &cancel, "todo_write", &args).is_err());
        assert!(gate.request(&etx, &cancel, "worker_status", &args).is_err());
    }
}
