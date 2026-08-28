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
        Some(doc) => doc.clone(),
        None => return Ok(None),
    };
    let program = match &doc.program {
        Some(p) => p,
        None => return Ok(None),
    };

    let offset = doc.line_index.position_to_offset(&doc.source, pos);
    let refs = crate::lookup::find_references_in_program(program, &doc.source, offset);

    if refs.is_empty() {
        return Ok(None);
    }

    let mut changes: std::collections::HashMap<Url, Vec<TextEdit>> =
        std::collections::HashMap::new();

    let edits: Vec<TextEdit> = refs
        .iter()
        .map(|r| TextEdit {
            range: doc.line_index.span_to_range(&doc.source, r.span),
            new_text: new_name.clone(),
        })
        .collect();
    if !edits.is_empty() {
        changes.insert(uri.clone(), edits);
    }

    let name = crate::lookup::find_node_at(program, &doc.source, offset)
        .name
        .unwrap_or_default();
    if !name.is_empty() {
        let index = backend.state.module_index.read().unwrap();
        for (target_uri, entry) in &index.entries {
            if target_uri == uri {
                continue;
            }
            if let Some(target_program) = &entry.program {
                let target_source = &entry.source;
                let target_line_index = crate::convert::LineIndex::new(target_source);
                let target_refs = crate::lookup::find_references_to_name_in_program(
                    target_program,
                    target_source,
                    &name,
                );
                let mut target_edits = Vec::new();
                for r in &target_refs {
                    target_edits.push(TextEdit {
                        range: target_line_index.span_to_range(target_source, r.span),
                        new_text: new_name.clone(),
                    });
                }
                if !target_edits.is_empty() {
                    changes.insert(target_uri.clone(), target_edits);
                }
            }
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
