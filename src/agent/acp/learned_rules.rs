//! `/learn`: what the user got corrected on becomes rules every later chat starts with. The
//! command asks the agent to pick rules out of the chat; the ones the user keeps go through the
//! `learn_rules` tool (and its permission prompt) into `.oxi/rules/learned.md`, personal and out
//! of git through the repository's local exclude list. HTTP providers read that folder through their system prompt; ACP agents have their own
//! prompt, so a new connection's first message carries the rules.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub(super) const TOOL: &str = "learn_rules";
const RULES_DIR: &str = ".oxi/rules";
const RULES_FILE: &str = "learned.md";
/// Rules sent to an agent; the newest are kept beyond this.
const MAX_CHARS: usize = 16_000;

/// Where the rules of the project around `cwd` live. Every work tree of a repository shares the
/// main checkout's file: it's ignored by git, so it wouldn't reach them otherwise.
fn rules_path(cwd: &Path) -> PathBuf {
    let root = git2::Repository::discover(cwd)
        .ok()
        .and_then(|repo| Some(repo.commondir().parent()?.to_path_buf()))
        .unwrap_or_else(|| cwd.to_path_buf());
    root.join(RULES_DIR).join(RULES_FILE)
}

/// Keep the rules out of commits, in the repository's local exclude list: a `.gitignore` of ours
/// would itself turn up in the user's next `git add .`.
fn exclude_from_git(cwd: &Path) -> Result<(), String> {
    let Ok(repo) = git2::Repository::discover(cwd) else {
        return Ok(());
    };
    let exclude = repo.commondir().join("info/exclude");
    let line = format!("/{RULES_DIR}/{RULES_FILE}");
    let listed = std::fs::read_to_string(&exclude).unwrap_or_default();
    if listed.lines().any(|l| l.trim() == line) {
        return Ok(());
    }
    if let Some(dir) = exclude.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let lead = if listed.is_empty() || listed.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    crate::fsutil::write_atomic(&exclude, format!("{listed}{lead}{line}\n").as_bytes())
}

fn read(cwd: &Path) -> String {
    std::fs::read_to_string(rules_path(cwd)).unwrap_or_default()
}

/// Add `rules` that aren't saved yet, one line each; returns how many were new.
fn save(cwd: &Path, rules: &[String]) -> Result<usize, String> {
    let path = rules_path(cwd);
    let dir = path.parent().ok_or("No folder for the rules file.")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    exclude_from_git(cwd)?;
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut added = 0;
    for rule in rules {
        let rule = rule.split_whitespace().collect::<Vec<_>>().join(" ");
        let rule = rule.trim_start_matches(['-', '*', ' ']);
        let known = text
            .lines()
            .any(|line| line.trim_start_matches("- ") == rule);
        if rule.is_empty() || known {
            continue;
        }
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("- {rule}\n"));
        added += 1;
    }
    if added > 0 {
        crate::fsutil::write_atomic(&path, text.as_bytes())?;
    }
    Ok(added)
}

pub(super) fn definition() -> Value {
    json!({
        "name": TOOL,
        "description": "Save rules for this project that every later oxi chat starts with, with any agent. Call it only with rules the user picked after you showed them (see /learn), never on your own. Each rule is one short, checkable line.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "rules": { "type": "array", "items": { "type": "string" }, "minItems": 1 }
            },
            "required": ["rules"]
        }
    })
}

pub(super) fn call(args: &Value) -> Result<String, String> {
    let dir = crate::evidence::tools::conn_dir()
        .ok_or("learn_rules is only available in an oxi chat.")?;
    let cwd = super::chat_archive::origin(&dir)
        .1
        .ok_or("oxi has not said which folder this chat is in.")?;
    let rules: Vec<String> = args["rules"]
        .as_array()
        .ok_or("Missing array `rules`.")?
        .iter()
        .filter_map(|rule| rule.as_str().map(str::to_owned))
        .collect();
    let added = save(Path::new(&cwd), &rules)?;
    Ok(format!(
        "Saved {added} new rule(s) in {}; later chats start with them.",
        rules_path(Path::new(&cwd)).display()
    ))
}

/// The prompt behind `/learn` and `/learn <focus>`; `None` for any other message.
pub(super) fn expand(text: &str, cwd: &Path) -> Option<String> {
    let focus = text.trim().strip_prefix("/learn")?;
    if !focus.is_empty() && !focus.starts_with(char::is_whitespace) {
        return None;
    }
    let mut prompt = String::from(
        "The user ran /learn: turn what they were corrected on into lasting project rules.\n\
         1. Find the corrections in this conversation: review comments they pasted, mistakes they \
         pointed out, fixes they asked for. Read earlier turns with chat_read if you lack them.\n\
         2. Keep only what can happen again elsewhere in this project. One rule per mistake: one \
         short, checkable line saying what to do, without the story. Skip one-off fixes, personal \
         taste of the moment, and whatever the project's instructions or the saved rules already say.\n\
         3. Show the candidates numbered, each with the comment it came from, in the user's \
         language, and ask which to keep. Change nothing and save nothing yet; end your turn. If \
         nothing qualifies, say so and stop.\n\
         4. Once the user has picked, call oxi's learn_rules tool with exactly those rules.",
    );
    let focus = focus.trim();
    if !focus.is_empty() {
        prompt.push_str(&format!("\n\nThe user added: {focus}"));
    }
    let saved = read(cwd);
    if !saved.trim().is_empty() {
        prompt.push_str(&format!("\n\nRules already saved:\n{}", saved.trim()));
    }
    Some(prompt)
}

