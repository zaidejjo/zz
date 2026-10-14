//! Code action provider: maps tiered FixIt suggestions to LSP quick-fixes.

use std::collections::HashMap;

use tower_lsp::lsp_types::{
    CodeAction, CodeActionKind, CodeActionParams, CodeActionResponse, Diagnostic, Position, Range,
    TextEdit, Url, WorkspaceEdit,
};
use zz_frontend::ast::{Program, Stmt};
use zz_frontend::diag::{FixIt, FixSafety, RawDiag};

use crate::convert::span_to_range;
use crate::state::GlobalState;

/// Extract code actions from diagnostics in the requested range.
///
/// Each diagnostic carries a stashed `RawDiag` (serialized as JSON) in its
/// `.data` field. We deserialize it, iterate over its `FixIt` list, and
/// produce one `CodeAction` per fixit.
///
/// - `FixSafety::Safe` fixes produce a single "Apply fix" action.
/// - `FixSafety::Ambiguous` fixes with N alternatives produce N actions,
///   one per candidate, so the user can pick in the editor UI.
pub fn code_actions_for_range(
    params: &CodeActionParams,
    source: &str,
    file_uri: &Url,
) -> CodeActionResponse {
    let mut actions = CodeActionResponse::new();

    for diag in &params.context.diagnostics {
        let Some(raw) = extract_raw_diag(diag) else {
            continue;
        };

        for fixit in &raw.fixits {
            match fixit.safety {
                FixSafety::Safe => {
                    if let Some(action) = make_quickfix(fixit, diag, source, file_uri) {
                        actions.push(action.into());
                    }
                }
                FixSafety::Ambiguous if fixit.alternatives.len() > 1 => {
                    // One action per candidate so the editor shows a pick list.
                    for alt in &fixit.alternatives {
                        if let Some(action) =
                            make_ambiguous_quickfix(fixit, alt, diag, source, file_uri)
                        {
                            actions.push(action.into());
                        }
                    }
                }
                FixSafety::Ambiguous => {
                    // Single alternative despite being marked ambiguous — treat as safe.
                    if let Some(action) = make_quickfix(fixit, diag, source, file_uri) {
                        actions.push(action.into());
                    }
                }
            }
        }
    }

    actions
}

/// Deserialize the stashed `RawDiag` from a diagnostic's `.data` field.
fn extract_raw_diag(diag: &Diagnostic) -> Option<RawDiag> {
    let data = diag.data.as_ref()?;
    serde_json::from_value(data.clone()).ok()
}

/// Build a single `CodeAction` for an unambiguous fix.
fn make_quickfix(
    fixit: &FixIt,
    diag: &Diagnostic,
    source: &str,
    file_uri: &Url,
) -> Option<CodeAction> {
    let text_edit = TextEdit {
        range: span_to_range(source, fixit.span),
        new_text: fixit.replacement.clone(),
    };

    let mut changes = HashMap::new();
    changes.insert(file_uri.clone(), vec![text_edit]);

    Some(CodeAction {
        title: format!("{}: `{}`", fixit.message, fixit.replacement),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diag.clone()]),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }),
        ..Default::default()
    })
}

/// Build a `CodeAction` for one candidate of an ambiguous fix.
fn make_ambiguous_quickfix(
    fixit: &FixIt,
    candidate: &str,
    diag: &Diagnostic,
    source: &str,
    file_uri: &Url,
) -> Option<CodeAction> {
    let text_edit = TextEdit {
        range: span_to_range(source, fixit.span),
        new_text: candidate.to_string(),
    };

    let mut changes = HashMap::new();
    changes.insert(file_uri.clone(), vec![text_edit]);

    Some(CodeAction {
        title: format!("{}: `{}`", fixit.message, candidate),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diag.clone()]),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }),
        ..Default::default()
    })
}

/// Maximum import candidates offered per undefined name (checker
/// precedent: more than a few is a common word, not a missed import).
const MAX_IMPORT_CANDIDATES: usize = 3;

/// One place `name` can be imported from: full `import m` or selective
/// `import m(name)`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ImportCandidate {
    module: String,
    member: Option<String>,
}

impl ImportCandidate {
    fn statement(&self) -> String {
        match &self.member {
            Some(member) => format!("import {}({member})", self.module),
            None => format!("import {}", self.module),
        }
    }
}

