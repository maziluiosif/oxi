//! Lightweight Rust "go to definition" support for the workspace editor.
//!
//! This deliberately uses the Tree-sitter parser already shipped for highlighting, so navigation
//! works without requiring a separately installed `rust-analyzer`. It resolves declarations in
//! open (possibly unsaved) buffers first, then indexes the remaining Rust files in the workspace.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tree_sitter::{Node, Parser};
use walkdir::WalkDir;

const MAX_RUST_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionLocation {
    pub path: PathBuf,
    /// Byte range of the declaration's name.
    pub byte_range: std::ops::Range<usize>,
}

#[derive(Clone, Debug)]
struct Definition {
    location: DefinitionLocation,
    name: String,
    /// Lexical scope in which a local declaration is visible. Item declarations use the file.
    scope: std::ops::Range<usize>,
    local: bool,
}

/// Resolve the Rust identifier at `cursor_byte` in `current_path`.
///
/// `open_buffers` overlays files on disk and should contain editor buffers so unsaved declarations
/// remain navigable.
pub fn find_definition(
    workspace_root: &Path,
    current_path: &Path,
    current_source: &str,
    cursor_byte: usize,
    open_buffers: &[(PathBuf, String)],
) -> Option<DefinitionLocation> {
    let (name, _) = identifier_at(current_source, cursor_byte)?;
    let current_path = canonical_or_owned(current_path);
    let root = canonical_or_owned(workspace_root);
    let mut sources = HashMap::<PathBuf, String>::new();

    for (path, source) in open_buffers {
        if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            sources.insert(canonical_or_owned(path), source.clone());
        }
    }
    sources
        .entry(current_path.clone())
        .or_insert_with(|| current_source.to_owned());

    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            entry.depth() == 0 || !matches!(name.as_ref(), ".git" | "target" | "node_modules")
        })
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("rs"))
    {
        let path = canonical_or_owned(entry.path());
        if sources.contains_key(&path) {
            continue;
        }
        if entry
            .metadata()
            .is_ok_and(|metadata| metadata.len() <= MAX_RUST_FILE_BYTES)
            && let Ok(source) = std::fs::read_to_string(entry.path())
        {
            sources.insert(path, source);
        }
    }

    let qualifier = qualifier_before(current_source, cursor_byte);
    let mut definitions = Vec::new();
    for (path, source) in &sources {
        collect_definitions(path, source, &mut definitions);
    }

    definitions
        .into_iter()
        .filter(|definition| definition.name == name)
        .filter_map(|definition| {
            let same_file = definition.location.path == current_path;
            if definition.local
                && (!same_file || !definition.scope.contains(&cursor_byte))
                && definition.location.byte_range.start != cursor_byte
            {
                return None;
            }

            let mut score = 0_i64;
            if same_file {
                score += 100_000;
                if definition.location.byte_range.start <= cursor_byte {
                    score += 20_000
                        - (cursor_byte - definition.location.byte_range.start).min(20_000) as i64;
                }
            }
            if definition.location.path.parent() == current_path.parent() {
                score += 5_000;
            }
            if qualifier.as_deref().is_some_and(|qualifier| {
                definition
                    .location
                    .path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    == Some(qualifier)
            }) {
                score += 50_000;
            }
            Some((score, definition.location))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, location)| location)
}

fn collect_definitions(path: &Path, source: &str, output: &mut Vec<Definition>) {
    let mut parser = Parser::new();
    let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    if parser.set_language(&language).is_err() {
        return;
    }
    let Some(tree) = parser.parse(source, None) else {
        return;
    };
    collect_node(
        tree.root_node(),
        path,
        source,
        0..source.len().saturating_add(1),
        output,
    );
}

fn collect_node(
    node: Node<'_>,
    path: &Path,
    source: &str,
    scope: std::ops::Range<usize>,
    output: &mut Vec<Definition>,
) {
    let kind = node.kind();
    let item = matches!(
        kind,
        "function_item"
            | "struct_item"
            | "enum_item"
            | "union_item"
            | "trait_item"
            | "type_item"
            | "const_item"
            | "static_item"
            | "mod_item"
            | "macro_definition"
            | "associated_type"
    );
    if item || matches!(kind, "field_declaration" | "enum_variant") {
        if let Some(name) = node.child_by_field_name("name") {
            push_definition(path, source, name, scope.clone(), false, output);
        }
    } else if kind == "let_declaration" {
        if let Some(pattern) = node.child_by_field_name("pattern") {
            collect_pattern_names(path, source, pattern, scope.clone(), output);
        }
    } else if matches!(kind, "parameter" | "self_parameter") {
        if let Some(pattern) = node
            .child_by_field_name("pattern")
            .or_else(|| node.child_by_field_name("name"))
        {
            collect_pattern_names(path, source, pattern, scope.clone(), output);
        } else if kind == "self_parameter" {
            // `self`, `&self`, and `mut self` do not expose a stable `name` field in every grammar
            // version. Find only the literal self token, never type identifiers.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "self" {
                    push_definition(path, source, child, scope.clone(), true, output);
                }
            }
        }
    }

    let child_scope = if matches!(kind, "function_item" | "closure_expression" | "block") {
        node.byte_range()
    } else {
        scope
    };
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_node(child, path, source, child_scope.clone(), output);
    }
}

