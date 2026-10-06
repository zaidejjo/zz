//! Context-aware autocompletion for ZZ.
//!
//! Four completion modes:
//! 1. **Dot access** (`obj.`) — resolves the object's type and returns its fields.
//! 2. **Scope completion** (bare identifier) — returns visible locals, globals,
//!    functions, structs, and keywords filtered by prefix.
//! 3. **Std root** (`std.`) — lists stdlib modules from the static table, no
//!    import or check result needed.
//! 4. **Import path** (`import std.ma|`) — completes `std.*` module paths
//!    inside import statements.

use std::collections::HashMap;

use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind, CompletionResponse};
use zz_checker::{CheckResult, FuncSig, StructSig, Type};
use zz_frontend::ast::{Expr, Program, Stmt};

use crate::convert::LineIndex;
use crate::lookup::resolve_type_of_expr;

// ── ZZ keywords ──────────────────────────────────────────────────────────

const KEYWORDS: &[&str] = &[
    "break", "continue", "defer", "else", "false", "for", "func", "if", "import", "match", "none",
    "return", "struct", "true", "while",
];

// ── Stdlib module names ──────────────────────────────────────────────────

const STDLIB_MODULES: &[&str] = &[
    "io", "str", "vec", "json", "http", "fs", "cenv", "math", "time", "sqlz", "db",
];

// ── Completion environment ─────────────────────────────────────────────────

/// Filesystem context for path-aware completion. Built per request from
/// the document URI (see the `textDocument/completion` handler); tests
/// pass `None` when the filesystem is irrelevant.
#[derive(Debug, Clone, Default)]
pub struct CompletionEnv {
    /// Directory of the document being completed (for relative imports).
    pub doc_dir: Option<std::path::PathBuf>,
    /// Seed bindings visible to the last check (dep + workspace globals).
    /// `CheckResult.bindings` holds only the file's own lets, so seeded
    /// globals (`lib.pi`) complete from here.
    pub seed_bindings: HashMap<String, Type>,
}

// ── Public API ───────────────────────────────────────────────────────────

/// Build completions for the given cursor position.
pub fn completions_for_position(
    program: &Program,
    source: &str,
    offset: u32,
    check_result: Option<&CheckResult>,
    cenv: Option<&CompletionEnv>,
) -> Option<CompletionResponse> {
    let ctx = detect_context(source, offset, cenv)?;
    let items = match ctx {
        CompletionContext::DotAccess {
            obj_name,
            partial_prefix,
        } => dot_access_completions(program, check_result, &obj_name, &partial_prefix, cenv),
        CompletionContext::Scope { partial_prefix } => {
            scope_completions(program, check_result, &partial_prefix, cenv)
        }
        CompletionContext::ImportPath {
            partial_prefix,
            replace_start,
        } => import_path_completions(
            source,
            &partial_prefix,
            replace_start,
            offset,
            cenv.and_then(|e| e.doc_dir.clone()),
        ),
        CompletionContext::ImportSelective {
            path,
            partial_prefix,
            replace_start,
        } => selective_member_completions(
            program,
            source,
            &path,
            &partial_prefix,
            replace_start,
            offset,
            check_result,
            cenv,
        ),
    };
    Some(CompletionResponse::Array(items))
}

/// Resolve extra detail for a completion item.
pub fn resolve_completion_detail(item: &mut CompletionItem, check_result: Option<&CheckResult>) {
    let cr = match check_result {
        Some(cr) => cr,
        None => return,
    };
    if let Some(sig) = cr.funcs.get(&item.label) {
        item.detail = Some(format_func_sig(&item.label, sig));
        if let Some(docs) = format_func_docs(sig) {
            item.documentation = Some(tower_lsp::lsp_types::Documentation::String(docs));
        }
    } else if let Some(sig) = cr.structs.get(&item.label) {
        item.detail = Some(format_struct_sig(&item.label, sig));
    }
}

// ── Context detection ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum CompletionContext {
    DotAccess {
        obj_name: String,
        partial_prefix: String,
    },
    Scope {
        partial_prefix: String,
    },
    ImportPath {
        partial_prefix: String,
        replace_start: usize,
    },
    ImportSelective {
        path: String,
        partial_prefix: String,
        replace_start: usize,
    },
}

fn detect_context(
    source: &str,
    offset: u32,
    cenv: Option<&CompletionEnv>,
) -> Option<CompletionContext> {
    // Defensive: clamp out-of-range offsets and floor to a char boundary
    // so a stale client position can never panic the handler.
    let mut offset = (offset as usize).min(source.len());
    while offset > 0 && !source.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &source[..offset];

    // Import contexts first: `import std.ma|` must not read as dot-access
    // on `std`, and `import std.math(P|` must not read as scope `P`.
    if let Some(kind) = import_completion_at(before, cenv) {
        return Some(match kind {
            ImportCompletionKind::Path {
                partial,
                replace_start,
            } => CompletionContext::ImportPath {
                partial_prefix: partial,
                replace_start,
            },
            ImportCompletionKind::Selective {
                path,
                partial,
                replace_start,
            } => CompletionContext::ImportSelective {
                path,
                partial_prefix: partial,
                replace_start,
            },
        });
    }

    let partial = extract_partial_identifier(before);
    let partial_prefix = partial.clone().unwrap_or_default();

    // String/comment guards come before dot-access: `"math.|"` is text,
    // not a receiver (completion inside a literal is always noise).
    if inside_string_literal(before) {
        return None;
    }
    if inside_line_comment(before) {
        return None;
    }

    let dot_check_offset = before.len() - partial_prefix.len();
    if dot_check_offset > 0 {
        let ch_before_dot = before.as_bytes().get(dot_check_offset - 1).copied();
        if ch_before_dot == Some(b'.') {
            let before_dot = &before[..dot_check_offset - 1];
            if let Some(obj_name) = extract_partial_identifier(before_dot) {
                return Some(CompletionContext::DotAccess {
                    obj_name,
                    partial_prefix,
                });
            }
        }
    }

    Some(CompletionContext::Scope { partial_prefix })
}

fn extract_partial_identifier(text: &str) -> Option<String> {
    let mut end = text.len();
    let bytes = text.as_bytes();
    while end > 0 {
        let ch = bytes[end - 1];
        if ch.is_ascii_alphanumeric() || ch == b'_' {
            end -= 1;
        } else {
            break;
        }
    }
    if end == text.len() {
        return None;
    }
    let word = &text[end..];
    if word.is_empty() {
        None
    } else {
        Some(word.to_string())
    }
}

/// Which import-list context the cursor sits in.
#[derive(Debug, Clone, PartialEq)]
enum ImportCompletionKind {
    /// Inside the module path: `import std.ma|`, `import |`.
    Path {
        partial: String,
        replace_start: usize,
    },
    /// Inside the selective list: `import std.math(P|`, `import std.math(PI, s|`.
    Selective {
        path: String,
        partial: String,
        replace_start: usize,
    },
}

/// Detect import-list completion contexts from the text before the cursor.
///
/// Only fires on the statement's first line (`import ...` with no newline
/// after it) and never inside the closed parens — `import std.math(PI)|`
/// is plain scope again. Returns the byte offset where the replacement
/// should start so items can carry an exact `textEdit`.
fn import_completion_at(
    before: &str,
    cenv: Option<&CompletionEnv>,
) -> Option<ImportCompletionKind> {
    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = &before[line_start..];
    let trimmed = line.trim_start();
    let after_import = trimmed.strip_prefix("import")?;
    // Require a real `import` keyword: `imports`, `importx` must not match.
    if !after_import.is_empty()
        && !after_import.starts_with(char::is_whitespace)
        && !after_import.starts_with('(')
    {
        return None;
    }
    let after = after_import.trim_start();
    let base = line_start + (line.len() - trimmed.len()) + "import".len();
    let after_start = base + (after_import.len() - after.len());

    // Selective list: `import <path>(a, b|`.
    if let Some(paren) = after.find('(') {
        let tail = &after[paren + 1..];
        // Closed or nested parens: the list is over.
        if tail.contains(')') || tail.contains('(') {
            return None;
        }
        // Path token = last whitespace-separated word before `(` —
        // covers `std.math(`, `std.math as m(`, and local `utils(`.
        let path = after[..paren]
            .split_whitespace()
            .last()
            .unwrap_or("")
            .to_string();
        if path.is_empty() {
            return None;
        }
        // Partial = trailing identifier run after the last comma.
        let after_comma = tail.rsplit(',').next().unwrap_or("");
        let segment = after_comma.trim_start();
        let seg_start_in_tail =
            tail.len() - after_comma.len() + (after_comma.len() - segment.len());
        let ident_len = segment
            .char_indices()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
            .map(|(i, c)| i + c.len_utf8())
            .last()
            .unwrap_or(0);
        // Trailing junk that is not an identifier prefix (e.g. `as ` with a
        // space, stray quotes) ends the completion context.
        if segment[ident_len..].trim_start().is_empty() {
            let partial = segment[..ident_len].to_string();
            let replace_start = after_start + paren + 1 + seg_start_in_tail;
            return Some(ImportCompletionKind::Selective {
                path,
                partial,
                replace_start,
            });
        }
        return None;
    }

    // Module path: identifier/dot/slash characters only.
    // `std` roots always complete; local paths complete when their first
    // segment resolves beside the document (otherwise `import foo` stays
    // plain scope — nothing to offer yet).
    // An empty path (`import |`) offers `std` plus local top-level modules.
    let path_ok = after
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '/');
    if path_ok
        && (after.is_empty() || after == "std" || after.starts_with("std.") || {
            let first = after.split(['.', '/']).next().unwrap_or("");
            cenv.and_then(|e| e.doc_dir.clone()).is_some_and(|dir| {
                !first.is_empty()
                    && (dir.join(first).with_extension("zz").is_file() || dir.join(first).is_dir())
            })
        })
    {
        return Some(ImportCompletionKind::Path {
            partial: after.to_string(),
            replace_start: after_start,
        });
    }
    None
}

