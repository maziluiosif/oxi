use super::super::state::{EditorDocument, EditorState};
use super::EditorLayoutCache;
use std::path::PathBuf;

fn document(path: PathBuf) -> EditorDocument {
    EditorDocument {
        markdown_preview_open: false,
        path,
        is_scratchpad: false,
        content: "my edits".into(),
        saved_content: "original".into(),
        disk_modified: None,
        externally_modified: false,
        syntax_state: None,
        content_revision: 0,
        dirty: true,
        layout_cache: EditorLayoutCache::default(),
        minimap_cache: None,
        viewport_width_bits: None,
        viewport_anchor_line: 0,
        media: None,
        diff: None,
    }
}

struct TestFile(PathBuf);
impl TestFile {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "oxi-editor-{}-{}.txt",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::write(&path, "original").unwrap();
        Self(path)
    }
}
impl Drop for TestFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn closing_other_tabs_preserves_the_visible_document() {
    for active in 0..4 {
        for closed in 0..4 {
            let mut editor = EditorState {
                documents: (0..4)
                    .map(|i| document(PathBuf::from(format!("{i}.rs"))))
                    .collect(),
                active: Some(active),
                hidden_active: Some(active),
                ..Default::default()
            };
            editor.remove_document(closed);
            if active != closed {
                assert_eq!(
                    editor.active_document().unwrap().path,
                    PathBuf::from(format!("{active}.rs"))
                );
                assert_eq!(editor.active, editor.hidden_active);
            } else {
                assert!(editor.active.unwrap() < editor.documents.len());
            }
        }
    }
}

#[test]
fn closing_last_document_clears_selections() {
    let mut editor = EditorState {
        documents: vec![document("a.rs".into())],
        active: Some(0),
        hidden_active: Some(0),
        file_picker_previous_active: Some(0),
        ..Default::default()
    };
    editor.remove_document(0);
    assert!(editor.active.is_none());
    assert!(editor.hidden_active.is_none());
    assert!(editor.file_picker_previous_active.is_none());
}

#[test]
fn closing_background_tab_does_not_reveal_a_hidden_editor() {
    let mut editor = EditorState {
        documents: vec![document("a.rs".into()), document("b.rs".into())],
        hidden_active: Some(1),
        ..Default::default()
    };
    editor.remove_document(0);
    assert!(editor.active.is_none());
    assert_eq!(editor.hidden_active, Some(0));
}

#[test]
fn save_preserves_external_edits_until_explicit_overwrite() {
    let file = TestFile::new();
    let mut doc = document(file.0.clone());
    std::fs::write(&file.0, "external edits").unwrap();
    assert!(doc.save_to_disk(false).is_err());
    assert_eq!(std::fs::read_to_string(&file.0).unwrap(), "external edits");
    assert_eq!(doc.content, "my edits");
    assert_eq!(doc.saved_content, "original");
    assert!(doc.dirty && doc.externally_modified);
    doc.save_to_disk(true).unwrap();
    assert_eq!(std::fs::read_to_string(&file.0).unwrap(), "my edits");
    assert!(!doc.dirty && !doc.externally_modified);
}

#[test]
fn save_does_not_silently_recreate_a_deleted_file() {
    let file = TestFile::new();
    let mut doc = document(file.0.clone());
    std::fs::remove_file(&file.0).unwrap();
    assert!(doc.save_to_disk(false).is_err());
    assert!(!file.0.exists());
    assert!(doc.dirty);
    doc.save_to_disk(true).unwrap();
    assert_eq!(std::fs::read_to_string(&file.0).unwrap(), "my edits");
}

#[test]
fn saving_unchanged_disk_version_commits_buffer_and_clears_dirty() {
    let file = TestFile::new();
    let mut doc = document(file.0.clone());
    doc.save_to_disk(false).unwrap();
    assert_eq!(std::fs::read_to_string(&file.0).unwrap(), "my edits");
    assert_eq!(doc.saved_content, doc.content);
    assert!(!doc.is_dirty());
}

#[test]
fn failed_write_preserves_the_unsaved_buffer() {
    let file = TestFile::new();
    let mut doc = document(file.0.join("missing/child.txt"));
    assert!(doc.save_to_disk(true).is_err());
    assert!(doc.is_dirty());
    assert_eq!(doc.saved_content, "original");
    assert_eq!(doc.content, "my edits");
}

fn clean_document(path: PathBuf) -> EditorDocument {
    let mut doc = document(path);
    doc.content.clone_from(&doc.saved_content);
    doc.dirty = false;
    doc
}

#[test]
fn scratchpad_reloads_same_size_changes_even_with_preserved_mtime() {
    let file = TestFile::new();
    let mut doc = clean_document(file.0.clone());
    doc.is_scratchpad = true;
    doc.reload_from_disk().unwrap();
    let modified = doc.disk_modified.unwrap();
    crate::scratchpad::update(&file.0, "rewrite", "new text").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file.0)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    doc.sync_from_disk();
    assert_eq!(doc.content, "new text");
    assert_eq!(doc.content_revision, 1);
    assert!(!doc.is_dirty());
}