fn collect_pattern_names(
    path: &Path,
    source: &str,
    node: Node<'_>,
    scope: std::ops::Range<usize>,
    output: &mut Vec<Definition>,
) {
    if node.kind() == "identifier" {
        push_definition(path, source, node, scope, true, output);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_pattern_names(path, source, child, scope.clone(), output);
    }
}

fn push_definition(
    path: &Path,
    source: &str,
    name_node: Node<'_>,
    scope: std::ops::Range<usize>,
    local: bool,
    output: &mut Vec<Definition>,
) {
    let range = name_node.byte_range();
    if let Some(name) = source.get(range.clone()) {
        output.push(Definition {
            location: DefinitionLocation {
                path: path.to_path_buf(),
                byte_range: range,
            },
            name: name.to_owned(),
            scope,
            local,
        });
    }
}

/// Identifier and its byte range at (or immediately before) a caret position.
pub fn identifier_at(source: &str, cursor_byte: usize) -> Option<(&str, std::ops::Range<usize>)> {
    let mut byte = cursor_byte.min(source.len());
    while byte > 0 && !source.is_char_boundary(byte) {
        byte -= 1;
    }
    if byte == source.len() || !identifier_char_at(source, byte) {
        let previous = source[..byte].char_indices().next_back()?;
        if !is_identifier_char(previous.1) {
            return None;
        }
        byte = previous.0;
    }

    let mut start = byte;
    while let Some((index, character)) = source[..start].char_indices().next_back() {
        if !is_identifier_char(character) {
            break;
        }
        start = index;
    }
    let mut end = byte;
    for (offset, character) in source[byte..].char_indices() {
        if !is_identifier_char(character) {
            break;
        }
        end = byte + offset + character.len_utf8();
    }
    (start < end).then(|| (&source[start..end], start..end))
}

fn qualifier_before(source: &str, cursor_byte: usize) -> Option<String> {
    let (_, identifier) = identifier_at(source, cursor_byte)?;
    let prefix = source[..identifier.start].trim_end();
    let prefix = prefix.strip_suffix("::")?.trim_end();
    identifier_at(prefix, prefix.len()).map(|(name, _)| name.to_owned())
}

fn identifier_char_at(source: &str, byte: usize) -> bool {
    source[byte..]
        .chars()
        .next()
        .is_some_and(is_identifier_char)
}

fn is_identifier_char(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

fn canonical_or_owned(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_works_at_middle_and_end() {
        assert_eq!(identifier_at("let hello = 1", 6), Some(("hello", 4..9)));
        assert_eq!(identifier_at("hello", 5), Some(("hello", 0..5)));
        assert_eq!(identifier_at(" ", 1), None);
    }

    #[test]
    fn resolves_function_and_local_shadowing() {
        let root = std::env::temp_dir().join(format!("oxi-rust-goto-{}", std::process::id()));
        let path = root.join("main.rs");
        let source = "fn helper() {}\nfn main() { let helper = 3; dbg!(helper); }\n";
        let reference = source.rfind("helper").unwrap();
        let result = find_definition(
            &root,
            &path,
            source,
            reference,
            &[(path.clone(), source.to_owned())],
        )
        .unwrap();
        assert_eq!(&source[result.byte_range.clone()], "helper");
        assert_eq!(result.byte_range.start, source.find("helper =").unwrap());
    }

    #[test]
    fn qualifier_prefers_matching_module_file() {
        let root = std::env::temp_dir().join(format!("oxi-rust-goto-{}", std::process::id()));
        let main = root.join("main.rs");
        let util = root.join("util.rs");
        let other = root.join("other.rs");
        let source = "fn main() { util::run(); }";
        let buffers = vec![
            (main.clone(), source.to_owned()),
            (util.clone(), "pub fn run() {}".to_owned()),
            (other, "pub fn run() {}".to_owned()),
        ];
        let result =
            find_definition(&root, &main, source, source.find("run").unwrap(), &buffers).unwrap();
        assert_eq!(result.path, util);
    }
}
