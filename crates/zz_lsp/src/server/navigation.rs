use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::convert::LineIndex;
use zz_frontend::ast::{Expr, ImportItem, Stmt};

use super::find_struct_field_location;
use super::Backend;

pub(crate) async fn handle_goto_definition(
    backend: &Backend,
    params: GotoDefinitionParams,
) -> Result<Option<GotoDefinitionResponse>> {
    let uri = &params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;

    let doc = match backend.state.documents.get(uri) {
        Some(doc) => doc.clone(),
        None => return Ok(None),
    };
    let program = match &doc.program {
        Some(p) => p,
        None => return Ok(None),
    };

    let offset = doc.line_index.position_to_offset(&doc.source, pos);
    if let Some(resp) = collection_member_definition(backend, uri, program, &doc.source, offset) {
        return Ok(Some(resp));
    }
    let node = crate::lookup::find_node_at(program, &doc.source, offset);

    if let Some(Stmt::Import { path, items, .. }) = node.stmt {
        return Ok(import_definition(
            backend,
            uri,
            &doc.source,
            offset,
            path,
            items,
        ));
    }

    let name = match &node.name {
        Some(n) => n.clone(),
        None => return Ok(None),
    };

    let defs = crate::lookup::collect_definitions(program, &doc.source);
    let def = defs.values().find(|d| d.name == name);

    if def.is_none() {
        if let Some(check_result) = &doc.check_result {
            if let Some(Expr::Field {
                name: field, obj, ..
            }) = node.expr
            {
                if let Some(zz_checker::Type::Struct(ref struct_name, _)) =
                    crate::lookup::resolve_type_of_expr(program, check_result, obj)
                {
                    if let Some(resp) = find_struct_field_location(
                        check_result,
                        struct_name,
                        field,
                        program,
                        uri,
                        &doc.source,
                    ) {
                        return Ok(Some(resp));
                    }
                }
            }

            if let Some(Expr::Path { parts, .. }) = node.expr {
                if parts.len() >= 2 {
                    let obj_name = &parts[0];
                    let field = parts.last().unwrap();
                    if let Some(zz_checker::Type::Struct(ref struct_name, _)) =
                        check_result.bindings.get(obj_name)
                    {
                        if let Some(resp) = find_struct_field_location(
                            check_result,
                            struct_name,
                            field,
                            program,
                            uri,
                            &doc.source,
                        ) {
                            return Ok(Some(resp));
                        }
                    }
                }
            }
        }
    }

    let def = match def {
        Some(d) => d,
        None => return Ok(None),
    };

    Ok(Some(GotoDefinitionResponse::Scalar(Location {
        uri: uri.clone(),
        range: doc.line_index.span_to_range(&doc.source, def.span),
    })))
}

/// Resolve a go-to-definition request sitting on an import statement.
///
/// Three zones, finest first:
/// - selective member (`import lib(ci|rcle)`, including aliases and
///   `import lib(*)`): jumps to the member's definition inside the
///   target file, or the file itself when the member cannot be found;
/// - module path (`import math_utils.l|ib`, `import ti|ny`): jumps to
///   the target file — workspace files, workspace-relative paths, and
///   installed packages (`vendor/`, path deps) alike;
/// - anything else on the line: the target file.
fn import_definition(
    backend: &Backend,
    uri: &Url,
    source: &str,
    offset: u32,
    path: &[String],
    items: &[ImportItem],
) -> Option<GotoDefinitionResponse> {
    // `import collect.<name>`: module names don't map to files on disk.
    // Land on the first member's exported function instead.
    if path.len() == 2 && path[0] == "collect" {
        return collection_import_definition(backend, uri, &path[1]);
    }
    let target_uri = resolve_import_target(backend, uri, path)?;
    let (target_program, target_source) = load_target_program(backend, &target_uri)?;

    // Selective zone: cursor on a member (or alias) name?
    if let Some(member) = selective_member_at(source, offset, items) {
        let defs = crate::lookup::collect_definitions(&target_program, &target_source);
        let hit = defs.values().find(|d| d.name == member).or_else(|| {
            // Qualified definitions (`Point.dist`): unique suffix wins,
            // ambiguity falls back to the file.
            let mut suffix = defs
                .values()
                .filter(|d| d.name.rsplit('.').next() == Some(member.as_str()));
            let first = suffix.next()?;
            suffix.next().is_none().then_some(first)
        });
        let line_index = LineIndex::new(&target_source);
        let range = match hit {
            Some(def) => line_index.span_to_range(&target_source, def.span),
            None => Range::default(),
        };
        return Some(GotoDefinitionResponse::Scalar(Location {
            uri: target_uri,
            range,
        }));
    }

    // Path zone (or wildcard): the file itself. Prefer its first
    // definition when indexed so the cursor lands on code, not line 1.
    let line_index = LineIndex::new(&target_source);
    let range = {
        let index = backend.state.module_index.read().unwrap();
        index
            .entries
            .get(&target_uri)
            .and_then(|entry| entry.definitions.first())
            .map(|def| line_index.span_to_range(&target_source, def.span))
            .unwrap_or_default()
    };
    Some(GotoDefinitionResponse::Scalar(Location {
        uri: target_uri,
        range,
    }))
}

