use super::super::state::{EditorDocument, EditorState};
use super::EditorLayoutCache;
use std::path::PathBuf;

fn document(path: PathBuf) -> EditorDocument {
    EditorDocument {
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
