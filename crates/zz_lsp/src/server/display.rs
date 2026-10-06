use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use super::Backend;

pub(crate) async fn handle_formatting(
    backend: &Backend,
    params: DocumentFormattingParams,
) -> Result<Option<Vec<TextEdit>>> {
    let uri = &params.text_document.uri;
    let doc = match backend.state.documents.get(uri) {
        Some(doc) => doc.clone(),
        None => return Ok(None),
    };

    let edit = crate::formatting::format_as_edit(&doc.source);
    Ok(edit.map(|e| vec![e]))
}

pub(crate) async fn handle_inlay_hint(
    backend: &Backend,
    params: InlayHintParams,
) -> Result<Option<Vec<InlayHint>>> {
    let uri = &params.text_document.uri;
    let range = params.range;

    let doc = match backend.state.documents.get(uri) {
        Some(doc) => doc.clone(),
        None => return Ok(None),
    };
    let program = match &doc.program {
        Some(p) => p,
        None => return Ok(None),
    };

    let hints = crate::inlay_hints::inlay_hints(
        program,
        &doc.source,
        doc.check_result.as_ref(),
        Some(range),
    );
    Ok(Some(hints))
}

pub(crate) async fn handle_semantic_tokens_full(
    backend: &Backend,
    params: SemanticTokensParams,
) -> Result<Option<SemanticTokensResult>> {
    let uri = &params.text_document.uri;
    let doc = match backend.state.documents.get(uri) {
        Some(doc) => doc.clone(),
        None => return Ok(None),
    };
    let program = match &doc.program {
        Some(p) => p,
        None => return Ok(None),
    };

    // Known function names drive call-callee classification (`pow(2, 3)`
    // colors as a call, not a variable): every resolved signature plus
    // selectively-imported bare targets (generics have no value binding
    // but are still calls).
    let mut known: std::collections::HashSet<String> = doc
        .check_result
        .as_ref()
        .map(|cr| cr.funcs.keys().cloned().collect())
        .unwrap_or_default();
    for stmt in &program.stmts {
        if let zz_frontend::ast::Stmt::Import {
            path, alias, items, ..
        } = stmt
        {
            let ns = alias
                .as_ref()
                .cloned()
                .or_else(|| path.last().cloned())
                .unwrap_or_default();
            for item in items {
                if let zz_frontend::ast::ImportItem::Named {
                    name, alias: ia, ..
                } = item
                {
                    let target = ia.clone().unwrap_or_else(|| name.clone());
                    if doc
                        .check_result
                        .as_ref()
                        .is_some_and(|cr| cr.funcs.contains_key(&format!("{ns}.{name}")))
                    {
                        known.insert(target);
                    }
                }
            }
        }
    }

    let tokens = crate::semantic_tokens::collect_semantic_tokens_with(program, &doc.source, &known);
    let encoded = crate::semantic_tokens::encode_tokens(&tokens, &doc.source);
    Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
        result_id: None,
        data: encoded,
    })))
}

pub(crate) async fn handle_folding_range(
    backend: &Backend,
    params: FoldingRangeParams,
) -> Result<Option<Vec<FoldingRange>>> {
    let uri = &params.text_document.uri;
    let doc = match backend.state.documents.get(uri) {
        Some(doc) => doc.clone(),
        None => return Ok(None),
    };
    let program = match &doc.program {
        Some(p) => p,
        None => return Ok(None),
    };

    let ranges = crate::folding::folding_ranges(program, &doc.source);
    Ok(Some(ranges))
}
