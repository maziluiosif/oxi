mod dedupe;
mod format;
pub(crate) use format::chat_message_to_json_entries;
mod io;
mod paths;
pub mod run_journal;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::model::{Session, make_session_title};

#[cfg(test)]
pub use dedupe::chat_messages_equal;
#[cfg(test)]
pub use io::load_session_messages;
pub use io::{load_session_messages_with_wire, save_session_messages};
use paths::{agent_dir, configured_session_dir, default_session_dir};

pub fn load_workspace_sessions(root_path: &str) -> Vec<Session> {
    let root = Path::new(root_path);
    load_workspace_sessions_from(root, &agent_dir())
}

/// The folder holding this workspace's session files.
pub fn workspace_session_dir(root_path: &str) -> PathBuf {
    let root = Path::new(root_path);
    let agent_dir = agent_dir();
    configured_session_dir(root, &agent_dir)
        .unwrap_or_else(|| default_session_dir(root, &agent_dir))
}

/// The session files in `dir` with their mtimes, `None` when the folder can't be read. Cheap
/// enough to poll: one directory read, no file is opened.
pub fn list_session_files(dir: &Path) -> Option<Vec<(String, SystemTime)>> {
    let entries = fs::read_dir(dir).ok()?;
    let files = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((entry.path().to_string_lossy().to_string(), modified))
        })
        .collect();
    Some(files)
}

/// Folders that have chats in the default session store, newest chat first, with that
/// chat's mtime and the number of chats. The folder is the `cwd` in the newest file's header
/// when it has one, else the existing folder the session folder's name was made from.
pub fn workspaces_with_chats() -> Vec<(String, SystemTime, usize)> {
    let Ok(dirs) = fs::read_dir(agent_dir().join("sessions")) else {
        return Vec::new();
    };
    let mut out: Vec<(String, SystemTime, usize)> = dirs
        .filter_map(Result::ok)
        .filter_map(|dir| {
            let files = list_session_files(&dir.path())?;
            let (newest, modified) = files.iter().max_by_key(|(_, m)| *m)?.clone();
            let root = header_cwd(Path::new(&newest)).or_else(|| {
                let name = dir.file_name().to_string_lossy().to_string();
                folder_from_session_dir_name(&name, Path::new("/"))
            })?;
            Some((root, modified, files.len()))
        })
        .collect();
    out.sort_by_key(|(_, modified, _)| std::cmp::Reverse(*modified));
    out
}

fn header_cwd(file: &Path) -> Option<String> {
    let file = fs::File::open(file).ok()?;
    let mut header = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(file), &mut header).ok()?;
    let header: serde_json::Value = serde_json::from_str(&header).ok()?;
    header["cwd"].as_str().map(str::to_string)
}

/// Undo [`paths::default_session_dir`]'s naming (`--Users-me-my-app--`), which turned the
/// separators into dashes: the existing folder under `base` whose path gives that name.
fn folder_from_session_dir_name(name: &str, base: &Path) -> Option<String> {
    let inner = name.strip_prefix("--")?.strip_suffix("--")?;
    let parts: Vec<&str> = inner.split('-').collect();
    fn walk(dir: &Path, parts: &[&str]) -> Option<PathBuf> {
        if parts.is_empty() {
            return Some(dir.to_path_buf());
        }
        // Longest component first: `my-app` before `my/app`.
        (1..=parts.len()).rev().find_map(|n| {
            let next = dir.join(parts[..n].join("-"));
            next.is_dir().then(|| walk(&next, &parts[n..])).flatten()
        })
    }
    walk(base, &parts).map(|p| p.to_string_lossy().to_string())
}

fn load_workspace_sessions_from(root_path: &Path, agent_dir: &Path) -> Vec<Session> {
    let session_dir = configured_session_dir(root_path, agent_dir)
        .unwrap_or_else(|| default_session_dir(root_path, agent_dir));
    let Ok(entries) = fs::read_dir(&session_dir) else {
        return Vec::new();
    };

    let paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .collect();
    let mut sessions = parse_session_files(&paths);

    sessions.sort_by_key(|b| std::cmp::Reverse(b.modified));

    sessions.into_iter().map(unloaded_session).collect()
}

/// A sidebar entry for one session file: title and config only, the transcript is read when
/// the chat is opened.
pub fn load_session_summary(path: &str) -> Option<Session> {
    parse_session_file(Path::new(path)).map(unloaded_session)
}

fn unloaded_session(session: LoadedSession) -> Session {
    Session {
        delegated: Default::default(),
        title: session.title,
        messages: Vec::new(),
        config: session.config,
        session_file: Some(session.path),
        messages_loaded: false,
        input_text: String::new(),
        pending_images: Vec::new(),
        pending_texts: Vec::new(),
        modified: session.modified,
        chars_per_token: None,
        agent_context: None,
        wire_cache: None,
        disk_mtime: Some(session.modified),
    }
}

