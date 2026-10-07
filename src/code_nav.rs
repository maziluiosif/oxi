//! Tree-sitter code navigation for the workspace editor: go to definition, symbol outlines and
//! expand-selection, for every language with a grammar (Rust, Python, JavaScript/TypeScript, Go,
//! C/C++, Java, shell).
//!
//! It deliberately uses the parsers already shipped for highlighting, so navigation works without
//! a language server. Definitions come from a small per-language query; locals (variables and
//! parameters) are scoped to their enclosing function/block, everything else is visible
//! workspace-wide. Workspace files are parsed once and cached by mtime in a [`SymbolIndex`], so a
//! lookup after the first only stats the tree and reparses changed files.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use tree_sitter::{Language, Node, Parser, Query, QueryCursor, StreamingIterator};

/// Larger files are skipped when indexing the workspace (generated code, bundles).
const MAX_INDEXED_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionLocation {
    pub path: PathBuf,
    /// Byte range of the declaration's name.
    pub byte_range: Range<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Interface,
    Trait,
    Type,
    Module,
    Macro,
    Constant,
    Field,
    Variable,
    Parameter,
    /// A C/C++ prototype: only a fallback when no definition exists.
    Declaration,
    /// An imported name: only a fallback when the definition is outside the workspace.
    Import,
}

impl SymbolKind {
    fn from_capture(name: &str) -> Option<Self> {
        Some(match name.strip_prefix("definition.")? {
            "function" => Self::Function,
            "method" => Self::Method,
            "class" => Self::Class,
            "struct" => Self::Struct,
            "enum" => Self::Enum,
            "interface" => Self::Interface,
            "trait" => Self::Trait,
            "type" => Self::Type,
            "module" => Self::Module,
            "macro" => Self::Macro,
            "constant" => Self::Constant,
            "field" => Self::Field,
            "variable" => Self::Variable,
            "parameter" => Self::Parameter,
            "declaration" => Self::Declaration,
            "import" => Self::Import,
            _ => return None,
        })
    }

    /// Short tag shown next to a symbol in Goto Symbol.
    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Interface => "interface",
            Self::Trait => "trait",
            Self::Type => "type",
            Self::Module => "module",
            Self::Macro => "macro",
            Self::Constant => "constant",
            Self::Field => "field",
            Self::Variable => "variable",
            Self::Parameter => "parameter",
            Self::Declaration => "declaration",
            Self::Import => "import",
        }
    }

    /// Listed by Goto Symbol. Like Sublime: functions, types and modules, not locals or fields.
    pub fn is_outline(self) -> bool {
        matches!(
            self,
            Self::Function
                | Self::Method
                | Self::Class
                | Self::Struct
                | Self::Enum
                | Self::Interface
                | Self::Trait
                | Self::Type
                | Self::Module
                | Self::Macro
        )
    }

    fn can_be_local(self) -> bool {
        matches!(self, Self::Variable | Self::Parameter)
    }
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub name_range: Range<usize>,
    /// Lexical scope of a local declaration; `None` for names visible file-wide.
    pub scope: Option<Range<usize>>,
}

/// Language id (as returned by the editor's `language_for_path`) for a path, when navigation
/// supports it. Extensions the highlighter does not list (`.mjs`, `.hh`, ...) are included so
/// workspace indexing finds them.
pub fn language_for_path(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "rs" => "rs",
        "py" | "pyi" => "py",
        "js" | "mjs" | "cjs" => "js",
        "jsx" => "jsx",
        "ts" | "mts" | "cts" => "ts",
        "tsx" => "tsx",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "java" => "java",
        "sh" | "bash" | "zsh" => "sh",
        _ => return None,
    })
}

pub fn supports(language: &str) -> bool {
    spec(language).is_some()
}

/// Languages whose files can define each other's names (JS imports TS, C++ includes C headers).
pub fn family_of(language: &str) -> Option<&'static str> {
    Some(match language {
        "js" | "jsx" | "ts" | "tsx" => "js",
        "c" | "cpp" => "c",
        "rs" => "rs",
        "py" => "py",
        "go" => "go",
        "java" => "java",
        "sh" => "sh",
        _ => return None,
    })
}

pub fn same_family(a: &str, b: &str) -> bool {
    family_of(a).is_some() && family_of(a) == family_of(b)
}

/// Line-comment delimiters for Toggle Comment: `(prefix, suffix)`; the suffix is empty for
/// languages with line comments.
pub fn comment_tokens(language: &str) -> Option<(&'static str, &'static str)> {
    Some(match language {
        "rs" | "js" | "jsx" | "ts" | "tsx" | "go" | "c" | "cpp" | "java" => ("//", ""),
        "py" | "sh" | "yaml" | "toml" => ("#", ""),
        "css" => ("/*", "*/"),
        "html" | "md" => ("<!--", "-->"),
        _ => return None,
    })
}