fn inside_string_literal(text: &str) -> bool {
    let mut in_string = false;
    for ch in text.chars() {
        if ch == '"' {
            in_string = !in_string;
        }
    }
    in_string
}

fn inside_line_comment(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'/' && bytes[i + 1] == b'/' {
            return true;
        }
        i += 1;
    }
    false
}

// ── Import path + selective member completions ───────────────────────────

/// Range covering `replace_start..cursor` as an LSP range (for `textEdit`).
fn replace_range(source: &str, replace_start: usize, cursor: u32) -> tower_lsp::lsp_types::Range {
    let index = LineIndex::new(source);
    tower_lsp::lsp_types::Range {
        start: index.offset_to_position(source, replace_start.min(source.len()) as u32),
        end: index.offset_to_position(source, cursor.min(source.len() as u32)),
    }
}

/// Complete `std.*` module paths inside `import` statements.
///
/// Labels are full dotted paths (`std.math`) with a `textEdit` that replaces
/// exactly the typed path, so clients never double the prefix (`std.std.x`).
fn import_path_completions(
    source: &str,
    partial_prefix: &str,
    replace_start: usize,
    cursor: u32,
    doc_dir: Option<std::path::PathBuf>,
) -> Vec<CompletionItem> {
    let range = replace_range(source, replace_start, cursor);
    let text_edit = |new_text: String| {
        Some(tower_lsp::lsp_types::CompletionTextEdit::Edit(
            tower_lsp::lsp_types::TextEdit { range, new_text },
        ))
    };
    let mut items = Vec::new();
    if partial_prefix.is_empty() {
        items.push(CompletionItem {
            label: "std".to_string(),
            kind: Some(CompletionItemKind::MODULE),
            detail: Some("stdlib root".to_string()),
            text_edit: text_edit("std".to_string()),
            ..Default::default()
        });
        // Local top-level modules beside the document (`utils.zz` → `utils`).
        if let Some(dir) = doc_dir {
            let mut tops = local_tops(&dir);
            tops.sort();
            for top in tops {
                items.push(CompletionItem {
                    label: top.clone(),
                    kind: Some(CompletionItemKind::MODULE),
                    detail: Some("local module".to_string()),
                    text_edit: text_edit(top),
                    ..Default::default()
                });
            }
        }
        return items;
    }
    if partial_prefix == "std" || partial_prefix.starts_with("std.") {
        let mut seen = std::collections::HashSet::new();
        for module in zz_stdlib::STDLIB_MODULES {
            let full = format!("std.{module}");
            if full.starts_with(partial_prefix) && seen.insert(full.clone()) {
                items.push(CompletionItem {
                    label: full.clone(),
                    kind: Some(CompletionItemKind::MODULE),
                    detail: Some("stdlib module".to_string()),
                    text_edit: text_edit(full),
                    ..Default::default()
                });
            }
        }
        items.sort_by(|a, b| a.label.cmp(&b.label));
        return items;
    }
    // Local path: complete the last segment against the parent directory
    // (`import math_utils.l|` → `math_utils.lib`). Labels are full dotted
    // paths; the textEdit swaps the whole typed path so prefixes never
    // double. Only resolvable files are offered — the loader accepts
    // nothing else.
    if let Some(dir) = doc_dir {
        let (parent, last) = match partial_prefix.rsplit_once('.') {
            Some((p, l)) => (p, l),
            None => ("", partial_prefix),
        };
        let parent_dir = if parent.is_empty() {
            dir
        } else {
            dir.join(parent.replace('.', "/"))
        };
        if parent_dir.is_dir() {
            let mut names: Vec<String> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&parent_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().is_some_and(|e| e == "zz") {
                        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                            if stem.starts_with(last) {
                                let full = if parent.is_empty() {
                                    stem.to_string()
                                } else {
                                    format!("{parent}.{stem}")
                                };
                                names.push(full);
                            }
                        }
                    }
                    // Descend into matching subdirectories so
                    // `import math_utils|` discovers `math_utils.lib`
                    // (the loader only opens files — dirs alone are
                    // never offered, only their contents).
                    if p.is_dir() {
                        if let Some(sub) = p.file_name().and_then(|s| s.to_str()) {
                            if sub.starts_with(last) {
                                if let Ok(subs) = std::fs::read_dir(&p) {
                                    for sub_entry in subs.flatten() {
                                        let sp = sub_entry.path();
                                        if sp.extension().is_some_and(|e| e == "zz") {
                                            if let Some(stem) =
                                                sp.file_stem().and_then(|s| s.to_str())
                                            {
                                                let full = if parent.is_empty() {
                                                    format!("{sub}.{stem}")
                                                } else {
                                                    format!("{parent}.{sub}.{stem}")
                                                };
                                                names.push(full);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            names.sort();
            names.dedup();
            for full in names {
                items.push(CompletionItem {
                    label: full.clone(),
                    kind: Some(CompletionItemKind::MODULE),
                    detail: Some("local module".to_string()),
                    text_edit: text_edit(full),
                    ..Default::default()
                });
            }
        }
    }
    items
}

/// Top-level module stems beside `dir` (`utils.zz` → `utils`).
fn local_tops(dir: &std::path::Path) -> Vec<String> {
    let mut tops = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "zz") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    tops.push(stem.to_string());
                }
            }
        }
    }
    tops
}

/// Complete member names inside a selective import list:
/// `import std.math(P|` → `PI`, `sin`, …
///
/// Reads the static stdlib tables (same source the loader seeds from), so
/// answers never depend on check state. Non-`std` paths resolve through the
/// program's import aliases (`m` from `import std.math as m`); anything else
/// yields nothing rather than guessing.
#[allow(clippy::too_many_arguments)]
fn selective_member_completions(
    _program: &Program,
    source: &str,
    path: &str,
    partial_prefix: &str,
    replace_start: usize,
    cursor: u32,
    check_result: Option<&CheckResult>,
    cenv: Option<&CompletionEnv>,
) -> Vec<CompletionItem> {
    // Workspace / dependency namespace: members live in the seeded check
    // result under the import namespace (alias or last segment). The seed
    // is file-based, so it serves even while the current import statement
    // is still being typed — and when the program has no usable import
    // yet, the typed path is harvested on demand (generics included: they
    // deliberately skip bare value seeding, loader parity).
    if path != "std" && !path.starts_with("std.") {
        let ns = path.rsplit('.').next().unwrap_or(path);
        let prefix = format!("{ns}.");
        let range = replace_range(source, replace_start, cursor);
        let edit = |name: &str| {
            Some(tower_lsp::lsp_types::CompletionTextEdit::Edit(
                tower_lsp::lsp_types::TextEdit {
                    range,
                    new_text: name.to_string(),
                },
            ))
        };
        // (label, kind, detail) merged from the check seed and, when the
        // program cannot provide it, a fresh harvest of the typed file.
        let mut members: std::collections::HashMap<String, (CompletionItemKind, String)> =
            std::collections::HashMap::new();
        if let Some(cr) = check_result {
            for key in cr.funcs.keys() {
                if let Some(name) = key.strip_prefix(&prefix) {
                    if !name.contains('.') && name.starts_with(partial_prefix) {
                        members
                            .entry(name.to_string())
                            .or_insert((CompletionItemKind::FUNCTION, format!("{ns}.{name}")));
                    }
                }
            }
            for key in cr.structs.keys() {
                if let Some(name) = key.strip_prefix(&prefix) {
                    if !name.contains('.') && name.starts_with(partial_prefix) {
                        members
                            .entry(name.to_string())
                            .or_insert((CompletionItemKind::STRUCT, format!("{ns}.{name}")));
                    }
                }
            }
            for (key, ty) in cr.bindings.iter().chain(env_bindings(cenv)) {
                if let Some(name) = key.strip_prefix(&prefix) {
                    if !name.contains('.') && name.starts_with(partial_prefix) {
                        members
                            .entry(name.to_string())
                            .or_insert((CompletionItemKind::VARIABLE, format!("{name}: {ty}")));
                    }
                }
            }
        }
        // On-demand harvest: mid-typing the first import, the program has
        // no import statement to seed from — read the file beside the
        // document directly (cached per keystroke nowhere; target files
        // are small and this path only runs inside import parens).
        if members.is_empty() {
            if let Some(dir) = cenv.and_then(|e| e.doc_dir.clone()) {
                let segs: Vec<String> = path.split('.').map(|s| s.to_string()).collect();
                if let Some(file) = crate::deps::resolve_local_import(&dir, &segs) {
                    let file_ns = segs.last().cloned().unwrap_or_default();
                    let harvested = crate::deps::harvest_workspace_file(
                        &file,
                        &file_ns,
                        None,
                        &zz_stdlib::stdlib_funcs(),
                        0,
                    );
                    let hp = format!("{file_ns}.");
                    for key in harvested.funcs.keys() {
                        if let Some(name) = key.strip_prefix(&hp) {
                            if !name.contains('.') && name.starts_with(partial_prefix) {
                                members.entry(name.to_string()).or_insert((
                                    CompletionItemKind::FUNCTION,
                                    format!("{file_ns}.{name}"),
                                ));
                            }
                        }
                    }
                    for key in harvested.structs.keys() {
                        if let Some(name) = key.strip_prefix(&hp) {
                            if !name.contains('.') && name.starts_with(partial_prefix) {
                                members.entry(name.to_string()).or_insert((
                                    CompletionItemKind::STRUCT,
                                    format!("{file_ns}.{name}"),
                                ));
                            }
                        }
                    }
                    for (key, ty) in &harvested.bindings {
                        if let Some(name) = key.strip_prefix(&hp) {
                            if !name.contains('.') && name.starts_with(partial_prefix) {
                                members.entry(name.to_string()).or_insert((
                                    CompletionItemKind::VARIABLE,
                                    format!("{name}: {ty}"),
                                ));
                            }
                        }
                    }
                }
            }
        }
        let mut items: Vec<CompletionItem> = members
            .into_iter()
            .map(|(name, (kind, detail))| CompletionItem {
                label: name.clone(),
                kind: Some(kind),
                detail: Some(detail),
                text_edit: edit(&name),
                ..Default::default()
            })
            .collect();
        items.sort_by(|a, b| a.label.cmp(&b.label));
        return items;
    }
    if path == "std" {
        return Vec::new();
    }
    let module = match path.strip_prefix("std.") {
        Some(rest) => rest.to_string(),
        None => return Vec::new(),
    };
    let prefix = format!("std.{module}.");
    let range = replace_range(source, replace_start, cursor);
    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (key, sig) in zz_stdlib::stdlib_funcs_cached() {
        if let Some(name) = key.strip_prefix(&prefix) {
            if !name.contains('.')
                && name.starts_with(partial_prefix)
                && seen.insert(name.to_string())
            {
                let is_const = zz_stdlib::stdlib_consts_cached().contains_key(key);
                items.push(CompletionItem {
                    label: name.to_string(),
                    kind: Some(if is_const {
                        CompletionItemKind::CONSTANT
                    } else {
                        CompletionItemKind::FUNCTION
                    }),
                    detail: Some(format!("std.{module}.{name}")),
                    text_edit: Some(tower_lsp::lsp_types::CompletionTextEdit::Edit(
                        tower_lsp::lsp_types::TextEdit {
                            range,
                            new_text: name.to_string(),
                        },
                    )),
                    ..Default::default()
                });
                let _ = sig;
            }
        }
    }
    // Constants that have no func entry (defensive: the tables overlap today).
    for key in zz_stdlib::stdlib_consts_cached().keys() {
        if let Some(name) = key.strip_prefix(&prefix) {
            if !name.contains('.')
                && name.starts_with(partial_prefix)
                && seen.insert(name.to_string())
            {
                items.push(CompletionItem {
                    label: name.to_string(),
                    kind: Some(CompletionItemKind::CONSTANT),
                    detail: Some(format!("std.{module}.{name}")),
                    text_edit: Some(tower_lsp::lsp_types::CompletionTextEdit::Edit(
                        tower_lsp::lsp_types::TextEdit {
                            range,
                            new_text: name.to_string(),
                        },
                    )),
                    ..Default::default()
                });
            }
        }
    }
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items
}

/// Complete top-level stdlib module names after a bare `std.`.
///
/// Static table, no check result needed — `std.` can never resolve through
/// imports (no import statement names the `std` namespace itself).
fn std_root_completions(partial_prefix: &str) -> Vec<CompletionItem> {
    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();
    for module in zz_stdlib::STDLIB_MODULES {
        let top = module.split('.').next().unwrap_or(module);
        if top.starts_with(partial_prefix) && seen.insert(top.to_string()) {
            items.push(CompletionItem {
                label: top.to_string(),
                kind: Some(CompletionItemKind::MODULE),
                detail: Some(format!("stdlib module std.{top}")),
                insert_text: Some(top.to_string()),
                ..Default::default()
            });
        }
    }
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items
}

/// Seeded globals snapshot (`lib.pi` from deps/workspace).
/// `CheckResult.bindings` holds only the file's own lets by checker
/// design, so completion unions both sources.
fn env_bindings(cenv: Option<&CompletionEnv>) -> &HashMap<String, Type> {
    static EMPTY: std::sync::LazyLock<HashMap<String, Type>> =
        std::sync::LazyLock::new(HashMap::new);
    cenv.map(|e| &e.seed_bindings).unwrap_or(&EMPTY)
}

// ── Dot access completions ───────────────────────────────────────────────

fn dot_access_completions(
    program: &Program,
    check_result: Option<&CheckResult>,
    obj_name: &str,
    partial_prefix: &str,
    cenv: Option<&CompletionEnv>,
) -> Vec<CompletionItem> {
    // `std.` lists modules from the static table: no import statement ever
    // binds the `std` namespace, so the alias lookup below cannot serve it.
    if obj_name == "std" {
        return std_root_completions(partial_prefix);
    }
    let cr = match check_result {
        Some(cr) => cr,
        None => return Vec::new(),
    };

    // 1. Try struct field access (existing logic).
    let obj_type = resolve_obj_type(program, cr, obj_name);
    let struct_name = match obj_type.clone() {
        Some(Type::Struct(name, _)) => name,
        Some(other) => {
            // Receiver-typed method completions (`str.`, `vec.`, scalar ext
            // methods, ...): list `ns.method` entries from the merged funcs
            // table (inherent → extension → stdlib).
            let nss = match &other {
                Type::Str => vec!["str"],
                Type::Array(_) => vec!["vec"],
                Type::Option(_) => vec!["option"],
                Type::Result(_, _) => vec!["result"],
                Type::Int => vec!["int"],
                Type::Float => vec!["float"],
                Type::Bool => vec!["bool"],
                Type::Response | Type::HttpServer | Type::HttpRequest => vec!["http"],
                Type::TcpStream | Type::TcpListener => vec!["net"],
                Type::Json => vec!["json"],
                Type::Db => vec!["sqlz", "db"],
                Type::Chan => vec!["chan"],
                _ => vec![],
            };
            let mut items: Vec<CompletionItem> = Vec::new();
            for ns in nss {
                let prefix = format!("{ns}.");
                for k in cr.funcs.keys() {
                    if let Some(m) = k.strip_prefix(&prefix) {
                        if m.starts_with(partial_prefix) && !m.contains('.') {
                            items.push(CompletionItem {
                                label: m.to_string(),
                                kind: Some(CompletionItemKind::METHOD),
                                detail: Some(format!("{ns}.{m}")),
                                insert_text: Some(m.to_string()),
                                ..Default::default()
                            });
                        }
                    }
                }
            }
            if !items.is_empty() {
                items.sort_by(|a, b| a.label.cmp(&b.label));
                items.dedup_by(|a, b| a.label == b.label);
                return items;
            }
            // 2. Not a struct — check if obj_name is an imported stdlib module
            //    alias (e.g. `math` after `import std.math as math`).
            return stdlib_module_completions(program, cr, obj_name, partial_prefix, cenv);
        }
        _ => {
            // 2. Not a struct — check if obj_name is an imported stdlib module
            //    alias (e.g. `math` after `import std.math as math`).
            return stdlib_module_completions(program, cr, obj_name, partial_prefix, cenv);
        }
    };

    let sig = match cr.structs.get(&struct_name) {
        Some(s) => s,
        None => return Vec::new(),
    };

    let mut items: Vec<CompletionItem> = sig
        .fields
        .iter()
        .filter(|(fname, _)| fname.starts_with(partial_prefix))
        .map(|(fname, fty)| CompletionItem {
            label: fname.clone(),
            kind: Some(CompletionItemKind::FIELD),
            detail: Some(format!("{fname}: {fty}")),
            insert_text: Some(fname.clone()),
            ..Default::default()
        })
        .collect();
    // Struct methods (inherent + extensions, merged in `funcs`).
    let prefix = format!("{struct_name}.");
    for k in cr.funcs.keys() {
        if let Some(m) = k.strip_prefix(&prefix) {
            if m.starts_with(partial_prefix) && !m.contains('.') {
                items.push(CompletionItem {
                    label: m.to_string(),
                    kind: Some(CompletionItemKind::METHOD),
                    detail: Some(format!("{struct_name}.{m}")),
                    insert_text: Some(m.to_string()),
                    ..Default::default()
                });
            }
        }
    }
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items
}

/// Provide completions for module access (`math.`, `table.`, …).
///
/// When the user types `math.`, look up the import alias in the AST and
/// provide all functions from the corresponding module — stdlib namespaces
/// and harvested dependency namespaces alike.
fn stdlib_module_completions(
    program: &Program,
    cr: &CheckResult,
    obj_name: &str,
    partial_prefix: &str,
    cenv: Option<&CompletionEnv>,
) -> Vec<CompletionItem> {
    // Find the stdlib module for this alias.
    let module = find_stdlib_module_for_alias(program, obj_name);
    let module = match module {
        Some(m) => m,
        None => return Vec::new(),
    };

    // The checker registers functions as `math.abs`, `math.floor`, etc.
    // using the module's own name.  When the user uses an alias like
    // `import std.math as m`, we need to match against `math.*` keys and
    // present them as `m.*` completions.
    //
    // The import namespace itself is also tried: `import std.sqlz.postgres
    // as pg` registers `pg.connect`, so `pg.` completes from the `pg.`
    // prefix (likewise any other alias).
    let prefixes = [format!("{module}."), format!("{obj_name}.")];
    let _user_prefix = format!("{obj_name}.");
    let mut items: Vec<CompletionItem> = cr
        .funcs
        .keys()
        .filter_map(|k| {
            prefixes
                .iter()
                .find(|p| k.starts_with(p.as_str()))
                .map(|p| k[p.len()..].to_string())
        })
        .map(|func_name| CompletionItem {
            label: func_name.clone(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some(format!("{obj_name}.{func_name}")),
            documentation: Some(tower_lsp::lsp_types::Documentation::String(format!(
                "std.{module}.{func_name}"
            ))),
            insert_text: Some(func_name),
            ..Default::default()
        })
        .filter(|item| item.label.starts_with(partial_prefix))
        .collect();

    // Structs, type aliases, enums and globals under the same namespace
    // (`lib.Point`, `lib.VERSION`, …): `lib.` must serve everything `pub`.
    for (key, sig) in &cr.structs {
        if let Some(name) = prefixes.iter().find_map(|p| key.strip_prefix(p.as_str())) {
            if !name.contains('.') && name.starts_with(partial_prefix) {
                items.push(CompletionItem {
                    label: name.to_string(),
                    kind: Some(CompletionItemKind::STRUCT),
                    detail: Some(format_struct_sig(name, sig)),
                    insert_text: Some(name.to_string()),
                    ..Default::default()
                });
            }
        }
    }
    for (key, sig) in &cr.aliases {
        if let Some(name) = prefixes.iter().find_map(|p| key.strip_prefix(p.as_str())) {
            if !name.contains('.') && name.starts_with(partial_prefix) {
                items.push(CompletionItem {
                    label: name.to_string(),
                    kind: Some(CompletionItemKind::STRUCT),
                    detail: Some(format!("type {name} = {}", sig.target)),
                    insert_text: Some(name.to_string()),
                    ..Default::default()
                });
            }
        }
    }
    for (key, sig) in &cr.enums {
        if let Some(name) = prefixes.iter().find_map(|p| key.strip_prefix(p.as_str())) {
            if !name.contains('.') && name.starts_with(partial_prefix) {
                items.push(CompletionItem {
                    label: name.to_string(),
                    kind: Some(CompletionItemKind::ENUM),
                    detail: Some(format!("enum {name} ({} variants)", sig.variants.len())),
                    insert_text: Some(name.to_string()),
                    ..Default::default()
                });
            }
        }
    }
    for (name, ty) in cr.bindings.iter().chain(env_bindings(cenv)) {
        if let Some(short) = prefixes.iter().find_map(|p| name.strip_prefix(p.as_str())) {
            if !short.contains('.') && short.starts_with(partial_prefix) {
                items.push(CompletionItem {
                    label: short.to_string(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(format!("{short}: {ty}")),
                    insert_text: Some(short.to_string()),
                    ..Default::default()
                });
            }
        }
    }

    items.sort_by(|a, b| a.label.cmp(&b.label));
    items.dedup_by(|a, b| a.label == b.label);
    items
}

/// Walk the AST import statements to find which stdlib module an alias
/// maps to.  For example, `import std.math as math` maps alias `math`
/// to module `math`.
///
/// Falls back to the alias itself when some import statement binds it
/// (`import table` → `table`): dependency and local namespaces carry
/// their members under that prefix in the check seed, so `table.`
/// completes even though no `std.*` path is involved.
fn find_stdlib_module_for_alias(program: &Program, alias: &str) -> Option<String> {
    let mut fallback: Option<String> = None;
    for stmt in &program.stmts {
        if let Stmt::Import {
            path,
            alias: import_alias,
            ..
        } = stmt
        {
            // The effective namespace is the import alias, or the last
            // path segment if no explicit alias.
            let ns = import_alias
                .as_ref()
                .cloned()
                .or_else(|| path.last().cloned())
                .unwrap_or_default();
            if ns == alias {
                // Extract the module name from the path
                // (`std.math` → `math`, `std.sqlz.postgres` →
                // `sqlz.postgres`).
                if path.len() >= 2 && path[0] == "std" {
                    return Some(path[1..].join("."));
                }
                // Direct import like `import math` — assume it's a stdlib module.
                if path.len() == 1 && STDLIB_MODULES.contains(&path[0].as_str()) {
                    return Some(path[0].clone());
                }
                fallback = Some(ns);
            }
        }
    }
    fallback
}

fn resolve_obj_type(program: &Program, cr: &CheckResult, name: &str) -> Option<Type> {
    // 1. Check bindings.
    if let Some(ty) = cr.bindings.get(name) {
        return Some(ty.clone());
    }
    // 2. Check function name.
    if let Some(sig) = cr.funcs.get(name) {
        return Some(func_sig_to_type(sig));
    }
    // 3. Check struct name.
    if let Some(sig) = cr.structs.get(name) {
        // Generic parameters stay symbolic in hovers (`Box[T]`).
        let args = sig
            .generics
            .iter()
            .map(|g| Type::Named(g.clone()))
            .collect();
        return Some(Type::Struct(name.to_string(), args));
    }
    // 4. Walk AST for Decl.
    for stmt in &program.stmts {
        if let Stmt::Decl {
            name: ident, value, ..
        } = stmt
        {
            if ident.name == name {
                return resolve_type_of_expr(program, cr, value);
            }
        }
    }
    // 5. Check function params.
    for stmt in &program.stmts {
        if let Stmt::Func { params, body, .. } = stmt {
            for param in params {
                if param.name.name == name {
                    if let Some(ref ty) = param.ty {
                        return Some(type_from_annotation(ty));
                    }
                    return Some(Type::Unit);
                }
            }
            if let Some(ty) = find_local_in_block(body, name, cr) {
                return Some(ty);
            }
        }
    }
    None
}

fn find_local_in_block(
    block: &zz_frontend::ast::Block,
    name: &str,
    cr: &CheckResult,
) -> Option<Type> {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Decl {
                name: ident, value, ..
            } => {
                if ident.name == name {
                    return resolve_type_of_expr(
                        &Program {
                            stmts: vec![],
                            span: zz_frontend::span::Span::new(0, 0),
                        },
                        cr,
                        value,
                    );
                }
            }
            Stmt::For { vars, body, .. } => {
                for v in vars {
                    if v.name == name {
                        return Some(Type::Unit);
                    }
                }
                if let Some(ty) = find_local_in_block(body, name, cr) {
                    return Some(ty);
                }
            }
            Stmt::Expr(Expr::If { then, els, .. }) => {
                if let Some(ty) = find_local_in_block(then, name, cr) {
                    return Some(ty);
                }
                if let Some(Expr::Block(b)) = els.as_deref() {
                    if let Some(ty) = find_local_in_block(b, name, cr) {
                        return Some(ty);
                    }
                }
            }
            Stmt::Expr(Expr::While { body, .. }) => {
                if let Some(ty) = find_local_in_block(body, name, cr) {
                    return Some(ty);
                }
            }
            Stmt::Expr(Expr::Match { arms, .. }) => {
                for arm in arms {
                    if let Expr::Block(b) = &arm.body {
                        if let Some(ty) = find_local_in_block(b, name, cr) {
                            return Some(ty);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

// ── Scope completions ────────────────────────────────────────────────────

fn scope_completions(
    program: &Program,
    check_result: Option<&CheckResult>,
    partial_prefix: &str,
    cenv: Option<&CompletionEnv>,
) -> Vec<CompletionItem> {
    let mut items = Vec::new();

    // Keywords.
    for kw in KEYWORDS {
        if kw.starts_with(partial_prefix) {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some("keyword".to_string()),
                ..Default::default()
            });
        }
    }

    if let Some(cr) = check_result {
        // Global functions.
        for name in cr.funcs.keys() {
            if name.starts_with(partial_prefix) {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail: Some(format!("func {name}")),
                    ..Default::default()
                });
            }
        }
        // Global structs.
        for name in cr.structs.keys() {
            if name.starts_with(partial_prefix) {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::STRUCT),
                    detail: Some(format!("struct {name}")),
                    ..Default::default()
                });
            }
        }
        // Global bindings: own-file lets plus seeded dep/workspace globals.
        for (name, ty) in cr.bindings.iter().chain(env_bindings(cenv)) {
            if name.starts_with(partial_prefix) {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(format!("{name}: {ty}")),
                    ..Default::default()
                });
            }
        }
    }

    // Selectively-imported generics (`import utils.lib(area)`): no bare
    // value binding exists by loader design, yet bare CALLS typecheck via
    // the checker's import-alias maps — so the bare name must complete in
    // scope too. Non-generics already seed bare names; only fill the gap
    // where the qualified signature exists but no bare item does.
    if let Some(cr) = check_result {
        for stmt in &program.stmts {
            let Stmt::Import {
                path,
                alias,
                items: import_items,
                ..
            } = stmt
            else {
                continue;
            };
            if import_items.is_empty() {
                continue;
            }
            if path.first().map(String::as_str) == Some("std") {
                continue;
            }
            let ns = alias
                .as_ref()
                .cloned()
                .or_else(|| path.last().cloned())
                .unwrap_or_default();
            if ns.is_empty() {
                continue;
            }
            for item in import_items {
                let zz_frontend::ast::ImportItem::Named {
                    name, alias: ia, ..
                } = item
                else {
                    continue;
                };
                let target = ia.clone().unwrap_or_else(|| name.clone());
                if !target.starts_with(partial_prefix) {
                    continue;
                }
                if items.iter().any(|i| i.label == target) {
                    continue;
                }
                let qualified = format!("{ns}.{name}");
                if cr.funcs.contains_key(&qualified) {
                    items.push(CompletionItem {
                        label: target.clone(),
                        kind: Some(CompletionItemKind::FUNCTION),
                        detail: Some(format!("{qualified} (selective)")),
                        ..Default::default()
                    });
                }
            }
        }
    }

    // Stdlib module names (as imported aliases).
    for stmt in &program.stmts {
        if let Stmt::Import {
            path,
            alias: import_alias,
            ..
        } = stmt
        {
            let ns = import_alias
                .as_ref()
                .cloned()
                .or_else(|| path.last().cloned())
                .unwrap_or_default();
            if ns.starts_with(partial_prefix) {
                items.push(CompletionItem {
                    label: ns.clone(),
                    kind: Some(CompletionItemKind::MODULE),
                    detail: Some(format!("import {}", path.join("."))),
                    ..Default::default()
                });
            }
        }
    }

    // The `std` root itself: typing `st` should discover the stdlib
    // (`std.` then lists modules). Needs no import.
    if "std".starts_with(partial_prefix) {
        items.push(CompletionItem {
            label: "std".to_string(),
            kind: Some(CompletionItemKind::MODULE),
            detail: Some("stdlib root — `std.<module>`".to_string()),
            ..Default::default()
        });
    }

    // Local names from AST.
    collect_local_names(program, &mut items, partial_prefix);

    // Deduplicate by label.
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items.dedup_by(|a, b| a.label == b.label);
    items
}

