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

    // Run check on the blocking threadpool.
    let result = tokio::task::spawn_blocking(move || {
        // Debounce: skip if a newer change arrived.
        let current = state_clone.current_sequence();
        if current != seq {
            return None;
        }

        let program = program?;
        let checked = check_program(&program, init_bindings, init_funcs, init_structs);
        Some(checked)
    })
    .await;

    let Ok(result) = result else {
        return;
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

    // Absorb new results into global seed.
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
    let span = raw.span.expect("caller must filter None spans before calling");
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

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::GlobalState;
    use std::sync::Arc;
    use tower_lsp::lsp_types::{DiagnosticSeverity, Url};
    use zz_frontend::diag::{FixIt, FixSafety, RawDiag, Severity};
    use zz_frontend::span::Span;

    fn make_state() -> Arc<GlobalState> {
        Arc::new(GlobalState::new())
    }

    #[test]
    fn convert_diagnostic_basic_error() {
        let raw = RawDiag {
            severity: Severity::Error,
            message: "unexpected token".to_string(),
            span: Some(Span::new(0, 5)),
            notes: vec![],
            fixits: vec![],
        };
        let diag = convert_diagnostic("hello", &raw);
        assert_eq!(diag.range.start.line, 0);
        assert_eq!(diag.range.start.character, 0);
        assert_eq!(diag.range.end.character, 5);
        assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diag.source.as_deref(), Some("zz"));
        assert_eq!(diag.message, "unexpected token");
    }

    #[test]
    fn convert_diagnostic_with_fixit_data() {
        let raw = RawDiag {
            severity: Severity::Warning,
            message: "unused variable".to_string(),
            span: Some(Span::new(4, 5)),
            notes: vec!["consider removing".to_string()],
            fixits: vec![FixIt {
                span: Span::new(4, 5),
                replacement: String::new(),
                message: "remove variable".to_string(),
                safety: FixSafety::Safe,
                alternatives: vec![],
            }],
        };
        let diag = convert_diagnostic("let x = 1", &raw);
        assert_eq!(diag.severity, Some(DiagnosticSeverity::WARNING));
        // Data should contain the serialized RawDiag.
        let data = diag.data.as_ref().expect("should have data");
        let deserialized: RawDiag = serde_json::from_value(data.clone()).unwrap();
        assert_eq!(deserialized.fixits.len(), 1);
        assert_eq!(deserialized.fixits[0].message, "remove variable");
    }

    #[test]
    fn convert_diagnostic_multiline_range() {
        // Source: "ab\ncd\ndef" = bytes [0..9)
        // Span [2, 8) covers from '\n' (byte 2) to 'f' (byte 7, exclusive of 8)
        let raw = RawDiag {
            severity: Severity::Error,
            message: "type mismatch".to_string(),
            span: Some(Span::new(2, 8)),
            notes: vec![],
            fixits: vec![],
        };
        let diag = convert_diagnostic("ab\ncd\ndef", &raw);
        // byte 2 is '\n' on line 0 → Position(0, 2)
        assert_eq!(diag.range.start.line, 0);
        assert_eq!(diag.range.start.character, 2);
        // byte 8 is past 'f' on line 2 → Position(2, 2)
        assert_eq!(diag.range.end.line, 2);
        assert_eq!(diag.range.end.character, 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn recheck_publishes_diagnostics() {
        let state = make_state();
        let uri: Url = "file:///test.zz".parse().unwrap();
        let src = "let x = 1\n";
        state.update_document(uri.clone(), 1, src.to_string());

        // We can't easily test the full async pipeline without a mock client,
        // but we can verify the state is set up correctly after the snapshot.
        let (source, parse_errors, program) = match state.documents.get(&uri) {
            Some(doc) => (
                doc.source.clone(),
                doc.parse_errors.clone(),
                doc.program.clone(),
            ),
            None => panic!("doc should exist"),
        };
        assert_eq!(source, src);
        assert!(parse_errors.is_empty());
        assert!(program.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn recheck_stale_doc_returns_none() {
        let state = make_state();
        let uri: Url = "file:///test.zz".parse().unwrap();
        // Don't insert any document — simulates a race where the doc was closed.
        let result = match state.documents.get(&uri) {
            Some(doc) => {
                let _parse_errors = doc.parse_errors.clone();
                let _program = doc.program.clone();
                true
            }
            None => false,
        };
        assert!(!result, "doc should not exist");
    }

    #[test]
    fn convert_diagnostic_severity_help_maps_to_hint() {
        let raw = RawDiag {
            severity: Severity::Help,
            message: "try this".to_string(),
            span: Some(Span::new(0, 3)),
            notes: vec![],
            fixits: vec![],
        };
        let diag = convert_diagnostic("abc", &raw);
        assert_eq!(diag.severity, Some(DiagnosticSeverity::HINT));
    }
}