/// Synthesize "add import" quickfixes for `undefined variable` diagnostics
/// whose providers live outside the stdlib: workspace modules and
/// installed packages.
///
/// The checker already emits insert fixits for `std.*` providers, which
/// flow through `code_actions_for_range` untouched; this covers the rest.
/// Bare `circle` offers `import lib(circle)`; dotted `lib.circle` with an
/// unimported head offers `import lib`. Candidates already imported, and
/// imports the checker already suggested, are skipped.
pub fn auto_import_actions(
    state: &GlobalState,
    file_uri: &Url,
    source: &str,
    program: &Program,
    diagnostics: &[Diagnostic],
) -> CodeActionResponse {
    let mut actions = CodeActionResponse::new();
    let mut offered: Vec<String> = Vec::new();
    // Imports the checker's own fixits already add (dedupe).
    for diag in diagnostics {
        if let Some(raw) = extract_raw_diag(diag) {
            for fixit in &raw.fixits {
                if fixit.message == "add import" {
                    offered.push(fixit.replacement.trim().to_string());
                }
            }
        }
    }
    for diag in diagnostics {
        let Some(name) = undefined_name(&diag.message) else {
            continue;
        };
        for candidate in find_import_candidates(state, file_uri, program, &name) {
            let statement = candidate.statement();
            if offered.contains(&statement) {
                continue;
            }
            offered.push(statement.clone());
            let range = import_insert_range(source);
            let mut changes = HashMap::new();
            changes.insert(
                file_uri.clone(),
                vec![TextEdit {
                    range,
                    new_text: format!("{statement}\n"),
                }],
            );
            actions.push(
                CodeAction {
                    title: format!("add import: `{statement}`"),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![diag.clone()]),
                    edit: Some(WorkspaceEdit {
                        changes: Some(changes),
                        ..Default::default()
                    }),
                    ..Default::default()
                }
                .into(),
            );
            if offered.len() >= MAX_IMPORT_CANDIDATES * 2 {
                break;
            }
        }
    }
    actions
}

/// Extract the variable name from an `undefined variable \`X\`` message.
fn undefined_name(message: &str) -> Option<String> {
    message
        .strip_prefix("undefined variable `")
        .and_then(|rest| rest.strip_suffix('`'))
        .map(|name| name.to_string())
}

/// Find importable providers of `name`: workspace `pub` items first,
/// installed-package members second.
fn find_import_candidates(
    state: &GlobalState,
    file_uri: &Url,
    program: &Program,
    name: &str,
) -> Vec<ImportCandidate> {
    let (head, member) = match name.split_once('.') {
        Some((head, member)) => (head.to_string(), Some(member.to_string())),
        None => (name.to_string(), None),
    };
    // Usages under an already-imported head are member errors, not
    // missing imports.
    if member.is_some() && head_imported(program, &head) {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    candidates.extend(workspace_candidates(
        state,
        file_uri,
        program,
        &head,
        member.as_deref(),
    ));
    if candidates.len() < MAX_IMPORT_CANDIDATES {
        candidates.extend(dep_candidates(state, file_uri, &head, member.as_deref()));
    }
    candidates.truncate(MAX_IMPORT_CANDIDATES);
    candidates
}

/// Whether `program` imports head `ns` (`import a.b` exposes `b`,
/// `import a.b as x` exposes `x`).
fn head_imported(program: &Program, head: &str) -> bool {
    for stmt in &program.stmts {
        let Stmt::Import { path, alias, .. } = stmt else {
            continue;
        };
        if alias.as_deref() == Some(head) {
            return true;
        }
        if alias.is_none() && path.last().is_some_and(|last| last == head) {
            return true;
        }
    }
    false
}

/// Workspace providers: `pub` funcs/structs/aliases/enums whose name (or
/// enclosing module's last segment) matches.
fn workspace_candidates(
    state: &GlobalState,
    file_uri: &Url,
    program: &Program,
    head: &str,
    member: Option<&str>,
) -> Vec<ImportCandidate> {
    // (uri, program, source, module path) with fresh buffers first.
    let mut files: Vec<(Url, Program, String, Option<String>)> = Vec::new();
    for entry in state.documents.iter() {
        let doc = entry.value();
        if let Some(doc_program) = doc.program.clone() {
            let module = module_path_for(state, file_uri, entry.key());
            files.push((entry.key().clone(), doc_program, doc.source.clone(), module));
        }
    }
    {
        let index = state.module_index.read().unwrap();
        for (entry_uri, entry) in &index.entries {
            if files.iter().any(|(u, _, _, _)| u == entry_uri) {
                continue;
            }
            let module = index.uri_to_module.get(entry_uri).cloned();
            let entry_program = match &entry.program {
                Some(p) => p.clone(),
                None => zz_frontend::parse(&entry.source).program,
            };
            files.push((
                entry_uri.clone(),
                entry_program,
                entry.source.clone(),
                module,
            ));
        }
    }

    let imported_modules: Vec<String> = program
        .stmts
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::Import { path, .. } => Some(path.join(".")),
            _ => None,
        })
        .collect();

    let mut out = Vec::new();
    for (uri, file_program, _source, module) in &files {
        if uri == file_uri {
            continue;
        }
        let Some(module) = module else { continue };
        if imported_modules.iter().any(|m| m == module) {
            continue;
        }
        let last_is_head = module.split('.').last().is_some_and(|last| last == head);
        for (item, _kind) in pub_importables(file_program) {
            if let Some(member) = member {
                // Dotted usage: the head names the module.
                if last_is_head && item == member {
                    out.push(ImportCandidate {
                        module: module.clone(),
                        member: None,
                    });
                    break;
                }
            } else if item == head {
                // Bare usage: selective import binds the name directly.
                out.push(ImportCandidate {
                    module: module.clone(),
                    member: Some(head.to_string()),
                });
                break;
            }
        }
        if out.len() >= MAX_IMPORT_CANDIDATES {
            break;
        }
    }
    out
}