fn collect_local_names(program: &Program, items: &mut Vec<CompletionItem>, prefix: &str) {
    for stmt in &program.stmts {
        collect_locals_in_stmt(stmt, items, prefix);
    }
}

fn collect_locals_in_stmt(stmt: &Stmt, items: &mut Vec<CompletionItem>, prefix: &str) {
    match stmt {
        Stmt::Func { params, body, .. } => {
            for param in params {
                if param.name.name.starts_with(prefix) {
                    let detail = match &param.ty {
                        Some(ty) => format!("param: {ty:?}"),
                        None => "param".to_string(),
                    };
                    items.push(CompletionItem {
                        label: param.name.name.clone(),
                        kind: Some(CompletionItemKind::VARIABLE),
                        detail: Some(detail),
                        insert_text: Some(param.name.name.clone()),
                        ..Default::default()
                    });
                }
            }
            collect_locals_in_block(body, items, prefix);
        }
        Stmt::Decl { name, .. } => {
            if name.name.starts_with(prefix) {
                items.push(CompletionItem {
                    label: name.name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(format!("let {}", name.name)),
                    insert_text: Some(name.name.clone()),
                    ..Default::default()
                });
            }
        }
        Stmt::For { vars, body, .. } => {
            for v in vars {
                if v.name.starts_with(prefix) {
                    items.push(CompletionItem {
                        label: v.name.clone(),
                        kind: Some(CompletionItemKind::VARIABLE),
                        detail: Some(format!("for var {}", v.name)),
                        insert_text: Some(v.name.clone()),
                        ..Default::default()
                    });
                }
            }
            collect_locals_in_block(body, items, prefix);
        }
        Stmt::Expr(expr) => collect_locals_in_expr(expr, items, prefix),
        _ => {}
    }
}