/// `text` with the project's learned rules ahead of it, when it has any.
pub(super) fn with_rules(cwd: &Path, text: &str) -> String {
    let rules = read(cwd);
    let rules = rules.trim();
    if rules.is_empty() {
        return text.to_string();
    }
    let skip = rules.chars().count().saturating_sub(MAX_CHARS);
    let start = rules.char_indices().nth(skip).map_or(0, |(at, _)| at);
    let rules = &rules[start..];
    format!(
        "<project_rules>\nRules the user set for this project after earlier corrections. Follow them, and check your work against them before you call it done.\n{rules}\n</project_rules>\n\n{text}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = crate::fsutil::unique_temp_path(&std::env::temp_dir(), "oxi-learned", "d");
        std::fs::create_dir_all(&dir).unwrap();
        // macOS hands out /var symlinks; git reports the real path.
        dir.canonicalize().unwrap()
    }

    #[test]
    fn saves_new_rules_once() {
        let dir = temp();
        let rules = vec![
            "Title PRs as  feat(scope): summary".to_string(),
            "- Add a test with every fix".to_string(),
            "   ".to_string(),
        ];
        assert_eq!(save(&dir, &rules).unwrap(), 2);
        assert_eq!(save(&dir, &rules).unwrap(), 0);
        assert_eq!(
            read(&dir),
            "- Title PRs as feat(scope): summary\n- Add a test with every fix\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_rules_leave_no_trace_in_git_status() {
        let dir = temp();
        let repo = git2::Repository::init(&dir).unwrap();
        let exclude = dir.join(".git/info/exclude");
        std::fs::write(&exclude, "*.log").unwrap();
        save(&dir, &["A rule".to_string()]).unwrap();
        save(&dir, &["Another rule".to_string()]).unwrap();
        assert_eq!(
            std::fs::read_to_string(&exclude).unwrap(),
            "*.log\n/.oxi/rules/learned.md\n"
        );
        let mut options = git2::StatusOptions::new();
        options.include_untracked(true).recurse_untracked_dirs(true);
        assert!(repo.statuses(Some(&mut options)).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_subfolder_and_a_work_tree_share_the_main_checkouts_rules() {
        let dir = temp();
        let main = dir.join("main");
        std::fs::create_dir_all(main.join("src")).unwrap();
        let repo = git2::Repository::init(&main).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let tree = repo.treebuilder(None).unwrap().write().unwrap();
        let tree = repo.find_tree(tree).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        repo.worktree("wt", &dir.join("wt"), None).unwrap();

        // git2 spells Windows paths with forward slashes, so compare the folders as the OS sees them.
        std::fs::create_dir_all(main.join(RULES_DIR)).unwrap();
        let resolved = |cwd: &Path| {
            let path = rules_path(cwd);
            assert_eq!(path.file_name().unwrap(), RULES_FILE);
            std::fs::canonicalize(path.parent().unwrap()).unwrap()
        };
        let expected = std::fs::canonicalize(main.join(RULES_DIR)).unwrap();
        assert_eq!(resolved(&main), expected);
        assert_eq!(resolved(&main.join("src")), expected);
        assert_eq!(resolved(&dir.join("wt")), expected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expands_only_the_command_and_lists_saved_rules() {
        let dir = temp();
        assert!(expand("/learnt something", &dir).is_none());
        assert!(expand("please /learn", &dir).is_none());
        let bare = expand("  /learn ", &dir).unwrap();
        assert!(bare.contains("learn_rules") && !bare.contains("already saved"));

        save(&dir, &["Run the linter".to_string()]).unwrap();
        let focused = expand("/learn only naming", &dir).unwrap();
        assert!(focused.contains("The user added: only naming"), "{focused}");
        assert!(focused.contains("- Run the linter"), "{focused}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rules_go_ahead_of_the_message_and_the_newest_survive_the_cap() {
        let dir = temp();
        assert_eq!(with_rules(&dir, "hi"), "hi");
        save(&dir, &["Old ă rule".to_string()]).unwrap();
        assert!(with_rules(&dir, "hi").contains("- Old ă rule\n</project_rules>\n\nhi"));

        let long = "x".repeat(MAX_CHARS);
        save(&dir, &[long, "Newest rule".to_string()]).unwrap();
        let got = with_rules(&dir, "hi");
        assert!(got.contains("Newest rule") && !got.contains("Old ă rule"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