#[test]
fn scratchpad_sync_preserves_unsaved_manual_edits() {
    let file = TestFile::new();
    let mut doc = document(file.0.clone());
    doc.is_scratchpad = true;
    crate::scratchpad::update(&file.0, "append", " agent notes").unwrap();
    doc.sync_from_disk();
    assert_eq!(doc.content, "my edits");
    assert_eq!(doc.saved_content, "original");
    assert!(doc.is_dirty() && doc.externally_modified);
    assert!(crate::scratchpad::save(&file.0, &doc.saved_content, &doc.content).is_err());
    assert_eq!(
        std::fs::read_to_string(&file.0).unwrap(),
        "original agent notes"
    );
}

#[test]
fn external_changes_reload_clean_documents() {
    let file = TestFile::new();
    let mut doc = clean_document(file.0.clone());
    std::fs::write(&file.0, "external edits").unwrap();
    doc.sync_from_disk();
    assert_eq!(doc.content, "external edits");
    assert_eq!(doc.saved_content, doc.content);
    assert_eq!(doc.content_revision, 1);
    assert!(!doc.dirty && !doc.externally_modified);
    doc.sync_from_disk();
    assert_eq!(doc.content_revision, 1);
}

#[test]
fn external_changes_preserve_dirty_documents() {
    let file = TestFile::new();
    let mut doc = document(file.0.clone());
    std::fs::write(&file.0, "external edits").unwrap();
    doc.sync_from_disk();
    assert_eq!(doc.content, "my edits");
    assert_eq!(doc.saved_content, "original");
    assert_eq!(doc.content_revision, 0);
    assert!(doc.dirty && doc.externally_modified);
}

#[test]
fn external_reload_updates_inactive_tabs_without_changing_selection() {
    let first = TestFile::new();
    let second = TestFile::new();
    let mut editor = EditorState {
        documents: vec![
            clean_document(first.0.clone()),
            clean_document(second.0.clone()),
        ],
        active: Some(0),
        ..Default::default()
    };
    std::fs::write(&second.0, "updated inactive tab").unwrap();
    for doc in &mut editor.documents {
        doc.sync_from_disk();
    }
    assert_eq!(editor.active, Some(0));
    assert_eq!(editor.documents[0].content, "original");
    assert_eq!(editor.documents[1].content, "updated inactive tab");
}

#[test]
fn same_content_reload_does_not_invalidate_the_revision() {
    let file = TestFile::new();
    let mut doc = clean_document(file.0.clone());
    doc.sync_from_disk();
    assert_eq!(doc.content_revision, 0);
    assert!(doc.disk_modified.is_some());
    assert!(!doc.externally_modified);
}

#[test]
fn deleted_file_keeps_its_clean_buffer_until_it_reappears() {
    let file = TestFile::new();
    let mut doc = clean_document(file.0.clone());
    doc.sync_from_disk();
    std::fs::remove_file(&file.0).unwrap();
    doc.sync_from_disk();
    assert_eq!(doc.content, "original");
    assert!(!doc.dirty && doc.externally_modified);
    std::fs::write(&file.0, "restored").unwrap();
    doc.sync_from_disk();
    assert_eq!(doc.content, "restored");
    assert!(!doc.externally_modified);
}

#[test]
fn binary_replacement_preserves_the_text_buffer() {
    let file = TestFile::new();
    let mut doc = clean_document(file.0.clone());
    std::fs::write(&file.0, [0xff, 0xfe]).unwrap();
    doc.sync_from_disk();
    assert_eq!(doc.content, "original");
    assert!(doc.externally_modified);
    assert!(!doc.dirty);
}

#[test]
fn oversized_replacement_preserves_the_text_buffer() {
    let file = TestFile::new();
    let mut doc = clean_document(file.0.clone());
    std::fs::write(
        &file.0,
        vec![b'x'; super::documents::MAX_TEXT_FILE_BYTES as usize + 1],
    )
    .unwrap();
    doc.sync_from_disk();
    assert_eq!(doc.content, "original");
    assert!(doc.externally_modified);
    assert!(!doc.dirty);
}

#[test]
fn changed_size_is_detected_even_when_the_mtime_is_preserved() {
    let file = TestFile::new();
    let mut doc = clean_document(file.0.clone());
    doc.sync_from_disk();
    let modified = doc.disk_modified.unwrap();
    std::fs::write(&file.0, "longer external content").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file.0)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    doc.sync_from_disk();
    assert_eq!(doc.content, "longer external content");
    assert!(!doc.externally_modified);
}

#[test]
fn preview_selection_survives_other_tab_closes_and_clears_with_its_source() {
    let mut editor = EditorState {
        documents: vec![
            document(PathBuf::from("other.rs")),
            document(PathBuf::from("readme.md")),
        ],
        active: Some(1),
        markdown_preview_active: true,
        ..Default::default()
    };
    editor.documents[1].markdown_preview_open = true;
    editor.remove_document(0);
    assert_eq!(editor.active, Some(0));
    assert!(editor.markdown_preview_active);
    assert!(editor.active_document().unwrap().markdown_preview_open);
    editor.remove_document(0);
    assert!(editor.active.is_none());
    assert!(!editor.markdown_preview_active);
}
