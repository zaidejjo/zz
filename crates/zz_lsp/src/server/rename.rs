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

    let mut changes: std::collections::HashMap<Url, Vec<TextEdit>> =
        std::collections::HashMap::new();
    for file in &scope.files {
        let refs = crate::lookup::find_references_to_name_in_program(
            &file.program,
            &file.source,
            &scope.name,
        );
        let line_index = crate::convert::LineIndex::new(&file.source);
        let mut edits = Vec::new();
        for r in &refs {
            if !crate::symbol_refs::hit_counts_in_file(&scope, &file.uri, r, &file.source) {
                continue;
            }
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
