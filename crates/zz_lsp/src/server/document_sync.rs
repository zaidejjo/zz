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

    recheck_and_publish(backend.state.clone(), &backend.client, uri.clone()).await;
    // Surface a broken project manifest on open too (watchers may be
    // unsupported by the client): silent zz.toml + failing imports is
    // otherwise a mystery.
    let root = uri
        .to_file_path()
        .ok()
        .and_then(|p| crate::deps::find_project_root(&p))
        .or_else(|| backend.state.root.read().unwrap().clone());
    if let Some(root) = root {
        crate::watch::publish_manifest_diagnostic(&backend.client, &root).await;
    }
}

pub(crate) async fn handle_did_change(backend: &Backend, params: DidChangeTextDocumentParams) {
    let uri = params.text_document.uri;
    let version = params.text_document.version;
    log::debug!("did_change {} v{version}", uri.path());

    // Apply every content change in order. FULL sync sends a single
    // full-text change; ranged changes (from non-conformant clients or
    // future incremental sync) apply against the current buffer.
    let mut text = match backend.state.documents.get(&uri) {
        Some(doc) => doc.source.clone(),
        None => String::new(),
    };
    for change in params.content_changes {
        match change.range {
            Some(range) => {
                let start =
                    crate::convert::LineIndex::new(&text).position_to_offset(&text, range.start);
                let end =
                    crate::convert::LineIndex::new(&text).position_to_offset(&text, range.end);
                let (start, end) = (start.min(end), start.max(end));
                if (end as usize) <= text.len() {
                    text.replace_range(start as usize..end as usize, &change.text);
                } else {
                    text = change.text.clone();
                }
            }
            None => text = change.text,
        }
    }

    let (_, old_defs) = backend.state.update_document(uri.clone(), version, text);
    if let Some(defs) = &old_defs {
        backend.state.prune_defs(defs);
    }

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // Coalesce bursts: when the user is still typing, a newer change
    // (higher version) is pending and will run its own recheck — skip
    // ours instead of stacking full checks per keystroke.
    let current_version = backend.state.documents.get(&uri).map(|doc| doc.version);
    if current_version != Some(version) {
        return;
    }

    recheck_and_publish(backend.state.clone(), &backend.client, uri).await;
}

pub(crate) async fn handle_did_save(backend: &Backend, params: DidSaveTextDocumentParams) {
    let uri = params.text_document.uri;
    recheck_and_publish(backend.state.clone(), &backend.client, uri.clone()).await;
    // Dependents observe this file: refresh open ones so a fixed
    // dependency clears their diagnostics on save (one level, bounded
    // by the open set — the same rule as watched-file changes).
    for dependent in backend.state.get_dependents(&uri) {
        if backend.state.documents.contains_key(&dependent) {
            recheck_and_publish(backend.state.clone(), &backend.client, dependent.clone()).await;
        }
    }
}

pub(crate) async fn handle_did_close(backend: &Backend, params: DidCloseTextDocumentParams) {
    if let Some(defs) = backend.state.remove_document(&params.text_document.uri) {
        backend.state.prune_defs(&defs);
    }
    backend
        .client
        .publish_diagnostics(params.text_document.uri, vec![], None)
        .await;
}