fn collect_locals_in_block(
    block: &zz_frontend::ast::Block,
    items: &mut Vec<CompletionItem>,
    prefix: &str,
) {
    for stmt in &block.stmts {
        collect_locals_in_stmt(stmt, items, prefix);
    }
}

fn collect_locals_in_expr(expr: &Expr, items: &mut Vec<CompletionItem>, prefix: &str) {
    match expr {
        Expr::Block(b) => collect_locals_in_block(b, items, prefix),
        Expr::If { then, els, .. } => {
            collect_locals_in_block(then, items, prefix);
            if let Some(Expr::Block(b)) = els.as_deref() {
                collect_locals_in_block(b, items, prefix);
            }
        }
        Expr::While { body, .. } => collect_locals_in_block(body, items, prefix),
        Expr::Match { arms, .. } => {
            for arm in arms {
                if let Expr::Block(b) = &arm.body {
                    collect_locals_in_block(b, items, prefix);
                }
            }
        }
        Expr::IfLet { then, els, .. } => {
            collect_locals_in_block(then, items, prefix);
            if let Some(Expr::Block(b)) = els.as_deref() {
                collect_locals_in_block(b, items, prefix);
            }
        }
        _ => {}
    }
}

// ── Formatting helpers ───────────────────────────────────────────────────

