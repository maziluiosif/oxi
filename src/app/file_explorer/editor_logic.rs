//! Pure editor buffer helpers shared by rendering and navigation.

use crate::git::{GitLineChange, GitLineKind};

/// Project Git's saved-file markers onto an edited in-memory buffer when line counts differ.
pub(super) fn live_git_line_changes(
    disk_changes: &[GitLineChange],
    saved: &str,
    current: &str,
) -> Vec<GitLineChange> {
    let saved_lines = saved.split('\n').collect::<Vec<_>>();
    let current_lines = current.split('\n').collect::<Vec<_>>();
    if saved_lines.len() == current_lines.len() {
        return disk_changes.to_vec();
    }

    let prefix = saved_lines
        .iter()
        .zip(&current_lines)
        .take_while(|(saved, current)| saved == current)
        .count();
    let suffix = saved_lines[prefix..]
        .iter()
        .rev()
        .zip(current_lines[prefix..].iter().rev())
        .take_while(|(saved, current)| saved == current)
        .count();
    let old_end = saved_lines.len().saturating_sub(suffix);
    let new_end = current_lines.len().saturating_sub(suffix);
    let line_delta = current_lines.len() as isize - saved_lines.len() as isize;

    let mut changes = disk_changes
        .iter()
        .filter_map(|change| {
            let line = if change.line < prefix {
                change.line
            } else if change.line >= old_end {
                change.line.saturating_add_signed(line_delta)
            } else {
                return None;
            };
            Some(GitLineChange { line, ..*change })
        })
        .collect::<Vec<_>>();

    let replaced_lines = old_end
        .saturating_sub(prefix)
        .min(new_end.saturating_sub(prefix));
    for line in prefix..new_end {
        let kind = if line < prefix + replaced_lines {
            GitLineKind::Modified
        } else {
            GitLineKind::Added
        };
        if let Some(change) = changes.iter_mut().find(|change| change.line == line) {
            // An inserted line is more specific than a pre-existing modified marker.
            if kind == GitLineKind::Added {
                change.kind = kind;
            }
        } else {
            changes.push(GitLineChange { line, kind });
        }
    }
    // A pure deletion has no new line to color; mark the line immediately after it instead.
    if new_end == prefix
        && prefix < current_lines.len()
        && !changes.iter().any(|change| change.line == prefix)
    {
        changes.push(GitLineChange {
            line: prefix,
            kind: GitLineKind::Modified,
        });
    }
    changes.sort_by_key(|change| change.line);
    changes
}

pub(super) fn char_index_to_byte(content: &str, char_index: usize) -> usize {
    content
        .char_indices()
        .nth(char_index)
        .map(|(byte, _)| byte)
        .unwrap_or(content.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_index_maps_unicode_and_end_of_text_to_bytes() {
        let content = "aé🦀";
        assert_eq!(char_index_to_byte(content, 0), 0);
        assert_eq!(char_index_to_byte(content, 1), 1);
        assert_eq!(char_index_to_byte(content, 2), 3);
        assert_eq!(char_index_to_byte(content, 3), content.len());
        assert_eq!(char_index_to_byte(content, usize::MAX), content.len());
    }

    #[test]
    fn inserted_lines_shift_existing_disk_markers() {
        let disk = vec![GitLineChange {
            line: 2,
            kind: GitLineKind::Modified,
        }];
        let changes = live_git_line_changes(&disk, "a\nb\nc", "a\nnew\nb\nc");
        assert!(changes.contains(&GitLineChange {
            line: 1,
            kind: GitLineKind::Added,
        }));
        assert!(changes.contains(&GitLineChange {
            line: 3,
            kind: GitLineKind::Modified,
        }));
    }

    #[test]
    fn pure_deletion_marks_the_following_line() {
        let changes = live_git_line_changes(&[], "a\nremoved\nc", "a\nc");
        assert_eq!(
            changes,
            vec![GitLineChange {
                line: 1,
                kind: GitLineKind::Modified,
            }]
        );
    }
}