/// Installed-package providers from warmed dep seeds (`mid.foo` keys).
fn dep_candidates(
    state: &GlobalState,
    file_uri: &Url,
    head: &str,
    member: Option<&str>,
) -> Vec<ImportCandidate> {
    let path = match file_uri.to_file_path() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let root = crate::deps::find_project_root(&path)
        .or_else(|| state.root.read().unwrap().clone())
        .unwrap_or_else(|| path.parent().map(|p| p.to_path_buf()).unwrap_or_default());
    let cache = state.dep_cache.read().unwrap();
    let Some(cached) = cache.get(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in cached.seed.funcs.keys() {
        consider_dep_key(&mut out, head, member, key);
        if out.len() >= MAX_IMPORT_CANDIDATES {
            return out;
        }
    }
    for key in cached.seed.structs.keys() {
        consider_dep_key(&mut out, head, member, key);
        if out.len() >= MAX_IMPORT_CANDIDATES {
            return out;
        }
    }
    for key in cached.seed.aliases.keys() {
        consider_dep_key(&mut out, head, member, key);
        if out.len() >= MAX_IMPORT_CANDIDATES {
            return out;
        }
    }
    for key in cached.seed.enums.keys() {
        consider_dep_key(&mut out, head, member, key);
        if out.len() >= MAX_IMPORT_CANDIDATES {
            return out;
        }
    }
    out.truncate(MAX_IMPORT_CANDIDATES);
    // Prefer shorter (closer) module paths for stable output.
    out.sort_by(|a, b| a.module.len().cmp(&b.module.len()));
    out.dedup();
    out
}

/// Test one dep-seed key (`mid.foo`) against the wanted name.
fn consider_dep_key(out: &mut Vec<ImportCandidate>, head: &str, member: Option<&str>, key: &str) {
    let Some((module, item)) = key.rsplit_once('.') else {
        return;
    };
    if let Some(member) = member {
        if (module == head || module.split('.').last() == Some(head)) && item == member {
            out.push(ImportCandidate {
                module: module.to_string(),
                member: None,
            });
        }
    } else if item == head {
        out.push(ImportCandidate {
            module: module.to_string(),
            member: Some(head.to_string()),
        });
    }
}

/// Top-level `pub` items importable from another file.
fn pub_importables(program: &Program) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    for stmt in &program.stmts {
        match stmt {
            Stmt::Func { name, pub_, .. } if *pub_ => out.push((name.join("."), "func")),
            Stmt::Struct { name, pub_, .. } if *pub_ => out.push((name.join("."), "struct")),
            Stmt::TypeAlias { name, pub_, .. } if *pub_ => out.push((name.join("."), "alias")),
            Stmt::Enum { name, pub_, .. } if *pub_ => out.push((name.join("."), "enum")),
            _ => {}
        }
    }
    out
}