fn format_func_sig(name: &str, sig: &FuncSig) -> String {
    let params: Vec<String> = sig
        .params
        .iter()
        .map(|(n, t)| format!("{n}: {t}"))
        .collect();
    format!("func {name}({}) -> {}", params.join(", "), sig.ret)
}

fn format_func_docs(sig: &FuncSig) -> Option<String> {
    let params: Vec<String> = sig
        .params
        .iter()
        .map(|(n, t)| format!("{n}: {t}"))
        .collect();
    Some(format!(
        "```zz\nfunc({}) -> {}\n```",
        params.join(", "),
        sig.ret
    ))
}

fn format_struct_sig(name: &str, sig: &StructSig) -> String {
    let fields: Vec<String> = sig
        .fields
        .iter()
        .map(|(n, t)| format!("{n}: {t}"))
        .collect();
    format!("struct {name} {{ {} }}", fields.join(", "))
}

fn func_sig_to_type(sig: &FuncSig) -> Type {
    let params: Vec<Type> = sig.params.iter().map(|(_, t)| t.clone()).collect();
    Type::Func(params, Box::new(sig.ret.clone()))
}

fn type_from_annotation(ty: &zz_frontend::ast::Ty) -> Type {
    use zz_frontend::ast::TyKind;
    match &ty.kind {
        TyKind::Named(name, _generics) => {
            let name = name.clone();
            match name.as_str() {
                "int" => Type::Int,
                "float" => Type::Float,
                "bool" => Type::Bool,
                "str" => Type::Str,
                "void" | "unit" => Type::Unit,
                _ => Type::Struct(name, Vec::new()),
            }
        }
        TyKind::Int => Type::Int,
        TyKind::Float => Type::Float,
        TyKind::Bool => Type::Bool,
        TyKind::Str => Type::Str,
        TyKind::Unit => Type::Unit,
        TyKind::Array(inner) => Type::Array(Box::new(type_from_annotation(inner))),
        _ => Type::Unit,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use zz_checker::check_program;
    use zz_frontend::parse;

    fn check(source: &str) -> (Program, Option<CheckResult>) {
        let parsed = parse(source);
        let cr = check_program(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        (parsed.program, Some(cr))
    }

    /// Check with stdlib functions registered (for stdlib module tests).
    fn check_with_stdlib(source: &str) -> (Program, Option<CheckResult>) {
        let parsed = parse(source);
        let mut funcs = zz_stdlib::stdlib_funcs();
        // Register each stdlib module under its own name (e.g. math.abs).
        for module in zz_stdlib::STDLIB_MODULES {
            let _ = zz_stdlib::register_module_namespace(
                module,
                module,
                &mut funcs,
                &mut std::collections::HashMap::new(),
            );
        }
        let cr = check_program(
            &parsed.program,
            HashMap::new(),
            funcs,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        (parsed.program, Some(cr))
    }

    // ── Context detection ─────────────────────────────────────────────

    #[test]
    fn detect_scope_empty_prefix() {
        let src = "x := 1\n";
        let ctx = detect_context(src, src.len() as u32, None);
        assert_eq!(
            ctx,
            Some(CompletionContext::Scope {
                partial_prefix: "".into()
            })
        );
    }

    #[test]
    fn detect_scope_partial() {
        let src = "xy := 1\nxz := 2\n";
        let ctx = detect_context(src, 2, None);
        assert_eq!(
            ctx,
            Some(CompletionContext::Scope {
                partial_prefix: "xy".into()
            })
        );
    }

    #[test]
    fn detect_dot_access() {
        let src = "struct Point { x: int }\np := Point{ x: 1 }\np.";
        let ctx = detect_context(src, src.len() as u32, None);
        assert_eq!(
            ctx,
            Some(CompletionContext::DotAccess {
                obj_name: "p".into(),
                partial_prefix: "".into(),
            })
        );
    }

    #[test]
    fn detect_dot_access_with_partial() {
        let src = "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x";
        let ctx = detect_context(src, src.len() as u32, None);
        assert_eq!(
            ctx,
            Some(CompletionContext::DotAccess {
                obj_name: "p".into(),
                partial_prefix: "x".into(),
            })
        );
    }

    #[test]
    fn detect_inside_string_returns_none() {
        let src = r#"x := "hello world""#;
        let ctx = detect_context(src, 12, None);
        assert_eq!(ctx, None);
    }

    // ── Scope completions ─────────────────────────────────────────────

    #[test]
    fn scope_keywords() {
        let src = "x := 1\n";
        let (program, cr) = check(src);
        let items = scope_completions(&program, cr.as_ref(), "ret", None);
        assert!(items.iter().any(|i| i.label == "return"));
        assert!(!items.iter().any(|i| i.label == "func"));
    }

    #[test]
    fn scope_globals() {
        let src = "func add(a: int, b: int) -> int { return a + b }\nx := 1\nxy := 2\n";
        let (program, cr) = check(src);
        let items = scope_completions(&program, cr.as_ref(), "x", None);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"x"));
        assert!(labels.contains(&"xy"));
        assert!(!labels.contains(&"add"));
    }

    #[test]
    fn scope_local_params() {
        let src = "func f(param_a: int) -> int {\n  let local_b = param_a\n  return local_b\n}\n";
        let (program, cr) = check(src);
        let items = scope_completions(&program, cr.as_ref(), "par", None);
        assert!(items.iter().any(|i| i.label == "param_a"));
    }

    #[test]
    fn scope_local_let() {
        let src = "alpha := 1\nbeta := 2\n";
        let (program, cr) = check(src);
        let items = scope_completions(&program, cr.as_ref(), "al", None);
        assert!(items.iter().any(|i| i.label == "alpha"));
        assert!(!items.iter().any(|i| i.label == "beta"));
    }

    // ── Dot access completions ────────────────────────────────────────

    #[test]
    fn dot_access_struct_fields() {
        let src = "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.";
        let (program, cr) = check(src);
        let items = dot_access_completions(&program, cr.as_ref(), "p", "", None);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"x"));
        assert!(labels.contains(&"y"));
        assert!(items
            .iter()
            .all(|i| i.kind == Some(CompletionItemKind::FIELD)));
    }

    #[test]
    fn dot_access_partial_filter() {
        let src = "struct Point { x: int, y: int, z: int }\np := Point{ x: 1, y: 2, z: 3 }\np.x";
        let (program, cr) = check(src);
        let items = dot_access_completions(&program, cr.as_ref(), "p", "x", None);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "x");
    }

    #[test]
    fn dot_access_non_struct_returns_empty() {
        let src = "x := 42\nx.";
        let (program, cr) = check(src);
        let items = dot_access_completions(&program, cr.as_ref(), "x", "", None);
        assert!(items.is_empty());
    }

    // ── Full completion pipeline ──────────────────────────────────────

    #[test]
    fn completions_for_position_scope() {
        let src = "myvar := 1\n";
        let (program, cr) = check(src);
        let resp = completions_for_position(&program, src, src.len() as u32, cr.as_ref(), None);
        let items = match resp {
            Some(CompletionResponse::Array(v)) => v,
            _ => panic!("expected array response"),
        };
        assert!(items.iter().any(|i| i.label == "myvar"));
    }

    #[test]
    fn completions_for_position_dot() {
        let src = "struct Foo { bar: int }\nf := Foo{ bar: 1 }\nf.";
        let (program, cr) = check(src);
        let resp = completions_for_position(&program, src, src.len() as u32, cr.as_ref(), None);
        let items = match resp {
            Some(CompletionResponse::Array(v)) => v,
            _ => panic!("expected array response"),
        };
        assert!(items.iter().any(|i| i.label == "bar"));
    }

    // ── Prefix matching ───────────────────────────────────────────────

    #[test]
    fn prefix_filtering() {
        let src =
            "struct Abc { a1: int, b1: int, a2: int }\nobj := Abc{ a1: 1, b1: 2, a2: 3 }\nobj.a";
        let (program, cr) = check(src);
        let resp = completions_for_position(&program, src, src.len() as u32, cr.as_ref(), None);
        let items = match resp {
            Some(CompletionResponse::Array(v)) => v,
            _ => panic!("expected array response"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"a1"));
        assert!(labels.contains(&"a2"));
        assert!(!labels.contains(&"b1"));
    }

    // ── Stdlib module completions ─────────────────────────────────────

    #[test]
    fn stdlib_module_dot_access() {
        let src = "import std.math\nmath.";
        let (program, cr) = check_with_stdlib(src);
        let resp = completions_for_position(&program, src, src.len() as u32, cr.as_ref(), None);
        let items = match resp {
            Some(CompletionResponse::Array(v)) => v,
            _ => panic!("expected array response"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"abs"), "expected 'abs' in {labels:?}");
        assert!(labels.contains(&"floor"), "expected 'floor' in {labels:?}");
        assert!(labels.contains(&"ceil"), "expected 'ceil' in {labels:?}");
        assert!(labels.contains(&"sqrt"), "expected 'sqrt' in {labels:?}");
        assert!(labels.contains(&"pow"), "expected 'pow' in {labels:?}");
    }

    #[test]
    fn stdlib_module_dot_access_partial() {
        let src = "import std.math\nmath.f";
        let (program, cr) = check_with_stdlib(src);
        let resp = completions_for_position(&program, src, src.len() as u32, cr.as_ref(), None);
        let items = match resp {
            Some(CompletionResponse::Array(v)) => v,
            _ => panic!("expected array response"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"floor"), "expected 'floor' in {labels:?}");
        assert!(!labels.contains(&"abs"), "should not contain 'abs'");
    }

    #[test]
    fn stdlib_module_aliased_dot_access() {
        let src = "import std.math as m\nm.";
        let (program, cr) = check_with_stdlib(src);
        let resp = completions_for_position(&program, src, src.len() as u32, cr.as_ref(), None);
        let items = match resp {
            Some(CompletionResponse::Array(v)) => v,
            _ => panic!("expected array response"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"abs"), "expected 'abs' in {labels:?}");
        assert!(labels.contains(&"pow"), "expected 'pow' in {labels:?}");
    }

    #[test]
    fn stdlib_module_name_in_scope() {
        let src = "import std.math as math\nmath";
        let (program, cr) = check_with_stdlib(src);
        let resp = completions_for_position(&program, src, src.len() as u32, cr.as_ref(), None);
        let items = match resp {
            Some(CompletionResponse::Array(v)) => v,
            _ => panic!("expected array response"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"math"), "expected 'math' in {labels:?}");
    }
}