/// One chat read from its session file, transcript and provider cache included.
pub fn load_session(path: &str) -> Option<Session> {
    let summary = parse_session_file(Path::new(path))?;
    let (loaded, wire_cache) = load_session_messages_with_wire(path)?;
    Some(Session {
        delegated: loaded.delegated,
        title: summary.title,
        messages: loaded.messages,
        config: loaded.config.or(summary.config),
        session_file: Some(summary.path),
        messages_loaded: true,
        input_text: String::new(),
        pending_images: Vec::new(),
        pending_texts: Vec::new(),
        modified: summary.modified,
        chars_per_token: None,
        agent_context: None,
        wire_cache,
        disk_mtime: Some(summary.modified),
    })
}

struct LoadedSession {
    path: String,
    title: String,
    config: Option<crate::model::SessionConfig>,
    modified: SystemTime,
}

/// Summarise session files on a few threads: startup lists every chat, and a busy workspace
/// holds hundreds of files that are independent of each other.
fn parse_session_files(paths: &[PathBuf]) -> Vec<LoadedSession> {
    const MIN_PER_THREAD: usize = 16;
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(paths.len().div_ceil(MIN_PER_THREAD))
        .max(1);
    if threads == 1 {
        return paths.iter().filter_map(|p| parse_session_file(p)).collect();
    }
    let chunk = paths.len().div_ceil(threads);
    std::thread::scope(|scope| {
        let workers: Vec<_> = paths
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .filter_map(|p| parse_session_file(p))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap_or_default())
            .collect()
    })
}

