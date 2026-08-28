use tower_lsp::lsp_types::*;

use crate::diagnostics::recheck_and_publish;

use super::Backend;

pub(crate) async fn handle_did_open(backend: &Backend, params: DidOpenTextDocumentParams) {
    let uri = params.text_document.uri;
    let version = params.text_document.version;
    let text = params.text_document.text;

    let (_, old_defs) = backend.state.update_document(uri.clone(), version, text);
    if let Some(defs) = &old_defs {
        backend.state.prune_defs(defs);
    }

    recheck_and_publish(backend.state.clone(), &backend.client, uri).await;
}

pub(crate) async fn handle_did_change(backend: &Backend, params: DidChangeTextDocumentParams) {
    let uri = params.text_document.uri;
    let version = params.text_document.version;
    log::debug!("did_change {} v{version}", uri.path());

    let text = params
        .content_changes
        .into_iter()
        .next()
        .map(|c| c.text)
        .unwrap_or_default();

    let (_, old_defs) = backend.state.update_document(uri.clone(), version, text);
    if let Some(defs) = &old_defs {
        backend.state.prune_defs(defs);
    }

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    recheck_and_publish(backend.state.clone(), &backend.client, uri).await;
}

pub(crate) async fn handle_did_save(backend: &Backend, params: DidSaveTextDocumentParams) {
    recheck_and_publish(
        backend.state.clone(),
        &backend.client,
        params.text_document.uri,
    )
    .await;
}

pub(crate) async fn handle_did_close(backend: &Backend, params: DidCloseTextDocumentParams) {
    if let Some(defs) = backend
        .state
        .remove_document(&params.text_document.uri)
    {
        backend.state.prune_defs(&defs);
    }
    backend
        .client
        .publish_diagnostics(params.text_document.uri, vec![], None)
        .await;
}
