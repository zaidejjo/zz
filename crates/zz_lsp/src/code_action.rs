//! Code action provider: maps tiered FixIt suggestions to LSP quick-fixes.

use std::collections::HashMap;

use tower_lsp::lsp_types::{
    CodeAction, CodeActionKind, CodeActionParams, CodeActionResponse, Diagnostic, TextEdit, Url,
    WorkspaceEdit,
};
use zz_frontend::diag::{FixIt, FixSafety, RawDiag};

use crate::convert::span_to_range;

/// Extract code actions from diagnostics in the requested range.
///
/// Each diagnostic carries a stashed `RawDiag` (serialized as JSON) in its
/// `.data` field. We deserialize it, iterate over its `FixIt` list, and
/// produce one `CodeAction` per fixit.
///
/// - `FixSafety::Safe` fixes produce a single "Apply fix" action.
/// - `FixSafety::Ambiguous` fixes with N alternatives produce N actions,
///   one per candidate, so the user can pick in the editor UI.
pub fn code_actions_for_range(
    params: &CodeActionParams,
    source: &str,
    file_uri: &Url,
) -> CodeActionResponse {
    let mut actions = CodeActionResponse::new();

    for diag in &params.context.diagnostics {
        let Some(raw) = extract_raw_diag(diag) else {
            continue;
        };

        for fixit in &raw.fixits {
            match fixit.safety {
                FixSafety::Safe => {
                    if let Some(action) = make_quickfix(fixit, diag, source, file_uri) {
                        actions.push(action.into());
                    }
                }
                FixSafety::Ambiguous if fixit.alternatives.len() > 1 => {
                    // One action per candidate so the editor shows a pick list.
                    for alt in &fixit.alternatives {
                        if let Some(action) =
                            make_ambiguous_quickfix(fixit, alt, diag, source, file_uri)
                        {
                            actions.push(action.into());
                        }
                    }
                }
                FixSafety::Ambiguous => {
                    // Single alternative despite being marked ambiguous — treat as safe.
                    if let Some(action) = make_quickfix(fixit, diag, source, file_uri) {
                        actions.push(action.into());
                    }
                }
            }
        }
    }

    actions
}

/// Deserialize the stashed `RawDiag` from a diagnostic's `.data` field.
fn extract_raw_diag(diag: &Diagnostic) -> Option<RawDiag> {
    let data = diag.data.as_ref()?;
    serde_json::from_value(data.clone()).ok()
}

/// Build a single `CodeAction` for an unambiguous fix.
fn make_quickfix(
    fixit: &FixIt,
    diag: &Diagnostic,
    source: &str,
    file_uri: &Url,
) -> Option<CodeAction> {
    let text_edit = TextEdit {
        range: span_to_range(source, fixit.span),
        new_text: fixit.replacement.clone(),
    };

    let mut changes = HashMap::new();
    changes.insert(file_uri.clone(), vec![text_edit]);

    Some(CodeAction {
        title: format!("{}: `{}`", fixit.message, fixit.replacement),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diag.clone()]),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }),
        ..Default::default()
    })
}

/// Build a `CodeAction` for one candidate of an ambiguous fix.
fn make_ambiguous_quickfix(
    fixit: &FixIt,
    candidate: &str,
    diag: &Diagnostic,
    source: &str,
    file_uri: &Url,
) -> Option<CodeAction> {
    let text_edit = TextEdit {
        range: span_to_range(source, fixit.span),
        new_text: candidate.to_string(),
    };

    let mut changes = HashMap::new();
    changes.insert(file_uri.clone(), vec![text_edit]);

    Some(CodeAction {
        title: format!("{}: `{}`", fixit.message, candidate),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diag.clone()]),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp::lsp_types::{
        CodeActionContext, CodeActionOrCommand, DiagnosticSeverity, Position, Range,
    };
    use zz_frontend::diag::{FixIt, RawDiag, Severity};
    use zz_frontend::span::Span;

    fn make_test_diag(raw: &RawDiag) -> Diagnostic {
        let data = serde_json::to_value(raw).unwrap();
        Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 5,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            code: None,
            code_description: None,
            source: Some("zz".to_string()),
            message: raw.message.clone(),
            related_information: None,
            tags: None,
            data: Some(data),
        }
    }

    fn make_params(diags: Vec<Diagnostic>, uri: &Url) -> CodeActionParams {
        CodeActionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 5,
                    character: 0,
                },
            },
            context: CodeActionContext {
                diagnostics: diags,
                only: None,
                trigger_kind: None,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    fn extract_action(item: &CodeActionOrCommand) -> &CodeAction {
        match item {
            CodeActionOrCommand::CodeAction(a) => a,
            _ => panic!("expected CodeAction"),
        }
    }

    #[test]
    fn safe_fix_produces_single_action() {
        let raw = RawDiag {
            severity: Severity::Error,
            message: "undefined variable `x`".into(),
            span: Some(Span::new(10, 11)),
            notes: vec![],
            fixits: vec![FixIt::safe(Span::new(10, 11), "_x", "rename to")],
        };
        let diag = make_test_diag(&raw);
        let uri: Url = "file:///test.zz".parse().unwrap();
        let params = make_params(vec![diag], &uri);

        let actions = code_actions_for_range(&params, "hello world\n", &uri);
        assert_eq!(actions.len(), 1);
        let action = extract_action(&actions[0]);
        assert!(action.title.contains("_x"));
        assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
    }

    #[test]
    fn ambiguous_fix_produces_one_action_per_candidate() {
        let raw = RawDiag {
            severity: Severity::Error,
            message: "undefined variable `prntlnn`".into(),
            span: Some(Span::new(0, 8)),
            notes: vec![],
            fixits: vec![FixIt::ambiguous(
                Span::new(0, 8),
                "println",
                "replace variable",
                vec!["println".into(), "println!".into()],
            )],
        };
        let diag = make_test_diag(&raw);
        let uri: Url = "file:///test.zz".parse().unwrap();
        let params = make_params(vec![diag], &uri);

        let actions = code_actions_for_range(&params, "prntlnn\n", &uri);
        assert_eq!(actions.len(), 2);
        let titles: Vec<String> = actions
            .iter()
            .map(|a| extract_action(a).title.clone())
            .collect();
        assert!(titles.iter().any(|t| t.contains("println")));
        assert!(titles.iter().any(|t| t.contains("println!")));
    }

    #[test]
    fn no_actions_when_data_missing() {
        let diag = Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 5,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            code: None,
            code_description: None,
            source: Some("zz".to_string()),
            message: "some error".into(),
            related_information: None,
            tags: None,
            data: None,
        };
        let uri: Url = "file:///test.zz".parse().unwrap();
        let params = make_params(vec![diag], &uri);
        let actions = code_actions_for_range(&params, "", &uri);
        assert!(actions.is_empty());
    }
}
