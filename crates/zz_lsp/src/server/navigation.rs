use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use crate::convert::LineIndex;
use crate::state::DocumentState;
use zz_frontend::ast::{Expr, Stmt};

use super::Backend;
use super::find_struct_field_location;

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
    let node = crate::lookup::find_node_at(program, &doc.source, offset);

    if let Some(Stmt::Import { path, .. }) = node.stmt {
        let root = backend.state.root.read().unwrap().clone();
        if let Some(root) = root {
            let index = backend.state.module_index.read().unwrap();
            if let Some(target_uri) = index.resolve_import(path, &root) {
                let target_loc = if let Some(entry) = index.entries.get(&target_uri) {
                    if let Some(def) = entry.definitions.first() {
                        let target_doc = DocumentState {
                            uri: target_uri.clone(),
                            version: 0,
                            source: entry.source.clone(),
                            parse_errors: vec![],
                            program: entry.program.clone(),
                            check_result: None,
                            file_defs: None,
                            line_index: LineIndex::new(&entry.source),
                        };
                        Location {
                            uri: target_uri,
                            range: target_doc.line_index.span_to_range(&entry.source, def.span),
                        }
                    } else {
                        Location {
                            uri: target_uri,
                            range: Range::default(),
                        }
                    }
                } else {
                    return Ok(None);
                };
                return Ok(Some(GotoDefinitionResponse::Scalar(target_loc)));
            }
        }
    }

    let name = match &node.name {
        Some(n) => n.clone(),
        None => return Ok(None),
    };

    let defs = crate::lookup::collect_definitions(program, &doc.source);
    let def = defs.values().find(|d| d.name == name);

    if def.is_none() {
        if let Some(check_result) = &doc.check_result {
            if let Some(Expr::Field { name: field, obj, .. }) = node.expr {
                if let Some(zz_checker::Type::Struct(ref struct_name)) =
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
                    if let Some(zz_checker::Type::Struct(ref struct_name)) =
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

pub(crate) async fn handle_references(
    backend: &Backend,
    params: ReferenceParams,
) -> Result<Option<Vec<Location>>> {
    let uri = &params.text_document_position.text_document.uri;
    let pos = params.text_document_position.position;

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

    let mut locations: Vec<Location> = refs
        .iter()
        .map(|r| Location {
            uri: uri.clone(),
            range: doc.line_index.span_to_range(&doc.source, r.span),
        })
        .collect();

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
                let target_line_index = LineIndex::new(target_source);
                let target_refs = crate::lookup::find_references_to_name_in_program(
                    target_program,
                    target_source,
                    &name,
                );
                for r in &target_refs {
                    locations.push(Location {
                        uri: target_uri.clone(),
                        range: target_line_index.span_to_range(target_source, r.span),
                    });
                }
            }
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
