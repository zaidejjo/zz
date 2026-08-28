use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use super::Backend;

pub(crate) async fn handle_document_symbol(
    backend: &Backend,
    params: DocumentSymbolParams,
) -> Result<Option<DocumentSymbolResponse>> {
    let uri = &params.text_document.uri;
    let doc = match backend.state.documents.get(uri) {
        Some(doc) => doc.clone(),
        None => return Ok(None),
    };
    let program = match &doc.program {
        Some(p) => p,
        None => return Ok(None),
    };

    let symbols = crate::symbols::document_symbols(program, &doc.source);
    Ok(Some(DocumentSymbolResponse::Nested(symbols)))
}

pub(crate) async fn handle_symbol(
    backend: &Backend,
    params: WorkspaceSymbolParams,
) -> Result<Option<Vec<SymbolInformation>>> {
    let query = &params.query;
    let mut all_symbols = Vec::new();

    for entry in backend.state.documents.iter() {
        let doc = entry.value();
        let program = match &doc.program {
            Some(p) => p,
            None => continue,
        };
        let syms = crate::symbols::workspace_symbols(program, &doc.source, &doc.uri, None);
        all_symbols.extend(syms);
    }

    {
        let index = backend.state.module_index.read().unwrap();
        for (uri, entry) in &index.entries {
            if backend.state.documents.contains_key(uri) {
                continue;
            }
            if let Some(program) = &entry.program {
                let syms =
                    crate::symbols::workspace_symbols(program, &entry.source, uri, None);
                all_symbols.extend(syms);
            }
        }
    }

    let filtered = crate::symbols::filter_symbols(&all_symbols, query);
    Ok(Some(filtered))
}
