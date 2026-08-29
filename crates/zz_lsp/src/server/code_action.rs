use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use super::Backend;

pub(crate) async fn handle_code_action(
    backend: &Backend,
    params: CodeActionParams,
) -> Result<Option<CodeActionResponse>> {
    let uri = &params.text_document.uri;
    let source = match backend.state.documents.get(uri) {
        Some(doc) => doc.source.clone(),
        None => return Ok(None),
    };

    let actions = crate::code_action::code_actions_for_range(&params, &source, uri);
    if actions.is_empty() {
        Ok(None)
    } else {
        Ok(Some(actions))
    }
}
