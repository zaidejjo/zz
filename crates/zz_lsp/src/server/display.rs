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
    let program = match &doc.program {
        Some(p) => p,
        None => return Ok(None),
    };

    let config = crate::formatting::FormatConfig::default();
    let edit = crate::formatting::format_as_edit(program, &doc.source, &config);
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

    let tokens = crate::semantic_tokens::collect_semantic_tokens(program, &doc.source);
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