// ── Directory collections ([collect.*]) ─────────────────────────────────

/// Nearest `zz.toml` for the document and its parsed manifest.
fn manifest_for_uri(uri: &Url) -> Option<(std::path::PathBuf, zz_pm::manifest::Manifest)> {
    let path = uri.to_file_path().ok()?;
    let root = crate::deps::find_project_root(&path)?;
    let manifest = zz_pm::manifest::Manifest::load(&root.join("zz.toml")).ok()?;
    Some((root, manifest))
}

/// Map a namespace (`commands`, or an alias) back to its manifest key.
fn collection_name_for_ns(
    program: &zz_frontend::ast::Program,
    manifest: &zz_pm::manifest::Manifest,
    ns: &str,
) -> Option<String> {
    for stmt in &program.stmts {
        if let Stmt::Import { path, alias, .. } = stmt {
            if path.len() == 2 && path[0] == "collect" {
                let imported_ns = alias.as_deref().unwrap_or(path[1].as_str());
                if imported_ns == ns && manifest.collections.contains_key(&path[1]) {
                    return Some(path[1].clone());
                }
            }
        }
    }
    manifest
        .collections
        .contains_key(ns)
        .then(|| ns.to_string())
}

/// True when the quote at `index` opens a string (an even number of
/// unescaped quotes precede it).
fn quote_opens_string(source: &str, index: usize) -> bool {
    let mut count = 0;
    let mut escaped = false;
    for ch in source[..index].chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => count += 1,
            _ => {}
        }
    }
    count % 2 == 0
}

/// Extract `(namespace, member)` when the cursor sits on the string in
/// `<ns>.get("<member>")`.
fn collection_member_at(source: &str, offset: u32) -> Option<(String, String)> {
    let offset = (offset as usize).min(source.len());
    let open = source[..offset].rfind('"')?;
    if !quote_opens_string(source, open) {
        return None;
    }
    let rest = &source[open + 1..];
    let close = open + 1 + rest.find('"')?;
    if offset > close || rest[..close - open - 1].contains('\n') {
        return None;
    }
    let member = &source[open + 1..close];
    if member.is_empty()
        || !member
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !member
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let head = source[..open].trim_end();
    let dot = head.rfind(".get")?;
    if head[dot + 4..].trim() != "(" {
        return None;
    }
    let receiver = head[..dot].trim_end();
    let start = receiver
        .char_indices()
        .rev()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '.'))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let ns = &receiver[start..];
    if ns.is_empty() || ns.starts_with('.') || ns.ends_with('.') {
        return None;
    }
    Some((ns.to_string(), member.to_string()))
}

/// Go-to-definition for `commands.get("greet")`: jump to `greet.zz`'s
/// exported function (or the file start when it is missing).
fn collection_member_definition(
    backend: &Backend,
    uri: &Url,
    program: &zz_frontend::ast::Program,
    source: &str,
    offset: u32,
) -> Option<GotoDefinitionResponse> {
    let (ns, member) = collection_member_at(source, offset)?;
    let (root, manifest) = manifest_for_uri(uri)?;
    let collection = collection_name_for_ns(program, &manifest, &ns)?;
    let spec = manifest.collections.get(&collection)?;
    let members = spec.discover(&root).ok()?;
    let hit = members.iter().find(|m| m.name == member)?;
    member_location(backend, &hit.path, &spec.export)
}