struct Spec {
    language: Language,
    query: Query,
    /// Per capture index: what the capture means.
    roles: Vec<Role>,
    /// Node kinds that open a lexical scope for locals.
    scopes: &'static [&'static str],
}

#[derive(Clone, Copy)]
enum Role {
    Definition(SymbolKind),
    Name,
    /// Every binding identifier inside the node is a definition (destructuring patterns).
    Pattern,
    Ignored,
}

const RUST_QUERY: &str = r#"
(function_item name: (identifier) @name) @definition.function
(function_signature_item name: (identifier) @name) @definition.function
(struct_item name: (type_identifier) @name) @definition.struct
(union_item name: (type_identifier) @name) @definition.struct
(enum_item name: (type_identifier) @name) @definition.enum
(enum_variant name: (identifier) @name) @definition.constant
(trait_item name: (type_identifier) @name) @definition.trait
(type_item name: (type_identifier) @name) @definition.type
(associated_type name: (type_identifier) @name) @definition.type
(const_item name: (identifier) @name) @definition.constant
(static_item name: (identifier) @name) @definition.constant
(mod_item name: (identifier) @name) @definition.module
(macro_definition name: (identifier) @name) @definition.macro
(field_declaration name: (field_identifier) @name) @definition.field
(let_declaration pattern: (_) @pattern) @definition.variable
(let_condition pattern: (_) @pattern) @definition.variable
(for_expression pattern: (_) @pattern) @definition.variable
(match_arm pattern: (_) @pattern) @definition.variable
(parameter pattern: (_) @pattern) @definition.parameter
(self_parameter (self) @name) @definition.parameter
(closure_parameters (_) @pattern) @definition.parameter
"#;

