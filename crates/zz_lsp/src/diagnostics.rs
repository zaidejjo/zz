//! Recheck pipeline: parse → check → convert → publish diagnostics.

use std::sync::Arc;

use tower_lsp::lsp_types::{Diagnostic, Url};
use tower_lsp::Client;
use zz_checker::check_program;

use crate::convert::{severity_to_lsp, span_to_range};
use crate::state::GlobalState;

/// Re-check a single file and publish diagnostics to the editor.
///
/// Uses `spawn_blocking` for CPU-bound parsing/checking so the async event
/// loop is never blocked. An atomic sequence counter acts as a debounce:
/// if the sequence changes between spawning and completion, the results
/// are silently discarded (a newer change is pending).
pub async fn recheck_and_publish(
    state: Arc<GlobalState>,
    client: &Client,
    uri: Url,
) {
    let seq = state.bump_sequence();

    // Snapshot what we need for the blocking closure.
    let (source, parse_errors, program) = match state.documents.get(&uri) {
        Some(doc) => (
            doc.source.clone(),
            doc.parse_errors.clone(),
            doc.program.clone(),
        ),
        None => return,
    };
    let (init_bindings, init_funcs, init_structs) = state.checker_seed();

    let client_clone = client.clone();
    let state_clone = state.clone();
    let uri_clone = uri.clone();

    // Run parse + check on the blocking threadpool.
    let result = tokio::task::spawn_blocking(move || {
        // Re-check that the sequence hasn't already moved on.
        let current = state_clone.current_sequence();
        if current != seq {
            return None; // stale — a newer change is pending
        }

        let program = program?;
        let checked = check_program(&program, init_bindings, init_funcs, init_structs);
        Some(checked)
    })
    .await;

    let Ok(result) = result else {
        return; // task panicked or was cancelled
    };

    // Another debounce check after await.
    if state.current_sequence() != seq {
        return;
    }

    let Some(checked) = result else {
        // Stale. Publish only parse errors (if any).
        if !parse_errors.is_empty() {
            let diags: Vec<Diagnostic> = parse_errors
                .iter()
                .filter(|e| e.span.is_some())
                .map(|raw| convert_diagnostic(&source, raw))
                .collect();
            client_clone
                .publish_diagnostics(uri_clone, diags, None)
                .await;
        }
        return;
    };

    // Persist the CheckResult in the document for hover / go-to-definition.
    if let Some(mut doc) = state.documents.get_mut(&uri) {
        doc.check_result = Some(checked.clone());
    }

    // Prune old per-file defs before absorbing new ones.
    // (The document was already replaced by update_document, so old defs
    // were returned and should have been pruned by the caller. We just
    // absorb the new results here.)
    state.absorb_result(&uri, &checked);

    // Combine parse errors + checker diagnostics.
    let mut all_errors = parse_errors;
    all_errors.extend(checked.errors);

    let diagnostics: Vec<Diagnostic> = all_errors
        .iter()
        .filter(|e| e.span.is_some())
        .map(|raw| convert_diagnostic(&source, raw))
        .collect();

    client_clone
        .publish_diagnostics(uri_clone, diagnostics, None)
        .await;
}

/// Convert a single ZZ `RawDiag` to an LSP `Diagnostic`.
///
/// Stashes the full `RawDiag` as JSON in `Diagnostic.data` so that
/// `textDocument/codeAction` can retrieve fixit info without re-checking.
fn convert_diagnostic(source: &str, raw: &zz_frontend::diag::RawDiag) -> Diagnostic {
    let span = raw.span.unwrap();
    let range = span_to_range(source, span);
    let severity = severity_to_lsp(raw.severity);

    // Serialize the entire RawDiag for code actions to extract later.
    let data = serde_json::to_value(raw).ok();

    Diagnostic {
        range,
        severity: Some(severity),
        code: None,
        code_description: None,
        source: Some("zz".to_string()),
        message: raw.message.clone(),
        related_information: None,
        tags: None,
        data,
    }
}