/// Go-to-definition for `import collect.<name>`: the first member file.
fn collection_import_definition(
    backend: &Backend,
    uri: &Url,
    collection: &str,
) -> Option<GotoDefinitionResponse> {
    let (root, manifest) = manifest_for_uri(uri)?;
    let spec = manifest.collections.get(collection)?;
    let members = spec.discover(&root).ok()?;
    let first = members.first()?;
    member_location(backend, &first.path, &spec.export)
}

/// Location of `pub <export>` inside a member file (file start as fallback).
fn member_location(
    backend: &Backend,
    path: &std::path::Path,
    export: &str,
) -> Option<GotoDefinitionResponse> {
    let target_uri = Url::from_file_path(path).ok()?;
    let (program, source) = load_target_program(backend, &target_uri)?;
    let line_index = LineIndex::new(&source);
    let range = program
        .stmts
        .iter()
        .find_map(|stmt| match stmt {
            Stmt::Func {
                name,
                pub_: true,
                span,
                ..
            } if name.last().map(String::as_str) == Some(export) => Some(*span),
            _ => None,
        })
        .map(|span| line_index.span_to_range(&source, span))
        .unwrap_or_default();
    Some(GotoDefinitionResponse::Scalar(Location {
        uri: target_uri,
        range,
    }))
}

/// Which selective member (true name, not alias) sits under `offset`, if any.
fn selective_member_at(source: &str, offset: u32, items: &[ImportItem]) -> Option<String> {
    for item in items {
        let (name, alias, span) = match item {
            ImportItem::Named { name, alias, span } => (name, alias, span),
            ImportItem::Wildcard { .. } => continue,
        };
        if offset < span.start || offset >= span.end {
            continue;
        }
        let slice = source.get(span.start as usize..span.end as usize)?;
        // Alias first: in `PI as pi` the alias is what the cursor reads.
        if let Some(a) = alias {
            if find_word(slice, a).is_some_and(|pos| {
                let abs = span.start as usize + pos;
                offset as usize >= abs && (offset as usize) < abs + a.len()
            }) {
                return Some(name.clone());
            }
        }
        if find_word(slice, name).is_some_and(|pos| {
            let abs = span.start as usize + pos;
            offset as usize >= abs && (offset as usize) < abs + name.len()
        }) {
            return Some(name.clone());
        }
    }
    None
}

/// Byte offset of `needle` in `haystack` at identifier boundaries.
fn find_word(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let abs = start + pos;
        let before = haystack[..abs].chars().next_back();
        let after = haystack[abs + needle.len()..].chars().next();
        let boundary = |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        if boundary(before) && boundary(after) {
            return Some(abs);
        }
        start = abs + 1;
    }
    None
}

/// Resolve an import path to a file URI: workspace index and relative
/// paths first, installed packages (`vendor/`, path deps) second.
fn resolve_import_target(backend: &Backend, uri: &Url, path: &[String]) -> Option<Url> {
    let root = backend.state.root.read().unwrap().clone().or_else(|| {
        uri.to_file_path()
            .ok()
            .and_then(|p| crate::deps::find_project_root(&p))
    });
    let base = root.clone().or_else(|| {
        uri.to_file_path()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    })?;
    {
        let index = backend.state.module_index.read().unwrap();
        if let Some(target) = index.resolve_import(path, &base) {
            return Some(target);
        }
    }
    // Installed packages: `import tiny` / `import tiny.sub`.
    let head = path.first()?;
    let root = root?;
    let declared = crate::deps::read_dep_names(&root);
    let path_opt = declared
        .iter()
        .find(|(name, _)| name == head)
        .and_then(|(_, opt)| opt.as_deref());
    // Undeclared heads still resolve from vendor/ (single-file scripts
    // prototyping against a vendored tree); anything else stays None.
    let pkg_dir = crate::deps::resolve_dep_dir(&root, head, path_opt)?;
    let entry = crate::deps::entry_in(&pkg_dir, head)?;
    Url::from_file_path(&entry).ok()
}

