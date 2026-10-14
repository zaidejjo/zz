use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use super::Backend;

pub(crate) async fn handle_code_action(
    backend: &Backend,
    params: CodeActionParams,
) -> Result<Option<CodeActionResponse>> {
    let uri = &params.text_document.uri;
    let doc = match backend.state.documents.get(uri) {
        Some(doc) => doc.value().clone(),
        None => return Ok(None),
    };

    let mut actions = crate::code_action::code_actions_for_range(&params, &doc.source, uri);
    if let Some(program) = &doc.program {
        actions.extend(crate::refactor::refactor_actions(
            program,
            &doc.source,
            uri,
            params.range,
        ));
    }
    // Workspace + installed-package providers for undefined names (the
    // checker only suggests `std.*` imports itself).
    if let Some(program) = &doc.program {
        actions.extend(crate::code_action::auto_import_actions(
            &backend.state,
            uri,
            &doc.source,
            program,
            &params.context.diagnostics,
        ));
    }
    if actions.is_empty() {
        Ok(None)
    } else {
        Ok(Some(actions))
    }
}