#[cfg(test)]
mod audit_tests {
    //! Full completion audit: every documented trigger must serve.
    //! Each case resolves through `completions_for_position` (the exact
    //! function the `textDocument/completion` handler calls).
    use super::*;
    use zz_checker::check_program;
    use zz_frontend::parse;

    fn test_state() -> crate::state::GlobalState {
        crate::state::GlobalState::new()
    }

    /// Seeded check like the live recheck pipeline (imports applied).
    fn seeded(source: &str) -> (Program, Option<CheckResult>) {
        let state = test_state();
        let parsed = parse(source);
        let (ib, ifunc, is, ia, ie) = state.checker_seed_for(&parsed.program);
        let cr = check_program(&parsed.program, ib, ifunc, is, ia, ie);
        (parsed.program, Some(cr))
    }

    fn labels(resp: Option<CompletionResponse>) -> Vec<String> {
        match resp.expect("completion must return Some") {
            CompletionResponse::Array(v) => v.into_iter().map(|i| i.label).collect(),
            CompletionResponse::List(_) => panic!("expected array"),
        }
    }

    fn items_at(src: &str) -> Vec<CompletionItem> {
        let (program, cr) = seeded(src);
        let offset = src.len() as u32;
        match completions_for_position(&program, src, offset, cr.as_ref(), None).expect("some") {
            CompletionResponse::Array(v) => v,
            CompletionResponse::List(_) => panic!("expected array"),
        }
    }

    // ── 1. scope ──────────────────────────────────────────────────────
    #[test]
    fn audit_scope_keywords() {
        let src = "x := 1\nret";
        let (p, cr) = seeded(src);
        let l = labels(completions_for_position(
            &p,
            src,
            src.len() as u32,
            cr.as_ref(),
            None,
        ));
        assert!(l.contains(&"return".to_string()), "keywords serve: {l:?}");
    }

