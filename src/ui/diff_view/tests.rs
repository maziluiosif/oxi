use super::parse::word_diff;
use super::selection::word_at;
use super::*;

#[test]
fn blocks_know_both_sides_and_where_they_start() {
    let model = parse(
        "diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,6 +1,7 @@\n a\n-b\n+B\n+B2\n c\n-d\n e\n f\n+g\n",
    );
    let file = &model.files[0];
    assert_eq!(file.blocks.len(), 3);
    let modified = file.block(0);
    assert_eq!((modified.old_start, modified.new_start), (2, 2));
    assert_eq!(modified.old_lines, ["b"]);
    assert_eq!(modified.new_lines, ["B", "B2"]);
    // A deletion has no new lines: it would be reinserted after new line 4 ("c").
    let deleted = file.block(1);
    assert_eq!((deleted.old_start, deleted.new_start), (4, 5));
    assert_eq!(deleted.old_lines, ["d"]);
    assert!(deleted.new_lines.is_empty());
    let appended = file.block(2);
    assert_eq!((appended.old_start, appended.new_start), (7, 7));
    assert_eq!(appended.new_lines, ["g"]);
}

fn lines(file: &DiffFile) -> Vec<(Kind, Option<usize>, Option<usize>, &str)> {
    file.items
        .iter()
        .filter_map(|item| match item {
            Item::Line(l) => Some((l.kind, l.old_no, l.new_no, l.text.as_str())),
            Item::Gap { .. } => None,
        })
        .collect()
}

#[test]
fn parses_git_patch_with_line_numbers_and_gaps() {
    let model = parse(
        "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n\
         @@ -10,3 +10,3 @@ fn main() {\n ctx\n-old\n+new\n tail\n@@ -40,2 +40,3 @@\n x\n+y\n z\n",
    );
    assert_eq!(model.files.len(), 1);
    let file = &model.files[0];
    assert_eq!(file.path(), "src/a.rs");
    assert_eq!(file.status(), 'M');
    assert_eq!((file.added, file.removed), (2, 1));
    assert!(matches!(
        &file.items[0],
        Item::Gap { hidden: Some(9), heading } if heading == "fn main() {"
    ));
    assert!(matches!(
        file.items[5],
        Item::Gap {
            hidden: Some(27),
            ..
        }
    ));
    let l = lines(file);
    assert_eq!(l[1], (Kind::Removed, Some(11), None, "old"));
    assert_eq!(l[2], (Kind::Added, None, Some(11), "new"));
    assert_eq!(l[5], (Kind::Added, None, Some(41), "y"));
}

#[test]
fn removed_line_that_looks_like_a_file_header_stays_in_the_hunk() {
    let model = parse("--- a/q.sql\n+++ b/q.sql\n@@ -1,2 +1,1 @@\n--- comment\n keep\n");
    assert_eq!(model.files.len(), 1);
    assert_eq!(
        lines(&model.files[0])[0],
        (Kind::Removed, Some(1), None, "-- comment")
    );
}

#[test]
fn parses_commit_header_and_new_and_deleted_files() {
    let model = parse(
        "commit abc123\nAuthor: Ana <a@b>\nDate:   Mon\n\n    Add things\n\n    Body\n\n\
         diff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n\
         @@ -0,0 +1 @@\n+hi\ndiff --git a/gone.txt b/gone.txt\ndeleted file mode 100644\n\
         --- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n\
         diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n",
    );
    let commit = model.commit.as_ref().unwrap();
    assert_eq!(commit.hash, "abc123");
    assert_eq!(commit.message, "Add things\n\nBody");
    let statuses: Vec<char> = model.files.iter().map(DiffFile::status).collect();
    assert_eq!(statuses, ['A', 'D', 'M']);
    assert!(model.files[2].binary);
    assert_eq!(
        lines(&model.files[0])[0],
        (Kind::Added, None, Some(1), "hi")
    );
}

#[test]
fn double_click_word_spans_one_character_class() {
    let text = "let foo_bar = x.len();";
    assert_eq!(word_at(text, 5), (4, 11));
    assert_eq!(word_at(text, 3), (3, 4));
    assert_eq!(word_at(text, 15), (15, 16));
    assert_eq!(word_at(text, 99), (19, 22));
    assert_eq!(word_at("", 0), (0, 0));
}

#[test]
fn selected_text_stays_in_one_pane() {
    let mut slot = None;
    DiffView::sync(
        &mut slot,
        "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,3 @@\n keep one\n-old two\n+new two\n keep three\n",
    );
    let view = slot.as_mut().unwrap();
    view.ensure_layout(true);
    let first = (0..view.layout.rows.len())
        .find(|&row| view.pane_line(row, 1).is_some())
        .unwrap();
    view.selection = Some(TextSelection {
        pane: 1,
        anchor: (first, 5),
        cursor: (first + 2, 4),
    });
    assert_eq!(view.selected_text().as_deref(), Some("one\nnew two\nkeep"));
    view.selection = Some(TextSelection {
        pane: 0,
        anchor: (first + 1, 0),
        cursor: (first + 1, usize::MAX),
    });
    assert_eq!(view.selected_text().as_deref(), Some("old two"));
}

#[test]
fn word_diff_marks_only_the_changed_token() {
    let (old, new) = word_diff("let total = sum(values);", "let total = mean(values);");
    assert_eq!(old, vec![12..15]);
    assert_eq!(new, vec![12..16]);
    let (old, new) = word_diff("alpha beta", "gamma delta");
    assert!(old.is_empty() && new.is_empty());
}

#[test]
fn split_rows_pair_changes_and_fold_long_unchanged_runs() {
    let mut text = String::from("--- a/f\n+++ b/f\n@@ -1,22 +1,22 @@\n");
    for i in 0..10 {
        text.push_str(&format!(" a{i}\n"));
    }
    text.push_str("-old1\n-old2\n+new1\n");
    for i in 0..10 {
        text.push_str(&format!(" b{i}\n"));
    }
    text.push_str("+tail\n");
    let model = parse(&text);
    let layout = build_rows(&model, true, true, &HashSet::new(), &HashSet::new());
    let folds: Vec<_> = layout
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::Fold { start, end, .. } => Some((*start, *end)),
            _ => None,
        })
        .collect();
    // Leading run keeps only its trailing context; the middle run keeps both edges.
    assert_eq!(folds, [(0, 7), (16, 20)]);
    assert!(layout.rows.contains(&Row::Split {
        file: 0,
        left: Some(10),
        right: Some(12)
    }));
    assert!(layout.rows.contains(&Row::Split {
        file: 0,
        left: Some(11),
        right: None
    }));
    assert_eq!(layout.changes.len(), 2);
    let expanded = HashSet::from([(0, 0)]);
    let layout = build_rows(&model, true, true, &expanded, &HashSet::new());
    assert_eq!(
        layout
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Fold { .. }))
            .count(),
        1
    );
}
