//! Searching what was said in every chat, not only in the ones already loaded. The sidebar's
//! search box filters by title and loaded messages right away; this finds the rest by reading
//! the chat files on a background thread (a pause in typing starts it, a newer query cancels
//! it), and gives each chat that matches a one-line excerpt around its first match.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use super::OxiApp;

/// Shorter queries match nearly everything; the in-memory filter covers them.
const MIN_QUERY: usize = 2;
/// Typing bursts start one search once they pause for this long.
const SEARCH_AFTER: Duration = Duration::from_millis(250);
/// Characters of context kept on each side of a match.
const CONTEXT: usize = 36;

/// The search over chat files for the sidebar's query.
#[derive(Default)]
pub(crate) struct ContentSearch {
    /// The query the hits are for (lowercased, trimmed).
    query: String,
    /// When the query last changed, until its search starts.
    changed_at: Option<Instant>,
    /// Bumped for every search; a running one stops once it is no longer current.
    generation: Arc<AtomicU64>,
    rx: Option<Receiver<HashMap<String, String>>>,
    /// Chat file → excerpt around its first match.
    pub hits: HashMap<String, String>,
    pub running: bool,
}

/// The text of a `message` line of a chat file: its user text, or its answer and thinking.
fn message_text(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value["type"].as_str() != Some("message") {
        return None;
    }
    let content = &value["message"]["content"];
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    let parts: Vec<&str> = content
        .as_array()?
        .iter()
        .filter_map(|part| part["text"].as_str().or_else(|| part["thinking"].as_str()))
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// One line of `text` around the first case-insensitive match of `query` (lowercased), with
/// an ellipsis where it was cut.
pub(crate) fn excerpt(text: &str, query: &str) -> Option<String> {
    let lower = text.to_lowercase();
    // Lowercasing can change byte lengths; map back through char positions.
    let at = lower.find(query)?;
    let char_at = lower[..at].chars().count();
    let chars: Vec<char> = text.chars().collect();
    let start = char_at.saturating_sub(CONTEXT);
    let end = (char_at + query.chars().count() + CONTEXT).min(chars.len());
    let mut out: String = chars[start..end]
        .iter()
        .map(|c| if c.is_whitespace() { ' ' } else { *c })
        .collect();
    out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        out.insert(0, '…');
    }
    if end < chars.len() {
        out.push('…');
    }
    Some(out)
}

/// The excerpt of the first message of chat file `path` that contains `query`.
fn search_file(path: &str, query: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    content
        .lines()
        // Cheap pre-filter: only lines that mention the query somewhere get parsed.
        .filter(|line| line.contains("\"type\":\"message\"") && line.to_lowercase().contains(query))
        .find_map(|line| message_text(line).and_then(|text| excerpt(&text, query)))
}

impl OxiApp {
    /// Once per frame: start a search when the sidebar's query settles, take finished results.
    pub(crate) fn poll_content_search(&mut self, ctx: &eframe::egui::Context) {
        let query = self.conv.sidebar.search.trim().to_lowercase();
        let search = &mut self.content_search;
        if let Some(rx) = search.rx.as_ref() {
            match rx.try_recv() {
                Ok(hits) => {
                    search.hits = hits;
                    search.rx = None;
                    search.running = false;
                }
                Err(TryRecvError::Disconnected) => {
                    search.rx = None;
                    search.running = false;
                }
                Err(TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(100)),
            }
        }
        if query == search.query {
            search.changed_at = None;
            return;
        }
        if query.chars().count() < MIN_QUERY {
            search.query = query;
            search.hits.clear();
            search.rx = None;
            search.running = false;
            search.generation.fetch_add(1, Ordering::SeqCst);
            return;
        }
        let changed_at = *search.changed_at.get_or_insert_with(Instant::now);
        if changed_at.elapsed() < SEARCH_AFTER {
            ctx.request_repaint_after(SEARCH_AFTER - changed_at.elapsed());
            return;
        }
        search.changed_at = None;
        search.query = query.clone();
        let generation = search.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let current = search.generation.clone();
        let files: Vec<String> = self
            .conv
            .workspaces
            .iter()
            .flat_map(|w| w.sessions.iter().filter_map(|s| s.session_file.clone()))
            .collect();
        let (tx, rx) = std::sync::mpsc::channel();
        let search = &mut self.content_search;
        search.rx = Some(rx);
        search.running = true;
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new()
            .name("oxi-chat-search".into())
            .spawn(move || {
                let mut hits = HashMap::new();
                for file in files {
                    if current.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    if let Some(excerpt) = search_file(&file, &query) {
                        hits.insert(file, excerpt);
                    }
                }
                let _ = tx.send(hits);
                ctx.request_repaint();
            });
    }

    /// The excerpt of chat `(wi, si)`'s first match of `query` (the sidebar's, lowercased and
    /// trimmed), once its search finished.
    pub(crate) fn content_search_hit(&self, wi: usize, si: usize, query: &str) -> Option<&str> {
        if self.content_search.query != query || self.content_search.running {
            return None;
        }
        let file = self.conv.workspaces[wi].sessions[si]
            .session_file
            .as_ref()?;
        self.content_search.hits.get(file).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_excerpt_keeps_the_match_with_some_context_on_one_line() {
        let text = format!(
            "{}\nthe Retry budget\nis spent {}",
            "a".repeat(60),
            "b".repeat(60)
        );
        let got = excerpt(&text, "retry budget").unwrap();
        assert!(got.starts_with('…') && got.ends_with('…'), "{got}");
        assert!(got.contains("the Retry budget is spent"), "{got}");
        assert!(!got.contains('\n'));
        assert_eq!(excerpt("short", "short").as_deref(), Some("short"));
        assert_eq!(
            excerpt("Știrile de azi", "știrile").as_deref(),
            Some("Știrile de azi")
        );
        assert!(excerpt("nothing", "else").is_none());
    }

    #[test]
    fn chat_files_are_searched_in_messages_only() {
        let dir = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-chat-search", "d");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("s.jsonl");
        std::fs::write(
            &file,
            concat!(
                r#"{"name":"zebra title","type":"session_info"}"#,
                "\n",
                r#"{"message":{"content":"hello","role":"user"},"type":"message"}"#,
                "\n",
                r#"{"message":{"content":[{"thinking":"hmm","type":"thinking"},{"text":"The Zebra crossing","type":"text"}],"role":"assistant"},"type":"message"}"#,
                "\n",
            ),
        )
        .unwrap();
        let path = file.to_string_lossy();
        assert_eq!(
            search_file(&path, "zebra").as_deref(),
            Some("hmm The Zebra crossing")
        );
        assert_eq!(
            search_file(&path, "hmm").as_deref(),
            Some("hmm The Zebra crossing")
        );
        assert!(search_file(&path, "title").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