    #[test]
    fn audit_scope_locals() {
        let src = "myvar := 1\nmyv";
        let (p, cr) = seeded(src);
        let l = labels(completions_for_position(
            &p,
            src,
            src.len() as u32,
            cr.as_ref(),
            None,
        ));
        assert!(l.contains(&"myvar".to_string()), "locals serve: {l:?}");
    }

    #[test]
    fn audit_scope_std_root() {
        let src = "import std.math\ns";
        let l: Vec<String> = items_at(src).into_iter().map(|i| i.label).collect();
        assert!(l.contains(&"std".to_string()), "`std` discoverable: {l:?}");
    }

    #[test]
    fn audit_scope_import_ns() {
        let src = "import std.math\nm";
        let l: Vec<String> = items_at(src).into_iter().map(|i| i.label).collect();
        assert!(l.contains(&"math".to_string()), "import ns serves: {l:?}");
    }

    // ── 2. dot access ─────────────────────────────────────────────────
    #[test]
    fn audit_dot_std_modules() {
        let l: Vec<String> = items_at("x := std.").into_iter().map(|i| i.label).collect();
        for m in ["math", "str", "vec", "json", "http", "fs"] {
            assert!(l.contains(&m.to_string()), "std. lists {m}: {l:?}");
        }
    }

    #[test]
    fn audit_dot_std_partial() {
        let l: Vec<String> = items_at("x := std.ma")
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert!(l.contains(&"math".to_string()), "std.ma filters: {l:?}");
        assert!(
            !l.contains(&"str".to_string()),
            "std.ma excludes str: {l:?}"
        );
    }

    #[test]
    fn audit_dot_math_members() {
        let src = "import std.math\nx := math.";
        let all: Vec<CompletionItem> = items_at(src);
        let l: Vec<&str> = all.iter().map(|i| i.label.as_str()).collect();
        assert!(
            l.contains(&"abs") && l.contains(&"sin"),
            "math. members: {l:?}"
        );
        assert!(all
            .iter()
            .all(|i| i.kind == Some(CompletionItemKind::FUNCTION)));
        assert!(all
            .iter()
            .all(|i| i.detail.as_deref().is_some_and(|d| d.starts_with("math."))));
    }

    #[test]
    fn audit_dot_math_partial() {
        let src = "import std.math\nx := math.s";
        let l: Vec<String> = items_at(src).into_iter().map(|i| i.label).collect();
        assert!(l.contains(&"sin".to_string()), "math.s filters: {l:?}");
        assert!(
            !l.contains(&"abs".to_string()),
            "math.s excludes abs: {l:?}"
        );
    }

    #[test]
    fn audit_dot_aliased_ns() {
        let src = "import std.math as m\nx := m.";
        let l: Vec<String> = items_at(src).into_iter().map(|i| i.label).collect();
        assert!(l.contains(&"abs".to_string()), "aliased ns serves: {l:?}");
    }

    #[test]
    fn audit_dot_struct_fields() {
        let src = "struct P { x: int, y: int }\np := P{ x: 1, y: 2 }\nv := p.";
        let l: Vec<String> = items_at(src).into_iter().map(|i| i.label).collect();
        assert!(
            l.contains(&"x".to_string()) && l.contains(&"y".to_string()),
            "fields: {l:?}"
        );
    }

    // ── 3. import paths ───────────────────────────────────────────────
    #[test]
    fn audit_import_bare_offers_std() {
        let l: Vec<String> = items_at("import ").into_iter().map(|i| i.label).collect();
        assert!(
            l.contains(&"std".to_string()),
            "`import ` offers std: {l:?}"
        );
    }

    #[test]
    fn audit_import_std_dot_lists_modules() {
        let l: Vec<String> = items_at("import std.")
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert!(l.contains(&"std.math".to_string()), "modules: {l:?}");
        assert!(
            l.contains(&"std.sqlz.postgres".to_string()),
            "nested: {l:?}"
        );
    }

    #[test]
    fn audit_import_partial_path() {
        let all = items_at("import std.ma");
        let l: Vec<&str> = all.iter().map(|i| i.label.as_str()).collect();
        assert!(l.contains(&"std.math"), "partial path: {l:?}");
        assert!(!l.iter().any(|s| s.starts_with("std.st")), "no str: {l:?}");
        // textEdit must replace the whole typed path (no std.std.math).
        let item = all.iter().find(|i| i.label == "std.math").unwrap();
        assert!(item.text_edit.is_some(), "import items carry textEdit");
    }

    #[test]
    fn audit_import_nested_partial() {
        let l: Vec<String> = items_at("import std.sqlz.post")
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert!(
            l.contains(&"std.sqlz.postgres".to_string()),
            "nested partial: {l:?}"
        );
    }

    #[test]
    fn audit_import_local_not_hijacked() {
        // `import foo` is a local module: scope context, not import-path.
        let src = "import foo";
        let ctx = detect_context(src, src.len() as u32, None).expect("some ctx");
        assert!(
            matches!(ctx, CompletionContext::Scope { .. }),
            "local import stays scope: {ctx:?}"
        );
    }

    // ── 4. selective lists ────────────────────────────────────────────
    #[test]
    fn audit_selective_open_paren() {
        let l: Vec<String> = items_at("import std.math(")
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert!(l.contains(&"PI".to_string()), "consts: {l:?}");
        assert!(l.contains(&"abs".to_string()), "fns: {l:?}");
    }

    #[test]
    fn audit_selective_partial() {
        let l: Vec<String> = items_at("import std.math(P")
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert!(l.contains(&"PI".to_string()), "PI: {l:?}");
        assert!(!l.contains(&"abs".to_string()), "excludes abs: {l:?}");
    }

    #[test]
    fn audit_selective_after_comma() {
        let l: Vec<String> = items_at("import std.math(PI, s")
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert!(l.contains(&"sin".to_string()), "after comma: {l:?}");
    }

    #[test]
    fn audit_selective_fs() {
        let l: Vec<String> = items_at("import std.fs(read_")
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert!(l.contains(&"read_dir".to_string()), "fs members: {l:?}");
    }

    #[test]
    fn audit_selective_closed_not_context() {
        // Cursor past `)` is ordinary scope again.
        let src = "import std.math(PI)\nx := 1";
        let off = "import std.math(PI)".len() as u32;
        let ctx = detect_context(src, off, None).expect("some ctx");
        assert!(
            !matches!(ctx, CompletionContext::ImportSelective { .. }),
            "closed list is not selective: {ctx:?}"
        );
    }

    // ── 5. negatives ──────────────────────────────────────────────────
    #[test]
    fn audit_inside_string_no_completion() {
        let src = "x := \"math.\"";
        let ctx = detect_context(src, (src.len() - 1) as u32, None);
        assert!(ctx.is_none(), "string literal serves nothing: {ctx:?}");
    }
}

#[cfg(test)]
mod workspace_audit_tests {
    //! Workspace audit: the user's exact layout —
    //! `main.zz` + `math_utils/lib.zz` with `pub` items, plus a vendored
    //! registry-style dep. Diagnostics, member completion, selective
    //! lists and import paths must all agree with `zz check`.
    use super::*;
    use zz_checker::check_program;
    use zz_frontend::parse;