const PYTHON_QUERY: &str = r#"
(function_definition name: (identifier) @name) @definition.function
(class_definition name: (identifier) @name) @definition.class
(assignment left: (identifier) @name) @definition.variable
(assignment left: [(pattern_list) (tuple_pattern)] @pattern) @definition.variable
(assignment
  left: (attribute object: (identifier) @_object attribute: (identifier) @name)
  (#eq? @_object "self")) @definition.field
(parameters (identifier) @name) @definition.parameter
(lambda_parameters (identifier) @name) @definition.parameter
(default_parameter name: (identifier) @name) @definition.parameter
(typed_parameter (identifier) @name) @definition.parameter
(typed_default_parameter name: (identifier) @name) @definition.parameter
(for_statement left: (_) @pattern) @definition.variable
(for_in_clause left: (_) @pattern) @definition.variable
(as_pattern alias: (as_pattern_target) @pattern) @definition.variable
(import_from_statement name: (dotted_name (identifier) @name)) @definition.import
(aliased_import alias: (identifier) @name) @definition.import
(import_statement name: (dotted_name . (identifier) @name)) @definition.import
"#;

/// Shared by JavaScript and TypeScript (TypeScript adds its own declarations below).
const JS_COMMON_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @definition.function
(generator_function_declaration name: (identifier) @name) @definition.function
(function_expression name: (identifier) @name) @definition.function
(method_definition name: (property_identifier) @name) @definition.method
(variable_declarator
  name: (identifier) @name
  value: [(arrow_function) (function_expression)]) @definition.function
(variable_declarator name: (identifier) @name) @definition.variable
(variable_declarator name: [(object_pattern) (array_pattern)] @pattern) @definition.variable
(arrow_function parameter: (identifier) @name) @definition.parameter
(catch_clause parameter: (_) @pattern) @definition.parameter
(for_in_statement left: (_) @pattern) @definition.variable
(pair key: (property_identifier) @name value: [(arrow_function) (function_expression)]) @definition.method
(import_specifier name: (identifier) @name !alias) @definition.import
(import_specifier alias: (identifier) @name) @definition.import
(import_clause (identifier) @name) @definition.import
(namespace_import (identifier) @name) @definition.import
"#;

const JS_QUERY: &str = r#"
(class_declaration name: (identifier) @name) @definition.class
(field_definition property: (property_identifier) @name) @definition.field
(formal_parameters (_) @pattern) @definition.parameter
"#;

const TS_QUERY: &str = r#"
(function_signature name: (identifier) @name) @definition.function
(class_declaration name: (type_identifier) @name) @definition.class
(abstract_class_declaration name: (type_identifier) @name) @definition.class
(interface_declaration name: (type_identifier) @name) @definition.interface
(type_alias_declaration name: (type_identifier) @name) @definition.type
(enum_declaration name: (identifier) @name) @definition.enum
(enum_body (property_identifier) @name) @definition.constant
(enum_assignment name: (property_identifier) @name) @definition.constant
(internal_module name: (identifier) @name) @definition.module
(method_signature name: (property_identifier) @name) @definition.method
(abstract_method_signature name: (property_identifier) @name) @definition.method
(public_field_definition name: (property_identifier) @name) @definition.field
(property_signature name: (property_identifier) @name) @definition.field
(required_parameter pattern: (_) @pattern) @definition.parameter
(optional_parameter pattern: (_) @pattern) @definition.parameter
"#;

const GO_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @definition.function
(method_declaration name: (field_identifier) @name) @definition.method
(type_spec name: (type_identifier) @name) @definition.type
(type_alias name: (type_identifier) @name) @definition.type
(field_declaration name: (field_identifier) @name) @definition.field
(method_elem name: (field_identifier) @name) @definition.method
(const_spec name: (identifier) @name) @definition.constant
(var_spec name: (identifier) @name) @definition.variable
(parameter_declaration name: (identifier) @name) @definition.parameter
(variadic_parameter_declaration name: (identifier) @name) @definition.parameter
(short_var_declaration left: (expression_list (identifier) @name)) @definition.variable
(range_clause left: (expression_list (identifier) @name)) @definition.variable
(import_spec name: (package_identifier) @name) @definition.import
"#;

/// Shared by C and C++.
const C_COMMON_QUERY: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @definition.function
(function_definition
  declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @definition.function
(declaration declarator: (function_declarator declarator: (identifier) @name)) @definition.declaration
(declaration
  declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @definition.declaration
(struct_specifier name: (type_identifier) @name body: (_)) @definition.struct
(union_specifier name: (type_identifier) @name body: (_)) @definition.struct
(enum_specifier name: (type_identifier) @name body: (_)) @definition.enum
(type_definition declarator: (type_identifier) @name) @definition.type
(enumerator name: (identifier) @name) @definition.constant
(field_declaration declarator: (field_identifier) @name) @definition.field
(field_declaration declarator: (pointer_declarator declarator: (field_identifier) @name)) @definition.field
(preproc_def name: (identifier) @name) @definition.macro
(preproc_function_def name: (identifier) @name) @definition.macro
(declaration declarator: (identifier) @name) @definition.variable
(declaration declarator: (init_declarator declarator: (identifier) @name)) @definition.variable
(declaration
  declarator: (init_declarator declarator: (pointer_declarator declarator: (identifier) @name))) @definition.variable
(declaration declarator: (pointer_declarator declarator: (identifier) @name)) @definition.variable
(declaration declarator: (array_declarator declarator: (identifier) @name)) @definition.variable
(parameter_declaration declarator: (identifier) @name) @definition.parameter
(parameter_declaration declarator: (pointer_declarator declarator: (identifier) @name)) @definition.parameter
"#;

const CPP_QUERY: &str = r#"
(function_definition declarator: (function_declarator declarator: (field_identifier) @name)) @definition.method
(function_definition
  declarator: (function_declarator declarator: (qualified_identifier name: (identifier) @name))) @definition.method
(field_declaration declarator: (function_declarator declarator: (field_identifier) @name)) @definition.declaration
(class_specifier name: (type_identifier) @name body: (_)) @definition.class
(namespace_definition name: (namespace_identifier) @name) @definition.module
(alias_declaration name: (type_identifier) @name) @definition.type
(for_range_loop declarator: (identifier) @name) @definition.variable
(parameter_declaration declarator: (reference_declarator (identifier) @name)) @definition.parameter
"#;

const JAVA_QUERY: &str = r#"
(class_declaration name: (identifier) @name) @definition.class
(interface_declaration name: (identifier) @name) @definition.interface
(enum_declaration name: (identifier) @name) @definition.enum
(record_declaration name: (identifier) @name) @definition.class
(annotation_type_declaration name: (identifier) @name) @definition.interface
(method_declaration name: (identifier) @name) @definition.method
(constructor_declaration name: (identifier) @name) @definition.method
(field_declaration declarator: (variable_declarator name: (identifier) @name)) @definition.field
(enum_constant name: (identifier) @name) @definition.constant
(local_variable_declaration declarator: (variable_declarator name: (identifier) @name)) @definition.variable
(formal_parameter name: (identifier) @name) @definition.parameter
(spread_parameter (variable_declarator name: (identifier) @name)) @definition.parameter
(catch_formal_parameter name: (identifier) @name) @definition.parameter
(enhanced_for_statement name: (identifier) @name) @definition.variable
(lambda_expression parameters: (identifier) @name) @definition.parameter
(inferred_parameters (identifier) @name) @definition.parameter
"#;

const BASH_QUERY: &str = r#"
(function_definition name: (word) @name) @definition.function
(variable_assignment name: (variable_name) @name) @definition.variable
"#;

const RUST_SCOPES: &[&str] = &["function_item", "closure_expression", "block"];
const PYTHON_SCOPES: &[&str] = &[
    "function_definition",
    "lambda",
    "list_comprehension",
    "set_comprehension",
    "dictionary_comprehension",
    "generator_expression",
];
const JS_SCOPES: &[&str] = &[
    "function_declaration",
    "generator_function_declaration",
    "function_expression",
    "arrow_function",
    "method_definition",
    "statement_block",
    "for_statement",
    "for_in_statement",
    "catch_clause",
];
const GO_SCOPES: &[&str] = &[
    "function_declaration",
    "method_declaration",
    "func_literal",
    "block",
];
const C_SCOPES: &[&str] = &[
    "function_definition",
    "compound_statement",
    "for_statement",
    "lambda_expression",
    "for_range_loop",
];
const JAVA_SCOPES: &[&str] = &[
    "method_declaration",
    "constructor_declaration",
    "lambda_expression",
    "block",
    "for_statement",
    "enhanced_for_statement",
    "catch_clause",
];

fn build_spec(
    language: Language,
    sources: &[&str],
    scopes: &'static [&'static str],
) -> Result<Spec, tree_sitter::QueryError> {
    let query = Query::new(&language, &sources.concat())?;
    let roles = query
        .capture_names()
        .iter()
        .map(|name| match *name {
            "name" => Role::Name,
            "pattern" => Role::Pattern,
            other => SymbolKind::from_capture(other).map_or(Role::Ignored, Role::Definition),
        })
        .collect();
    Ok(Spec {
        language,
        query,
        roles,
        scopes,
    })
}

fn build_specs() -> HashMap<&'static str, Spec> {
    let javascript: Language = tree_sitter_javascript::LANGUAGE.into();
    let typescript: Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    let tsx: Language = tree_sitter_typescript::LANGUAGE_TSX.into();
    let entries: [(&str, Result<Spec, tree_sitter::QueryError>); 11] = [
        (
            "rs",
            build_spec(
                tree_sitter_rust::LANGUAGE.into(),
                &[RUST_QUERY],
                RUST_SCOPES,
            ),
        ),
        (
            "py",
            build_spec(
                tree_sitter_python::LANGUAGE.into(),
                &[PYTHON_QUERY],
                PYTHON_SCOPES,
            ),
        ),
        (
            "js",
            build_spec(javascript.clone(), &[JS_COMMON_QUERY, JS_QUERY], JS_SCOPES),
        ),
        (
            "jsx",
            build_spec(javascript, &[JS_COMMON_QUERY, JS_QUERY], JS_SCOPES),
        ),
        (
            "ts",
            build_spec(typescript, &[JS_COMMON_QUERY, TS_QUERY], JS_SCOPES),
        ),
        (
            "tsx",
            build_spec(tsx, &[JS_COMMON_QUERY, TS_QUERY], JS_SCOPES),
        ),
        (
            "go",
            build_spec(tree_sitter_go::LANGUAGE.into(), &[GO_QUERY], GO_SCOPES),
        ),
        (
            "c",
            build_spec(tree_sitter_c::LANGUAGE.into(), &[C_COMMON_QUERY], C_SCOPES),
        ),
        (
            "cpp",
            build_spec(
                tree_sitter_cpp::LANGUAGE.into(),
                &[C_COMMON_QUERY, CPP_QUERY],
                C_SCOPES,
            ),
        ),
        (
            "java",
            build_spec(
                tree_sitter_java::LANGUAGE.into(),
                &[JAVA_QUERY],
                JAVA_SCOPES,
            ),
        ),
        (
            "sh",
            build_spec(tree_sitter_bash::LANGUAGE.into(), &[BASH_QUERY], &[]),
        ),
    ];
    entries
        .into_iter()
        .filter_map(|(language, spec)| match spec {
            Ok(spec) => Some((language, spec)),
            Err(error) => {
                // A grammar update renamed a node: lose navigation for that language only.
                log::warn!("code navigation query for {language} failed: {error}");
                None
            }
        })
        .collect()
}

fn spec(language: &str) -> Option<&'static Spec> {
    static SPECS: OnceLock<HashMap<&'static str, Spec>> = OnceLock::new();
    SPECS.get_or_init(build_specs).get(language)
}

fn parse(language: &str, source: &str) -> Option<tree_sitter::Tree> {
    thread_local! {
        static PARSERS: RefCell<HashMap<&'static str, Parser>> = RefCell::new(HashMap::new());
    }
    let spec = spec(language)?;
    PARSERS.with(|parsers| {
        let mut parsers = parsers.borrow_mut();
        let key = match language {
            "rs" => "rs",
            "py" => "py",
            "js" => "js",
            "jsx" => "jsx",
            "ts" => "ts",
            "tsx" => "tsx",
            "go" => "go",
            "c" => "c",
            "cpp" => "cpp",
            "java" => "java",
            _ => "sh",
        };
        let parser = parsers.entry(key).or_insert_with(Parser::new);
        parser.set_language(&spec.language).ok()?;
        parser.parse(source, None)
    })
}

/// Every definition in `source`, sorted by position.
pub fn file_symbols(language: &str, source: &str) -> Vec<Symbol> {
    let (Some(spec), Some(tree)) = (spec(language), parse(language, source)) else {
        return Vec::new();
    };
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&spec.query, tree.root_node(), source.as_bytes());
    while let Some(found) = matches.next() {
        let mut definition = None;
        let mut name = None;
        let mut pattern = None;
        for capture in found.captures() {
            match spec.roles[capture.index as usize] {
                Role::Definition(kind) => definition = Some((capture.node, kind)),
                Role::Name => name = Some(capture.node),
                Role::Pattern => pattern = Some(capture.node),
                Role::Ignored => {}
            }
        }
        let Some((node, kind)) = definition else {
            continue;
        };
        let scope = kind
            .can_be_local()
            .then(|| local_scope(spec, node))
            .flatten();
        let mut push = |name_node: Node<'_>| {
            let range = name_node.byte_range();
            if let Some(text) = source.get(range.clone()) {
                symbols.push(Symbol {
                    name: text.to_owned(),
                    kind,
                    name_range: range,
                    scope: scope.clone(),
                });
            }
        };
        if let Some(name) = name {
            push(name);
        }
        if let Some(pattern) = pattern {
            let mut bindings = Vec::new();
            pattern_bindings(pattern, language, source, &mut bindings);
            bindings.into_iter().for_each(&mut push);
        }
    }
    // Several patterns can match one name (`const f = () => {}` is both a variable and a
    // function); keep the most specific kind.
    symbols.sort_by_key(|symbol| {
        (
            symbol.name_range.start,
            symbol.kind.can_be_local() || symbol.kind == SymbolKind::Import,
        )
    });
    symbols.dedup_by(|later, earlier| later.name_range == earlier.name_range);
    symbols
}