/// Load a target file's program + source: open buffer wins, then the
/// workspace index, then disk.
fn load_target_program(
    backend: &Backend,
    target_uri: &Url,
) -> Option<(zz_frontend::ast::Program, String)> {
    if let Some(doc) = backend.state.documents.get(target_uri) {
        if let Some(program) = doc.program.clone() {
            return Some((program, doc.source.clone()));
        }
    }
    {
        let index = backend.state.module_index.read().unwrap();
        if let Some(entry) = index.entries.get(target_uri) {
            if let Some(program) = entry.program.clone() {
                return Some((program, entry.source.clone()));
            }
        }
    }
    let path = target_uri.to_file_path().ok()?;
    let source = std::fs::read_to_string(&path).ok()?;
    let program = zz_frontend::parse(&source).program;
    Some((program, source))
}

pub(crate) async fn handle_references(
    backend: &Backend,
    params: ReferenceParams,
) -> Result<Option<Vec<Location>>> {
    let uri = &params.text_document_position.text_document.uri;
    let pos = params.text_document_position.position;

    let doc = match backend.state.documents.get(uri) {
        Some(doc) => doc.value().clone(),
        None => return Ok(None),
    };
    let program = match &doc.program {
        Some(p) => p.clone(),
        None => return Ok(None),
    };
    let _ = program;

    let offset = doc.line_index.position_to_offset(&doc.source, pos);
    // Shared cross-file scope (same resolver as rename): defining file
    // plus importers, fresh buffers preferred, foreign definition sites
    // and unqualified foreign field hits excluded.
    let scope = match crate::symbol_refs::resolve_symbol_scope(&backend.state, uri, offset) {
        Some(s) => s,
        None => return Ok(None),
    };

    let mut locations: Vec<Location> = Vec::new();
    for file in &scope.files {
        let refs = match scope.field_target.clone() {
            Some((struct_name, field)) => {
                let open = backend.state.documents.get(&file.uri);
                let bindings = open
                    .as_ref()
                    .and_then(|doc| doc.check_result.as_ref().map(|cr| &cr.bindings));
                let resolver = crate::symbol_refs::FieldResolver::new(bindings);
                let refs = crate::lookup::find_field_references(
                    &file.program,
                    &file.source,
                    &struct_name,
                    &field,
                    &|obj| resolver.resolve(obj),
                );
                drop(open);
                refs
            }
            None => crate::lookup::find_references_to_name_in_program(
                &file.program,
                &file.source,
                &scope.name,
            ),
        };
        let line_index = LineIndex::new(&file.source);
        for r in &refs {
            if !crate::symbol_refs::hit_counts_in_file(&scope, &file.uri, r, &file.source) {
                continue;
            }
            locations.push(Location {
                uri: file.uri.clone(),
                range: line_index.span_to_range(&file.source, r.span),
            });
        }
    }

    Ok(Some(locations))
}

pub(crate) async fn handle_document_highlight(
    backend: &Backend,
    params: DocumentHighlightParams,
) -> Result<Option<Vec<DocumentHighlight>>> {
    let uri = &params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;

    let doc = match backend.state.documents.get(uri) {
        Some(doc) => doc.clone(),
        None => return Ok(None),
    };
    let program = match &doc.program {
        Some(p) => p,
        None => return Ok(None),
    };

    let offset = doc.line_index.position_to_offset(&doc.source, pos);
    let highlights = crate::lookup::find_highlights_in_program(program, &doc.source, offset);

    let result: Vec<DocumentHighlight> = highlights
        .iter()
        .map(|h| DocumentHighlight {
            range: doc.line_index.span_to_range(&doc.source, h.span),
            kind: Some(match h.kind {
                crate::lookup::HighlightKind::Read => DocumentHighlightKind::READ,
                crate::lookup::HighlightKind::Write => DocumentHighlightKind::WRITE,
            }),
        })
        .collect();

    Ok(Some(result))
}