    fn write(path: &std::path::Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    const LIB_SRC: &str = "pub pi := 3.14159\npub e := 2.71828\npub func circle(r: float) -> float {\n    pi * r * r\n}\npub func twice<T: Num>(x: T) -> T {\n    x * x\n}\npub struct Point {\n    x: float,\n    y: float,\n}\n";

    /// Build <tmp>/proj with main_full.zz, main_sel.zz, math_utils/lib.zz.
    fn ws_project(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("zz-lsp-ws-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root.join("math_utils/lib.zz"), LIB_SRC);
        write(
            &root.join("main_full.zz"),
            "import math_utils.lib\nfunc main() {\n    println(lib.circle(1.0))\n    println(lib.pi)\n    p := lib.Point{ x: 1.0, y: 2.0 }\n    println(p.x)\n}\n",
        );
        write(
            &root.join("main_sel.zz"),
            "import math_utils.lib(pi, e)\nfunc main() {\n    println(pi)\n    println(e)\n}\n",
        );
        root
    }

    fn ws_state() -> crate::state::GlobalState {
        crate::state::GlobalState::new()
    }

    fn ws_errors(state: &crate::state::GlobalState, file: &std::path::Path) -> Vec<String> {
        let src = std::fs::read_to_string(file).unwrap();
        let parsed = parse(&src);
        let (ib, ifunc, is, ia, ie) = state.checker_seed_for_path(&parsed.program, Some(file));
        let cr = check_program(&parsed.program, ib, ifunc, is, ia, ie);
        cr.errors.into_iter().map(|e| e.message).collect()
    }

    fn ws_env(root: &std::path::Path) -> CompletionEnv {
        CompletionEnv {
            doc_dir: Some(root.to_path_buf()),
            seed_bindings: HashMap::new(),
        }
    }

    /// Check `src` as if it were `file` and return the program, result,
    /// and an env carrying the exact seed bindings the check saw.
    fn ws_check(
        state: &crate::state::GlobalState,
        file: &std::path::Path,
        src: &str,
    ) -> (Program, CheckResult, CompletionEnv) {
        let parsed = parse(src);
        let (ib, ifunc, is, ia, ie) = state.checker_seed_for_path(&parsed.program, Some(file));
        let cr = check_program(&parsed.program, ib.clone(), ifunc, is, ia, ie);
        let env = CompletionEnv {
            doc_dir: file.parent().map(|d| d.to_path_buf()),
            seed_bindings: ib,
        };
        (parsed.program, cr, env)
    }

    // ── diagnostics ───────────────────────────────────────────────────
    #[test]
    fn audit_ws_full_import_clean() {
        let root = ws_project("full");
        let state = ws_state();
        let messages = ws_errors(&state, &root.join("main_full.zz"));
        assert!(
            messages.is_empty(),
            "full workspace import clean: {messages:?}"
        );
    }

    #[test]
    fn audit_ws_selective_clean() {
        let root = ws_project("sel");
        let state = ws_state();
        let messages = ws_errors(&state, &root.join("main_sel.zz"));
        assert!(
            messages.is_empty(),
            "selective workspace import clean: {messages:?}"
        );
    }

    // ── member completion ─────────────────────────────────────────────
    #[test]
    fn audit_ws_dot_members_everything_pub() {
        let root = ws_project("dot");
        let state = ws_state();
        let file = root.join("main_full.zz");
        let src = std::fs::read_to_string(&file).unwrap();
        let probe = format!("{src}\nprobe := lib.");
        let (program, cr, cenv) = ws_check(&state, &file, &probe);
        let items = match completions_for_position(
            &program,
            &probe,
            probe.len() as u32,
            Some(&cr),
            Some(&cenv),
        )
        .expect("some")
        {
            CompletionResponse::Array(v) => v,
            CompletionResponse::List(_) => panic!("expected array"),
        };
        let find = |l: &str| items.iter().find(|i| i.label == l);
        assert_eq!(
            find("circle").and_then(|i| i.kind),
            Some(CompletionItemKind::FUNCTION),
            "funcs serve"
        );
        assert_eq!(
            find("pi").and_then(|i| i.kind),
            Some(CompletionItemKind::VARIABLE),
            "globals serve"
        );
        assert_eq!(
            find("Point").and_then(|i| i.kind),
            Some(CompletionItemKind::STRUCT),
            "structs serve"
        );
    }

    // ── selective lists ───────────────────────────────────────────────
    #[test]
    fn audit_ws_selective_list() {
        let root = ws_project("sellist");
        let state = ws_state();
        let file = root.join("main_sel.zz");
        // Simulate typing: file on disk is complete, but completion runs
        // on the buffer text with a partial selective list.
        let src = "import math_utils.lib(p\nfunc main() {\n    println(pi)\n}\n";
        let (program, cr, cenv) = ws_check(&state, &file, src);
        // Cursor sits right after the partial `p`, not at end of buffer.
        let offset = "import math_utils.lib(p".len() as u32;
        let items = match completions_for_position(&program, src, offset, Some(&cr), Some(&cenv))
            .expect("some")
        {
            CompletionResponse::Array(v) => v,
            CompletionResponse::List(_) => panic!("expected array"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(
            labels.contains(&"pi"),
            "selective ws list serves pi: {labels:?}"
        );
        assert!(!labels.contains(&"circle"), "filters by prefix: {labels:?}");
    }

    // ── import paths ──────────────────────────────────────────────────
    #[test]
    fn audit_ws_import_path_lists_lib() {
        let root = ws_project("path");
        let src = "import math_utils.";
        let parsed = parse(src);
        let cenv = ws_env(&root);
        let items = match completions_for_position(
            &parsed.program,
            src,
            src.len() as u32,
            None,
            Some(&cenv),
        )
        .expect("some")
        {
            CompletionResponse::Array(v) => v,
            CompletionResponse::List(_) => panic!("expected array"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(
            labels.contains(&"math_utils.lib"),
            "path lists lib: {labels:?}"
        );
    }

    #[test]
    fn audit_ws_import_bare_discovers_nested() {
        let root = ws_project("nested");
        let src = "import math_utils";
        let parsed = parse(src);
        let cenv = ws_env(&root);
        let items = match completions_for_position(
            &parsed.program,
            src,
            src.len() as u32,
            None,
            Some(&cenv),
        )
        .expect("some")
        {
            CompletionResponse::Array(v) => v,
            CompletionResponse::List(_) => panic!("expected array"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(
            labels.contains(&"math_utils.lib"),
            "bare discovers nested: {labels:?}"
        );
    }

    #[test]
    fn audit_ws_import_empty_offers_std_and_local() {
        let root = ws_project("empty");
        let src = "import ";
        let parsed = parse(src);
        let cenv = ws_env(&root);
        let items = match completions_for_position(
            &parsed.program,
            src,
            src.len() as u32,
            None,
            Some(&cenv),
        )
        .expect("some")
        {
            CompletionResponse::Array(v) => v,
            CompletionResponse::List(_) => panic!("expected array"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"std"), "empty offers std: {labels:?}");
        assert!(
            labels.iter().any(|l| !l.ends_with(".zz") && *l != "std"),
            "empty offers locals: {labels:?}"
        );
    }

    // ── generics (selective skips bare seeding by design) ───────────────
    #[test]
    fn audit_ws_selective_list_midtyping_generic() {
        // Buffer holds ONLY the half-typed import: the program offers no
        // import to seed from, so the typed path harvests on demand.
        let root = ws_project("genmid");
        let state = ws_state();
        let file = root.join("main_sel.zz");
        let src = "import math_utils.lib(tw";
        let (program, cr, _) = ws_check(&state, &file, src);
        let env = CompletionEnv {
            doc_dir: Some(root.clone()),
            seed_bindings: HashMap::new(),
        };
        let offset = src.len() as u32;
        let items = match completions_for_position(&program, src, offset, Some(&cr), Some(&env))
            .expect("some")
        {
            CompletionResponse::Array(v) => v,
            CompletionResponse::List(_) => panic!("expected array"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(
            labels.contains(&"twice"),
            "generic mid-typing serves: {labels:?}"
        );
    }

    #[test]
    fn audit_ws_bare_scope_generic_selective() {
        // Complete import line; cursor on a bare call prefix in the body.
        let root = ws_project("genbare");
        let state = ws_state();
        let file = root.join("main_sel.zz");
        let src2 = "import math_utils.lib(twice)\nfunc main() {\n    y := tw";
        let (program, cr, env) = ws_check(&state, &file, src2);
        let offset = src2.len() as u32;
        let items = match completions_for_position(&program, src2, offset, Some(&cr), Some(&env))
            .expect("some")
        {
            CompletionResponse::Array(v) => v,
            CompletionResponse::List(_) => panic!("expected array"),
        };
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"twice"), "bare generic serves: {labels:?}");
    }

    #[test]
    fn audit_ws_dot_member_generic() {
        let root = ws_project("gendot");
        let state = ws_state();
        let file = root.join("main_full.zz");
        let src = std::fs::read_to_string(&file).unwrap();
        let probe = format!("{src}\nprobe := lib.tw");
        let (program, cr, env) = ws_check(&state, &file, &probe);
        let offset = probe.len() as u32;
        let items = match completions_for_position(&program, &probe, offset, Some(&cr), Some(&env))
            .expect("some")
        {
            CompletionResponse::Array(v) => v,
            CompletionResponse::List(_) => panic!("expected array"),
        };
        assert!(
            items.iter().any(|i| i.label == "twice"),
            "dot generic serves"
        );
    }

    // ── vendored dep ──────────────────────────────────────────────────
    #[test]
    fn audit_dep_full_cycle() {
        // Registry-style dep via vendor/: diagnostics + members + selective.
        let root = std::env::temp_dir().join(format!("zz-lsp-depws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write(&root.join("zz.toml"), "[package]\nname = \"proj\"\nversion = \"0.1.0\"\n\n[dependencies]\ntable2 = \"^0.1.0\"\n");
        write(
            &root.join("vendor/table2/zz.toml"),
            "[package]\nname = \"table2\"\nversion = \"0.1.0\"\n",
        );
        write(
            &root.join("vendor/table2/src/table2.zz"),
            "pub func render(rows: [[str]]) -> str {\n    rows[0][0]\n}\n",
        );
        let main = root.join("main.zz");
        write(
            &main,
            "import table2\nfunc main() {\n    println(table2.render([[\"a\"]]))\n}\n",
        );
        let state = ws_state();
        let messages = ws_errors(&state, &main);
        assert!(messages.is_empty(), "dep import clean: {messages:?}");

        // member completion through the seeded check
        let src = std::fs::read_to_string(&main).unwrap();
        let probe = format!("{src}\nprobe := table2.");
        let (program, cr, cenv) = ws_check(&state, &main, &probe);
        let items = match completions_for_position(
            &program,
            &probe,
            probe.len() as u32,
            Some(&cr),
            Some(&cenv),
        )
        .expect("some")
        {
            CompletionResponse::Array(v) => v,
            CompletionResponse::List(_) => panic!("expected array"),
        };
        assert!(
            items.iter().any(|i| i.label == "render"),
            "dep members serve"
        );
    }
}