/// The nearest enclosing scope of a local declaration, if any (top-level variables are global).
fn local_scope(spec: &Spec, node: Node<'_>) -> Option<Range<usize>> {
    let mut parent = node.parent();
    while let Some(ancestor) = parent {
        if spec.scopes.contains(&ancestor.kind()) {
            return Some(ancestor.byte_range());
        }
        parent = ancestor.parent();
    }
    None
}

/// Identifiers bound by a destructuring pattern. Type names, default values and Rust enum
/// variants (`Some(x)`, `None`) are not bindings.
fn pattern_bindings<'tree>(
    node: Node<'tree>,
    language: &str,
    source: &str,
    output: &mut Vec<Node<'tree>>,
) {
    if matches!(
        node.kind(),
        "identifier" | "shorthand_property_identifier_pattern" | "shorthand_field_identifier"
    ) {
        let uppercase = source
            .get(node.byte_range())
            .and_then(|text| text.chars().next())
            .is_some_and(char::is_uppercase);
        if !(language == "rs" && uppercase) {
            output.push(node);
        }
        return;
    }
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let skip = matches!(
            cursor.field_name(),
            Some("type" | "value" | "right" | "default_value" | "function")
        );
        if !skip && cursor.node().is_named() {
            pattern_bindings(cursor.node(), language, source, output);
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

/// A source file's top-level-visible symbols, cached by modification time and size.
struct IndexedFile {
    modified: Option<SystemTime>,
    len: u64,
    symbols: Arc<[Symbol]>,
}

/// Workspace-wide definitions. Shared between the UI and worker threads; files are parsed
/// outside the lock.
#[derive(Default)]
pub struct SymbolIndex {
    files: Mutex<HashMap<PathBuf, IndexedFile>>,
}

impl SymbolIndex {
    /// Symbols visible outside their file for each of `files`. Unchanged files come from the
    /// cache; files listed in `overrides` (open editor buffers) are parsed from that text.
    pub fn symbols_for(
        &self,
        files: &[PathBuf],
        overrides: &HashMap<PathBuf, &str>,
    ) -> Vec<(PathBuf, Arc<[Symbol]>)> {
        let mut output = Vec::with_capacity(files.len());
        for path in files {
            let Some(language) = language_for_path(path) else {
                continue;
            };
            if let Some(source) = overrides.get(path) {
                output.push((path.clone(), global_symbols(language, source)));
                continue;
            }
            let Ok(metadata) = std::fs::metadata(path) else {
                continue;
            };
            if metadata.len() > MAX_INDEXED_FILE_BYTES {
                continue;
            }
            let modified = metadata.modified().ok();
            let cached = self.files.lock().ok().and_then(|files| {
                files
                    .get(path)
                    .filter(|file| file.modified == modified && file.len == metadata.len())
                    .map(|file| Arc::clone(&file.symbols))
            });
            let symbols = match cached {
                Some(symbols) => symbols,
                None => {
                    let Ok(source) = std::fs::read_to_string(path) else {
                        continue;
                    };
                    let symbols = global_symbols(language, &source);
                    if let Ok(mut files) = self.files.lock() {
                        files.insert(
                            path.clone(),
                            IndexedFile {
                                modified,
                                len: metadata.len(),
                                symbols: Arc::clone(&symbols),
                            },
                        );
                    }
                    symbols
                }
            };
            output.push((path.clone(), symbols));
        }
        output
    }
}

fn global_symbols(language: &str, source: &str) -> Arc<[Symbol]> {
    file_symbols(language, source)
        .into_iter()
        .filter(|symbol| symbol.scope.is_none())
        .collect()
}

/// Inputs for one go-to-definition lookup.
pub struct DefinitionRequest {
    pub current_path: PathBuf,
    pub current_source: String,
    pub cursor_byte: usize,
    /// Candidate workspace files (any language; filtered to the current language family).
    pub workspace_files: Vec<PathBuf>,
    /// Open editor buffers, which win over the files on disk so unsaved code is navigable.
    pub open_buffers: Vec<(PathBuf, String)>,
}

/// Resolve the identifier at `request.cursor_byte`. Locals in an enclosing scope win; otherwise
/// the best-scored workspace-visible definition (same file, `qualifier::`/`qualifier.` matching
/// the file or directory name, same directory) is returned.
pub fn find_definition(
    index: &SymbolIndex,
    request: &DefinitionRequest,
) -> Option<DefinitionLocation> {
    let language = language_for_path(&request.current_path)?;
    let source = request.current_source.as_str();
    let cursor = request.cursor_byte;
    let (name, identifier) = identifier_at(source, cursor)?;

    let current = file_symbols(language, source);
    // On a declaration already: that is its definition.
    if let Some(symbol) = current
        .iter()
        .find(|symbol| symbol.name_range == identifier)
    {
        return Some(DefinitionLocation {
            path: request.current_path.clone(),
            byte_range: symbol.name_range.clone(),
        });
    }
    let local = current
        .iter()
        .filter(|symbol| symbol.name == name && symbol.name_range.start <= cursor)
        .filter(|symbol| {
            symbol
                .scope
                .as_ref()
                .is_some_and(|scope| scope.contains(&cursor))
        })
        .max_by_key(|symbol| symbol.name_range.start);
    if let Some(symbol) = local {
        return Some(DefinitionLocation {
            path: request.current_path.clone(),
            byte_range: symbol.name_range.clone(),
        });
    }

    let qualifier = qualifier_before(source, cursor);
    let overrides: HashMap<PathBuf, &str> = request
        .open_buffers
        .iter()
        .filter(|(path, _)| path != &request.current_path)
        .map(|(path, text)| (path.clone(), text.as_str()))
        .collect();
    let mut files: Vec<PathBuf> = request
        .workspace_files
        .iter()
        .filter(|path| *path != &request.current_path)
        .filter(|path| language_for_path(path).is_some_and(|other| same_family(language, other)))
        .cloned()
        .collect();
    for path in overrides.keys() {
        if !files.contains(path)
            && language_for_path(path).is_some_and(|other| same_family(language, other))
        {
            files.push(path.clone());
        }
    }
    let others = index.symbols_for(&files, &overrides);

    let current_dir = request.current_path.parent();
    let score = |path: &Path, symbol: &Symbol| {
        let mut score = 0_i64;
        if path == request.current_path {
            score += 100_000;
            if symbol.name_range.start <= cursor {
                score += 20_000 - (cursor - symbol.name_range.start).min(20_000) as i64;
            }
        }
        if path.parent() == current_dir {
            score += 5_000;
        }
        if let Some(qualifier) = qualifier.as_deref() {
            let stem = path.file_stem().and_then(|stem| stem.to_str());
            let directory = path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str());
            if stem == Some(qualifier) || (stem == Some("mod") && directory == Some(qualifier)) {
                score += 150_000;
            } else if directory == Some(qualifier) {
                score += 140_000;
            }
        }
        match symbol.kind {
            SymbolKind::Declaration => score -= 150_000,
            SymbolKind::Import => score -= 300_000,
            _ => {}
        }
        score
    };
    let current_globals = current
        .iter()
        .filter(|symbol| symbol.scope.is_none())
        .map(|symbol| (request.current_path.as_path(), symbol));
    let other_symbols = others
        .iter()
        .flat_map(|(path, symbols)| symbols.iter().map(move |symbol| (path.as_path(), symbol)));
    current_globals
        .chain(other_symbols)
        .filter(|(_, symbol)| symbol.name == name)
        .max_by_key(|(path, symbol)| score(path, symbol))
        .map(|(path, symbol)| DefinitionLocation {
            path: path.to_path_buf(),
            byte_range: symbol.name_range.clone(),
        })
}

/// The next larger syntax node around `selection` (Sublime's Expand Selection to Scope).
pub fn expand_selection(
    language: &str,
    source: &str,
    selection: Range<usize>,
) -> Option<Range<usize>> {
    let tree = parse(language, source)?;
    let mut node = tree
        .root_node()
        .descendant_for_byte_range(selection.start, selection.end)?;
    loop {
        let range = node.byte_range();
        if range.start <= selection.start
            && range.end >= selection.end
            && range != selection
            && !source[range.clone()].trim().is_empty()
        {
            // Skip wrappers that cover exactly the same text as their child.
            return Some(range);
        }
        node = node.parent()?;
    }
}

/// Identifier and its byte range at (or immediately before) a caret position.
pub fn identifier_at(source: &str, cursor_byte: usize) -> Option<(&str, Range<usize>)> {
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

/// `util` in `util::run`, `util.run` or `util->run`, with the caret on `run`.
fn qualifier_before(source: &str, cursor_byte: usize) -> Option<String> {
    let (_, identifier) = identifier_at(source, cursor_byte)?;
    let prefix = source[..identifier.start].trim_end();
    let prefix = prefix
        .strip_suffix("::")
        .or_else(|| prefix.strip_suffix("->"))
        .or_else(|| prefix.strip_suffix('.'))?
        .trim_end();
    identifier_at(prefix, prefix.len()).map(|(name, _)| name.to_owned())
}

fn identifier_char_at(source: &str, byte: usize) -> bool {
    source[byte..]
        .chars()
        .next()
        .is_some_and(is_identifier_char)
}

pub fn is_identifier_char(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(
        path: &str,
        source: &str,
        at: usize,
        buffers: &[(&str, &str)],
    ) -> Option<(PathBuf, String)> {
        let request = DefinitionRequest {
            current_path: PathBuf::from(path),
            current_source: source.to_owned(),
            cursor_byte: at,
            workspace_files: Vec::new(),
            open_buffers: buffers
                .iter()
                .map(|(path, text)| (PathBuf::from(path), (*text).to_owned()))
                .collect(),
        };
        let location = find_definition(&SymbolIndex::default(), &request)?;
        let text = if location.path == request.current_path {
            source.to_owned()
        } else {
            buffers
                .iter()
                .find(|(path, _)| Path::new(path) == location.path)?
                .1
                .to_owned()
        };
        // Return the line of the definition so assertions read naturally.
        let line_start = text[..location.byte_range.start]
            .rfind('\n')
            .map_or(0, |i| i + 1);
        let line_end = text[location.byte_range.start..]
            .find('\n')
            .map_or(text.len(), |i| location.byte_range.start + i);
        Some((location.path, text[line_start..line_end].trim().to_owned()))
    }

    #[test]
    fn every_language_query_compiles() {
        for language in [
            "rs", "py", "js", "jsx", "ts", "tsx", "go", "c", "cpp", "java", "sh",
        ] {
            assert!(supports(language), "{language} query failed to compile");
        }
    }

    #[test]
    fn identifier_works_at_middle_and_end() {
        assert_eq!(identifier_at("let hello = 1", 6), Some(("hello", 4..9)));
        assert_eq!(identifier_at("hello", 5), Some(("hello", 0..5)));
        assert_eq!(identifier_at(" ", 1), None);
    }

    #[test]
    fn rust_local_shadows_function() {
        let source = "fn helper() {}\nfn main() { let helper = 3; dbg!(helper); }\n";
        let at = source.rfind("helper").unwrap();
        let (_, line) = definition("/w/main.rs", source, at, &[]).unwrap();
        assert!(line.contains("let helper = 3"), "{line}");
    }

    #[test]
    fn rust_local_out_of_scope_falls_back_to_item() {
        let source = "fn helper() {}\nfn a() { let helper = 1; }\nfn b() { helper(); }\n";
        let at = source.rfind("helper").unwrap();
        let (_, line) = definition("/w/main.rs", source, at, &[]).unwrap();
        assert_eq!(line, "fn helper() {}");
    }

    #[test]
    fn rust_pattern_bindings_skip_variants() {
        let symbols = file_symbols(
            "rs",
            "fn f(o: Option<u8>) { if let Some(value) = o { value; } }",
        );
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"value"));
        assert!(!names.contains(&"Some"));
        assert!(names.contains(&"o"));
    }

    #[test]
    fn qualifier_prefers_matching_module_file() {
        let source = "fn main() { util::run(); }";
        let buffers = [
            ("/w/util.rs", "pub fn run() {}"),
            ("/w/other.rs", "pub fn run() {}"),
        ];
        let at = source.find("run").unwrap();
        let (path, _) = definition("/w/main.rs", source, at, &buffers).unwrap();
        assert_eq!(path, PathBuf::from("/w/util.rs"));
    }

    #[test]
    fn python_parameters_methods_and_fields() {
        let source = "class Stats:\n    def __init__(self, values):\n        self.values = values\n\n    def mean(self):\n        return sum(self.values)\n\ndef run(values):\n    return Stats(values).mean()\n";
        let at = source.rfind("values").unwrap();
        let (_, line) = definition("/w/stats.py", source, at, &[]).unwrap();
        assert_eq!(line, "def run(values):");
        let at = source.rfind("mean").unwrap();
        let (_, line) = definition("/w/stats.py", source, at, &[]).unwrap();
        assert_eq!(line, "def mean(self):");
        let at = source.find("sum(self.values)").unwrap() + "sum(self.".len();
        let (_, line) = definition("/w/stats.py", source, at, &[]).unwrap();
        assert_eq!(line, "self.values = values");
    }

    #[test]
    fn python_cross_file_definition_beats_import() {
        let source = "from helpers import median\n\nprint(median([1]))\n";
        let at = source.rfind("median").unwrap();
        let buffers = [("/w/helpers.py", "def median(values):\n    pass\n")];
        let (path, line) = definition("/w/main.py", source, at, &buffers).unwrap();
        assert_eq!(path, PathBuf::from("/w/helpers.py"));
        assert_eq!(line, "def median(values):");
        // Without the defining file, the import line is the best answer.
        let (path, _) = definition("/w/main.py", source, at, &[]).unwrap();
        assert_eq!(path, PathBuf::from("/w/main.py"));
    }

    #[test]
    fn typescript_and_javascript_definitions() {
        let source = "interface Point { x: number }\nconst dist = (p: Point) => p.x;\nfunction main() { const p = { x: 1 }; return dist(p); }\n";
        let at = source.rfind("dist").unwrap();
        let (_, line) = definition("/w/a.ts", source, at, &[]).unwrap();
        assert!(line.starts_with("const dist"), "{line}");
        let at = source.find("(p: Point)").unwrap() + 5;
        let (_, line) = definition("/w/a.ts", source, at, &[]).unwrap();
        assert!(line.starts_with("interface Point"), "{line}");
        let at = source.rfind("(p)").unwrap() + 1;
        let (_, line) = definition("/w/a.ts", source, at, &[]).unwrap();
        assert!(line.starts_with("function main"), "{line}");

        let js = "import { util } from './u.js';\nclass A { run() {} }\nnew A().run(util);\n";
        let at = js.rfind("run").unwrap();
        let (_, line) = definition("/w/a.js", js, at, &[]).unwrap();
        assert_eq!(line, "class A { run() {} }");
        let buffers = [("/w/u.ts", "export function util() {}\n")];
        let at = js.rfind("util").unwrap();
        let (path, _) = definition("/w/a.js", js, at, &buffers).unwrap();
        assert_eq!(path, PathBuf::from("/w/u.ts"));
    }

    #[test]
    fn go_c_java_definitions() {
        let go = "package main\ntype Server struct { port int }\nfunc (s *Server) Run() {}\nfunc main() { srv := &Server{}; srv.Run() }\n";
        let at = go.rfind("Run").unwrap();
        let (_, line) = definition("/w/main.go", go, at, &[]).unwrap();
        assert_eq!(line, "func (s *Server) Run() {}");
        let at = go.rfind("srv").unwrap();
        let (_, line) = definition("/w/main.go", go, at, &[]).unwrap();
        assert!(line.contains("srv := &Server{}"), "{line}");

        let c = "int add(int a, int b);\nint add(int a, int b) { return a + b; }\nint main(void) { return add(1, 2); }\n";
        let at = c.rfind("add").unwrap();
        let (_, line) = definition("/w/main.c", c, at, &[]).unwrap();
        assert!(line.contains("return a + b"), "{line}");

        let java = "class App {\n  int count;\n  void run(int times) { count += times; }\n}\n";
        let at = java.rfind("times").unwrap();
        let (_, line) = definition("/w/App.java", java, at, &[]).unwrap();
        assert!(line.starts_with("void run"), "{line}");
        let at = java.rfind("count").unwrap();
        let (_, line) = definition("/w/App.java", java, at, &[]).unwrap();
        assert_eq!(line, "int count;");
    }

    #[test]
    fn outline_lists_functions_and_types_only() {
        let symbols = file_symbols(
            "py",
            "X = 1\nclass A:\n    def f(self, y):\n        z = y\n",
        );
        let outline: Vec<_> = symbols
            .iter()
            .filter(|symbol| symbol.kind.is_outline())
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert_eq!(outline, ["A", "f"]);
    }

    #[test]
    fn expand_selection_grows_through_syntax_nodes() {
        let source = "fn main() { let total = add(1, 2); }";
        let caret = source.find("add").unwrap() + 1;
        let first = expand_selection("rs", source, caret..caret).unwrap();
        assert_eq!(&source[first.clone()], "add");
        let second = expand_selection("rs", source, first).unwrap();
        assert_eq!(&source[second], "add(1, 2)");
    }
}
