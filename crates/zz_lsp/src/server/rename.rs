use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use super::Backend;

pub(crate) async fn handle_prepare_rename(
    backend: &Backend,
    params: TextDocumentPositionParams,
) -> Result<Option<PrepareRenameResponse>> {
    let uri = &params.text_document.uri;
    let pos = params.position;

    let doc = match backend.state.documents.get(uri) {
        Some(doc) => doc.clone(),
        None => return Ok(None),
    };
    let program = match &doc.program {
        Some(p) => p,
        None => return Ok(None),
    };

    let offset = doc.line_index.position_to_offset(&doc.source, pos);
    let node = crate::lookup::find_node_at(program, &doc.source, offset);

    match &node.name {
        Some(name) => {
            let name_span = node.name_span.unwrap_or_else(|| {
                zz_frontend::span::Span::new(offset, offset + name.len() as u32)
            });
            Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
                range: doc.line_index.span_to_range(&doc.source, name_span),
                placeholder: name.clone(),
            }))
        }
        None => Ok(None),
    }
}

/// Short name of a possibly qualified definition (`Point.x` → `x`).
fn short_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

pub(crate) async fn handle_rename(
    backend: &Backend,
    params: RenameParams,
) -> Result<Option<WorkspaceEdit>> {
    let uri = &params.text_document_position.text_document.uri;
    let pos = params.text_document_position.position;
    let new_name = &params.new_name;

    let doc = match backend.state.documents.get(uri) {
        Some(doc) => doc.value().clone(),
        None => return Ok(None),
    };
    let program = match &doc.program {
        Some(p) => p.clone(),
        None => return Ok(None),
    };

    let offset = doc.line_index.position_to_offset(&doc.source, pos);
    // Shared cross-file scope: the defining file plus its importers,
    // fresh buffers preferred, foreign definition sites excluded.
    let scope = match crate::symbol_refs::resolve_symbol_scope(&backend.state, uri, offset) {
        Some(s) => s,
        None => return Ok(None),
    };
    let _ = program;

    // No-op renames stay silent.
    if new_name == &scope.name {
        return Ok(None);
    }

    // Collect the counted hits per file first: only files that gain
    // edits matter for the collision guard below (an untouched file's
    // same-named symbol lives in another scope entirely — most
    // importantly for project-global bindings, where every file is in
    // scope but only referencing files are edited).
    let mut touched: Vec<(
        &crate::symbol_refs::ScopedFile,
        Vec<crate::lookup::Reference>,
    )> = Vec::new();
    for file in &scope.files {
        let refs = crate::lookup::find_references_to_name_in_program(
            &file.program,
            &file.source,
            &scope.name,
        );
        let counted: Vec<crate::lookup::Reference> = refs
            .into_iter()
            .filter(|r| crate::symbol_refs::hit_counts_in_file(&scope, &file.uri, r, &file.source))
            .collect();
        if !counted.is_empty() {
            touched.push((file, counted));
        }
    }

    // Collision guard: renaming onto an existing definition silently
    // shadows or merges symbols. Exact matches block for values, plus
    // short-name matches when renaming a struct field (`Other.y`
    // collides with renaming `Point.x` to `y`). The rename aborts whole
    // (editors apply WorkspaceEdits atomically, so a partial rename
    // would be worse than none) with the colliding symbol named.
    let renaming_field = touched.iter().any(|(file, _)| {
        crate::lookup::collect_definitions(&file.program, &file.source)
            .values()
            .any(|d| d.kind == crate::lookup::DefKind::Field && short_name(&d.name) == scope.name)
    });
    for (file, _) in &touched {
        let defs = crate::lookup::collect_definitions(&file.program, &file.source);
        if let Some(hit) = defs
            .values()
            .find(|d| d.name == *new_name || (renaming_field && short_name(&d.name) == *new_name))
        {
            return Err(tower_lsp::jsonrpc::Error::invalid_params(format!(
                "rename blocked: `{new_name}` is already defined as `{}`",
                hit.name
            )));
        }
    }

    let mut changes: std::collections::HashMap<Url, Vec<TextEdit>> =
        std::collections::HashMap::new();
    for (file, counted) in &touched {
        let line_index = crate::convert::LineIndex::new(&file.source);
        let mut edits = Vec::new();
        for r in counted {
            // Qualified paths (`lib.circle`) rename the member segment
            // only — the qualifier stays.
            let span = crate::symbol_refs::rename_span_for(&file.source, r.span, &scope.name);
            edits.push(TextEdit {
                range: line_index.span_to_range(&file.source, span),
                new_text: new_name.clone(),
            });
        }
        if !edits.is_empty() {
            changes.insert(file.uri.clone(), edits);
        }
    }

    if changes.is_empty() {
        return Ok(None);
    }

    Ok(Some(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    }))
}