fn parse_session_file(path: &Path) -> Option<LoadedSession> {
    let summary = io::scan_session_summary(path)?;
    let config = summary.config;
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let title = summary
        .name
        .or_else(|| {
            summary
                .first_user_message
                .map(|text| make_session_title(&text))
        })
        .unwrap_or_else(|| "New chat".to_string());

    Some(LoadedSession {
        path: path.to_string_lossy().to_string(),
        title,
        config,
        modified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hydrate;
    use crate::model::{
        AssistantBlock, ChatMessage, MsgRole, Session, SessionConfig, UserAttachment, WireCache,
    };
    use crate::settings::LlmProviderKind;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn session_dir_names_map_back_to_existing_folders() {
        let base = temp_root("dirname");
        fs::create_dir_all(base.join("Users/me/my-app/src")).unwrap();
        fs::create_dir_all(base.join("Users/me/notes")).unwrap();
        let found = |name: &str| folder_from_session_dir_name(name, &base).map(PathBuf::from);
        assert_eq!(
            found("--Users-me-my-app--"),
            Some(base.join("Users/me/my-app"))
        );
        assert_eq!(
            found("--Users-me-my-app-src--"),
            Some(base.join("Users/me/my-app/src"))
        );
        assert_eq!(
            found("--Users-me-notes--"),
            Some(base.join("Users/me/notes"))
        );
        assert_eq!(found("--Users-me-gone--"), None);
        assert_eq!(found("not-a-session-dir"), None);
        let _ = fs::remove_dir_all(base);
    }

    /// A throwaway workspace whose chats are stored inside it (via `.pi/settings.json`), so
    /// tests never write into the real `~/.config/oxi/sessions`.
    fn temp_root(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_nanos();
        let path = std::env::temp_dir().join(format!("oxi-{name}-{nanos}"));
        fs::create_dir_all(path.join(".pi")).unwrap();
        fs::write(
            path.join(".pi/settings.json"),
            r#"{"sessionDir": "sessions"}"#,
        )
        .unwrap();
        path
    }

    #[test]
    fn dedupe_trailing_duplicate_messages_removes_repeated_history() {
        let user = ChatMessage {
            role: MsgRole::User,
            text: "hello".into(),
            is_summary: false,
            attachments: vec![UserAttachment::Image {
                mime: "image/png".into(),
                data: vec![1, 2, 3],
            }],
            blocks: vec![],
            streaming: false,
            started_at: None,
            worked_duration: None,
            route: None,
            changes: None,
            evidence: None,
        };
        let assistant = ChatMessage {
            role: MsgRole::Assistant,
            text: String::new(),
            is_summary: false,
            attachments: vec![],
            blocks: vec![
                AssistantBlock::Thinking("plan".into()),
                AssistantBlock::Tool {
                    tool_call_id: "call_1".into(),
                    name: "bash".into(),
                    args_summary: Some("{\"command\":\"pwd\"}".into()),
                    output: "/tmp\n".into(),
                    diff: None,
                    is_error: Some(false),
                    full_output_path: Some("/tmp/out.txt".into()),
                    output_truncated: false,
                    metadata: None,
                },
                AssistantBlock::Answer("done".into()),
            ],
            streaming: false,
            started_at: None,
            worked_duration: None,
            route: None,
            changes: None,
            evidence: None,
        };
        let mut messages = vec![user.clone(), assistant.clone(), user, assistant];

        crate::session_store::dedupe::dedupe_trailing_duplicate_messages(&mut messages);

        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn dedupe_trailing_duplicate_messages_keeps_distinct_history() {
        let mut messages = vec![
            ChatMessage {
                role: MsgRole::User,
                text: "hello".into(),
                is_summary: false,
                attachments: vec![],
                blocks: vec![],
                streaming: false,
                started_at: None,
                worked_duration: None,
                route: None,
                changes: None,
                evidence: None,
            },
            ChatMessage {
                role: MsgRole::Assistant,
                text: String::new(),
                is_summary: false,
                attachments: vec![],
                blocks: vec![AssistantBlock::Answer("first".into())],
                streaming: false,
                started_at: None,
                worked_duration: None,
                route: None,
                changes: None,
                evidence: None,
            },
            ChatMessage {
                role: MsgRole::User,
                text: "hello".into(),
                is_summary: false,
                attachments: vec![],
                blocks: vec![],
                streaming: false,
                started_at: None,
                worked_duration: None,
                route: None,
                changes: None,
                evidence: None,
            },
            ChatMessage {
                role: MsgRole::Assistant,
                text: String::new(),
                is_summary: false,
                attachments: vec![],
                blocks: vec![AssistantBlock::Answer("second".into())],
                streaming: false,
                started_at: None,
                worked_duration: None,
                route: None,
                changes: None,
                evidence: None,
            },
        ];

        crate::session_store::dedupe::dedupe_trailing_duplicate_messages(&mut messages);

        assert_eq!(messages.len(), 4);
    }

    #[test]
    fn save_and_reload_preserves_wire_cache() {
        let root = temp_root("wire-cache-roundtrip");
        let mut session = Session {
            delegated: Default::default(),
            title: "Chat".into(),
            messages: vec![],
            config: None,
            session_file: None,
            messages_loaded: true,
            input_text: String::new(),
            pending_images: Vec::new(),
            pending_texts: Vec::new(),
            chars_per_token: None,
            agent_context: None,
            wire_cache: Some(WireCache {
                fingerprint: "v1:sha256:abcd".into(),
                messages: vec![json!({"role":"system","content":"s"})],
            }),
            modified: SystemTime::now(),
            disk_mtime: None,
        };

        save_session_messages(root.to_str().unwrap(), &mut session).unwrap();
        let (_, loaded) =
            load_session_messages_with_wire(session.session_file.as_deref().unwrap()).unwrap();
        let loaded = loaded.unwrap();
        assert_eq!(loaded.fingerprint, "v1:sha256:abcd");
        assert_eq!(
            loaded.messages,
            vec![json!({"role":"system","content":"s"})]
        );
    }

    #[test]
    fn save_and_reload_preserves_session_config() {
        let root = temp_root("session-config-roundtrip");
        let config = SessionConfig {
            provider: LlmProviderKind::OpenRouter,
            model_id: "anthropic/claude-sonnet-5".into(),
            effort: "high".into(),
            context_window: Some(200_000),
        };
        let mut session = Session {
            delegated: Default::default(),
            title: "Chat".into(),
            messages: vec![],
            config: Some(config.clone()),
            session_file: None,
            messages_loaded: true,
            input_text: String::new(),
            pending_images: Vec::new(),
            pending_texts: Vec::new(),
            chars_per_token: None,
            agent_context: None,
            wire_cache: None,
            disk_mtime: None,
            modified: SystemTime::now(),
        };

        save_session_messages(root.to_str().unwrap(), &mut session).unwrap();
        let (loaded, _) =
            load_session_messages_with_wire(session.session_file.as_deref().unwrap()).unwrap();

        assert_eq!(loaded.config, Some(config));
    }

    #[test]
    fn save_session_messages_dedupes_duplicate_full_history_before_writing() {
        let root = temp_root("save-dedupe");
        let mut session = Session {
            delegated: Default::default(),
            title: "Chat".into(),
            config: None,
            messages: hydrate::messages_from_get_messages(&json!({
                "messages": [
                    { "role": "user", "content": "hello" },
                    { "role": "assistant", "content": [{ "type": "text", "text": "hi" }] },
                    { "role": "user", "content": "hello" },
                    { "role": "assistant", "content": [{ "type": "text", "text": "hi" }] }
                ]
            })),
            session_file: None,
            messages_loaded: true,
            input_text: String::new(),
            pending_images: Vec::new(),
            pending_texts: Vec::new(),
            chars_per_token: None,
            agent_context: None,
            wire_cache: None,
            disk_mtime: None,
            modified: SystemTime::now(),
        };

        save_session_messages(root.to_str().unwrap(), &mut session).unwrap();

        assert_eq!(session.messages.len(), 2);
        let loaded = load_session_messages(session.session_file.as_deref().unwrap()).unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(chat_messages_equal(&loaded[0], &session.messages[0]));
        assert!(chat_messages_equal(&loaded[1], &session.messages[1]));
    }

    #[test]
    fn save_and_reload_preserves_assistant_block_formatting() {
        let root = temp_root("formatting-roundtrip");
        let mut session = Session {
            delegated: Default::default(),
            title: "Chat".into(),
            config: None,
            messages: vec![
                ChatMessage {
                    role: MsgRole::User,
                    text: "look".into(),
                    is_summary: false,
                    attachments: vec![UserAttachment::Image {
                        mime: "image/png".into(),
                        data: vec![1, 2, 3, 4],
                    }],
                    blocks: vec![],
                    streaming: false,
                    started_at: None,
                    worked_duration: None,
                    route: None,
                    changes: None,
                    evidence: None,
                },
                ChatMessage {
                    role: MsgRole::Assistant,
                    text: String::new(),
                    is_summary: false,
                    attachments: vec![],
                    blocks: vec![
                        AssistantBlock::Thinking("thinking".into()),
                        AssistantBlock::Tool {
                            tool_call_id: "call_123".into(),
                            name: "edit".into(),
                            args_summary: Some("{\"path\":\"src/main.rs\"}".into()),
                            output: "changed file".into(),
                            diff: Some("+new\n-old".into()),
                            is_error: Some(false),
                            full_output_path: Some("/tmp/tool-output.txt".into()),
                            output_truncated: true,
                            metadata: None,
                        },
                        AssistantBlock::Answer("done".into()),
                    ],
                    streaming: false,
                    started_at: None,
                    worked_duration: None,
                    route: None,
                    changes: None,
                    evidence: None,
                },
            ],
            session_file: None,
            messages_loaded: true,
            input_text: String::new(),
            pending_images: Vec::new(),
            pending_texts: Vec::new(),
            chars_per_token: None,
            agent_context: None,
            wire_cache: None,
            disk_mtime: None,
            modified: SystemTime::now(),
        };

        save_session_messages(root.to_str().unwrap(), &mut session).unwrap();
        let loaded = load_session_messages(session.session_file.as_deref().unwrap()).unwrap();

        assert_eq!(loaded.len(), session.messages.len());
        assert!(chat_messages_equal(&loaded[0], &session.messages[0]));
        assert!(chat_messages_equal(&loaded[1], &session.messages[1]));
    }

    #[test]
    fn save_and_reload_preserves_worked_duration() {
        // The collapsed "Worked for Xs" summary depends on `worked_duration` surviving
        // a save/reload round-trip (the live `started_at` Instant cannot be persisted).
        let root = temp_root("worked-duration-roundtrip");
        let mut session = Session {
            delegated: Default::default(),
            title: "Chat".into(),
            config: None,
            messages: vec![
                ChatMessage {
                    role: MsgRole::User,
                    text: "hello".into(),
                    is_summary: false,
                    attachments: vec![],
                    blocks: vec![],
                    streaming: false,
                    started_at: None,
                    worked_duration: None,
                    route: None,
                    changes: None,
                    evidence: None,
                },
                ChatMessage {
                    role: MsgRole::Assistant,
                    text: String::new(),
                    is_summary: false,
                    attachments: vec![],
                    blocks: vec![AssistantBlock::Answer("hi there".into())],
                    streaming: false,
                    started_at: None,
                    worked_duration: Some(Duration::from_secs(67)),
                    route: None,
                    changes: None,
                    evidence: None,
                },
            ],
            session_file: None,
            messages_loaded: true,
            input_text: String::new(),
            pending_images: Vec::new(),
            pending_texts: Vec::new(),
            chars_per_token: None,
            agent_context: None,
            wire_cache: None,
            disk_mtime: None,
            modified: SystemTime::now(),
        };

        save_session_messages(root.to_str().unwrap(), &mut session).unwrap();
        let loaded = load_session_messages(session.session_file.as_deref().unwrap()).unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[1].worked_duration, Some(Duration::from_secs(67)));
    }
}