/// Resolve a file's importable module path: index first, derived second.
fn module_path_for(state: &GlobalState, current_uri: &Url, uri: &Url) -> Option<String> {
    if let Some(module) = state.module_index.read().unwrap().uri_to_module.get(uri) {
        return Some(module.clone());
    }
    let root = state.root.read().unwrap().clone().or_else(|| {
        current_uri
            .to_file_path()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    })?;
    let path = uri.to_file_path().ok()?;
    crate::cross_file::module_path_for_file(&path, &root)
}

/// Insertion range for a new import: after the trailing import block, or
/// file top when there are no imports (never splits header comments from
/// the code they describe... rather: never lands mid-file).
fn import_insert_range(source: &str) -> Range {
    let mut line: u32 = 0;
    for (i, text) in source.lines().enumerate() {
        let trimmed = text.trim_start();
        if trimmed.starts_with("import ") || trimmed.starts_with("import(") {
            line = i as u32 + 1;
        } else if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        } else {
            break;
        }
    }
    let position = Position { line, character: 0 };
    Range {
        start: position,
        end: position,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp::lsp_types::{
        CodeActionContext, CodeActionOrCommand, DiagnosticSeverity, Position, Range,
    };
    use zz_frontend::diag::{FixIt, RawDiag, Severity};
    use zz_frontend::span::Span;

    fn make_test_diag(raw: &RawDiag) -> Diagnostic {
        let data = serde_json::to_value(raw).unwrap();
        Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 5,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            code: None,
            code_description: None,
            source: Some("zz".to_string()),
            message: raw.message.clone(),
            related_information: None,
            tags: None,
            data: Some(data),
        }
    }

    fn make_params(diags: Vec<Diagnostic>, uri: &Url) -> CodeActionParams {
        CodeActionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 5,
                    character: 0,
                },
            },
            context: CodeActionContext {
                diagnostics: diags,
                only: None,
                trigger_kind: None,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    fn extract_action(item: &CodeActionOrCommand) -> &CodeAction {
        match item {
            CodeActionOrCommand::CodeAction(a) => a,
            _ => panic!("expected CodeAction"),
        }
    }

    #[test]
    fn safe_fix_produces_single_action() {
        let raw = RawDiag {
            severity: Severity::Error,
            message: "undefined variable `x`".into(),
            span: Some(Span::new(10, 11)),
            notes: vec![],
            fixits: vec![FixIt::safe(Span::new(10, 11), "_x", "rename to")],
            secondary: None,
        };
        let diag = make_test_diag(&raw);
        let uri: Url = "file:///test.zz".parse().unwrap();
        let params = make_params(vec![diag], &uri);

        let actions = code_actions_for_range(&params, "hello world\n", &uri);
        assert_eq!(actions.len(), 1);
        let action = extract_action(&actions[0]);
        assert!(action.title.contains("_x"));
        assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
    }

    #[test]
    fn ambiguous_fix_produces_one_action_per_candidate() {
        let raw = RawDiag {
            severity: Severity::Error,
            message: "undefined variable `prntlnn`".into(),
            span: Some(Span::new(0, 8)),
            notes: vec![],
            fixits: vec![FixIt::ambiguous(
                Span::new(0, 8),
                "println",
                "replace variable",
                vec!["println".into(), "println!".into()],
            )],
            secondary: None,
        };
        let diag = make_test_diag(&raw);
        let uri: Url = "file:///test.zz".parse().unwrap();
        let params = make_params(vec![diag], &uri);

        let actions = code_actions_for_range(&params, "prntlnn\n", &uri);
        assert_eq!(actions.len(), 2);
        let titles: Vec<String> = actions
            .iter()
            .map(|a| extract_action(a).title.clone())
            .collect();
        assert!(titles.iter().any(|t| t.contains("println")));
        assert!(titles.iter().any(|t| t.contains("println!")));
    }

    #[test]
    fn std_add_import_fixit_flows_to_quickfix() {
        // End-to-end: checker emits `add import` for `math.sin` without
        // imports; the existing fixit path must surface an insert action.
        use crate::state::GlobalState;

        let src = "func main() {\n    y := math.sin(1.0)\n    println(y)\n}\n";
        let state = GlobalState::new();
        let program = zz_frontend::parse(src).program;
        let (ib, ifunc, is, ia, ie) = state.checker_seed_for(&program);
        let cr = zz_checker::check_program(&program, ib, ifunc, is, ia, ie);
        let raw = cr
            .errors
            .iter()
            .find(|d| d.fixits.iter().any(|f| f.message == "add import"))
            .expect("checker suggests the std import");
        let fixit = raw
            .fixits
            .iter()
            .find(|f| f.message == "add import")
            .unwrap();
        assert_eq!(fixit.replacement, "import std.math\n");

        let diag = Diagnostic {
            range: span_to_range(src, raw.span.unwrap()),
            severity: Some(DiagnosticSeverity::ERROR),
            code: None,
            code_description: None,
            source: Some("zz".to_string()),
            message: raw.message.clone(),
            related_information: None,
            tags: None,
            data: Some(serde_json::to_value(raw).unwrap()),
        };
        let uri: Url = "file:///main.zz".parse().unwrap();
        let params = make_params(vec![diag], &uri);

        let actions = code_actions_for_range(&params, src, &uri);
        let titles = action_titles(&actions);
        assert!(
            titles.iter().any(|t| t.contains("import std.math")),
            "std import offered: {titles:?}"
        );
        let action = extract_action(
            actions
                .iter()
                .find(|a| extract_action(a).title.contains("std.math"))
                .unwrap(),
        );
        let edits = action.edit.as_ref().unwrap().changes.as_ref().unwrap()[&uri].clone();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "import std.math\n");
        assert_eq!(
            edits[0].range.start,
            Position {
                line: 0,
                character: 0
            }
        );
    }

    fn auto_import_state(lib_src: &str, main_src: &str) -> (GlobalState, Url, Program) {
        let state = GlobalState::new();
        let lib_uri: Url = "file:///lib.zz".parse().unwrap();
        state.update_document(lib_uri.clone(), 1, lib_src.to_string());
        {
            let entry = crate::cross_file::parse_file_entry(
                lib_uri.clone(),
                lib_src.to_string(),
                "lib".to_string(),
            );
            let mut index = state.module_index.write().unwrap();
            index
                .module_to_uri
                .insert("lib".to_string(), lib_uri.clone());
            index
                .uri_to_module
                .insert(lib_uri.clone(), "lib".to_string());
            index.entries.insert(lib_uri.clone(), entry);
        }
        let main_uri: Url = "file:///main.zz".parse().unwrap();
        state.update_document(main_uri.clone(), 1, main_src.to_string());
        let program = zz_frontend::parse(main_src).program;
        (state, main_uri, program)
    }

    fn undefined_diag(name: &str) -> Diagnostic {
        make_test_diag(&RawDiag {
            severity: Severity::Error,
            message: format!("undefined variable `{name}`"),
            span: Some(Span::new(0, 1)),
            notes: vec![],
            fixits: vec![],
            secondary: None,
        })
    }

    fn action_titles(actions: &CodeActionResponse) -> Vec<String> {
        actions
            .iter()
            .map(|a| extract_action(a).title.clone())
            .collect()
    }

    #[test]
    fn auto_import_offers_selective_workspace_import() {
        let lib_src = "pub func circle(r: float) -> float {\n    return r\n}\n";
        let main_src = "func main() {\n    y := circle(1.0)\n    println(y)\n}\n";
        let (state, main_uri, program) = auto_import_state(lib_src, main_src);
        let diags = vec![undefined_diag("circle")];

        let actions = auto_import_actions(&state, &main_uri, main_src, &program, &diags);
        let titles = action_titles(&actions);
        assert!(
            titles.iter().any(|t| t.contains("import lib(circle)")),
            "selective import offered: {titles:?}"
        );
        // Inserted at file top (no imports present).
        let action = extract_action(&actions[0]);
        let edits = action.edit.as_ref().unwrap().changes.as_ref().unwrap()[&main_uri].clone();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "import lib(circle)\n");
    }

    #[test]
    fn auto_import_skips_private_items() {
        let lib_src = "func hidden() -> int {\n    return 1\n}\n";
        let main_src = "func main() {\n    y := hidden()\n    println(y)\n}\n";
        let (state, main_uri, program) = auto_import_state(lib_src, main_src);
        let diags = vec![undefined_diag("hidden")];

        let actions = auto_import_actions(&state, &main_uri, main_src, &program, &diags);
        assert!(actions.is_empty(), "private items never offered");
    }

    #[test]
    fn auto_import_skips_already_imported_module() {
        let lib_src = "pub func circle(r: float) -> float {\n    return r\n}\n";
        let main_src = "import lib\nfunc main() {\n    y := circle(1.0)\n    println(y)\n}\n";
        let (state, main_uri, program) = auto_import_state(lib_src, main_src);
        let diags = vec![undefined_diag("circle")];

        let actions = auto_import_actions(&state, &main_uri, main_src, &program, &diags);
        assert!(actions.is_empty(), "already-imported module skipped");
    }

    #[test]
    fn auto_import_offers_full_import_for_dotted_head() {
        let lib_src = "pub func circle(r: float) -> float {\n    return r\n}\n";
        let main_src = "func main() {\n    y := lib.circle(1.0)\n    println(y)\n}\n";
        let (state, main_uri, program) = auto_import_state(lib_src, main_src);
        let diags = vec![undefined_diag("lib.circle")];

        let actions = auto_import_actions(&state, &main_uri, main_src, &program, &diags);
        let titles = action_titles(&actions);
        assert!(
            titles
                .iter()
                .any(|t| t.contains("import lib") && !t.contains('(')),
            "full import offered: {titles:?}"
        );
    }

    #[test]
    fn auto_import_silent_when_head_imported() {
        let lib_src = "pub func circle(r: float) -> float {\n    return r\n}\n";
        let main_src = "import lib\nfunc main() {\n    y := lib.square(1.0)\n    println(y)\n}\n";
        let (state, main_uri, program) = auto_import_state(lib_src, main_src);
        let diags = vec![undefined_diag("lib.square")];

        let actions = auto_import_actions(&state, &main_uri, main_src, &program, &diags);
        assert!(actions.is_empty(), "member errors are not import errors");
    }

    #[test]
    fn auto_import_inserts_after_existing_imports() {
        let lib_src = "pub func circle(r: float) -> float {\n    return r\n}\n";
        let main_src =
            "import std.math\n\nfunc main() {\n    y := circle(1.0)\n    println(y)\n}\n";
        // `circle` must not resolve to the already-imported `std.math`.
        let (state, main_uri, program) = auto_import_state(lib_src, main_src);
        // Point the workspace module at a different path so the existing
        // `import std.math` does not count as importing it.
        state
            .module_index
            .write()
            .unwrap()
            .uri_to_module
            .insert("file:///lib.zz".parse().unwrap(), "shapes".to_string());
        state
            .module_index
            .write()
            .unwrap()
            .module_to_uri
            .insert("shapes".to_string(), "file:///lib.zz".parse().unwrap());
        let diags = vec![undefined_diag("circle")];

        let actions = auto_import_actions(&state, &main_uri, main_src, &program, &diags);
        let titles = action_titles(&actions);
        assert!(
            titles.iter().any(|t| t.contains("import shapes(circle)")),
            "offered: {titles:?}"
        );
        let action = extract_action(
            actions
                .iter()
                .find(|a| extract_action(a).title.contains("shapes"))
                .unwrap(),
        );
        let edits = action.edit.as_ref().unwrap().changes.as_ref().unwrap()[&main_uri].clone();
        assert_eq!(edits[0].range.start.line, 1, "insert after import block");
    }

    #[test]
    fn no_actions_when_data_missing() {
        let diag = Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 5,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            code: None,
            code_description: None,
            source: Some("zz".to_string()),
            message: "some error".into(),
            related_information: None,
            tags: None,
            data: None,
        };
        let uri: Url = "file:///test.zz".parse().unwrap();
        let params = make_params(vec![diag], &uri);
        let actions = code_actions_for_range(&params, "", &uri);
        assert!(actions.is_empty());
    }
}
