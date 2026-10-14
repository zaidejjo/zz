use tower_lsp::jsonrpc::{Error, Result};
use tower_lsp::lsp_types::*;

use super::Backend;

pub(crate) async fn handle_code_lens(
    backend: &Backend,
    params: CodeLensParams,
) -> Result<Option<Vec<CodeLens>>> {
    let uri = &params.text_document.uri;
    let doc = match backend.state.documents.get(uri) {
        Some(doc) => doc.value().clone(),
        None => return Ok(None),
    };
    let program = match &doc.program {
        Some(p) => p,
        None => return Ok(None),
    };
    Ok(Some(crate::test_lens::test_lenses_for(
        program,
        &doc.source,
        uri,
    )))
}

pub(crate) async fn handle_execute_command(
    backend: &Backend,
    params: ExecuteCommandParams,
) -> Result<Option<serde_json::Value>> {
    if params.command != crate::test_lens::RUN_TEST_COMMAND {
        return Err(Error::method_not_found());
    }
    let Some((file, test_name)) = crate::test_lens::parse_run_args(&params.arguments) else {
        return Err(Error::invalid_params("expected [fileUri, testName]"));
    };
    Ok(crate::test_lens::run_single_test(&backend.client, &file, &test_name).await)
}
