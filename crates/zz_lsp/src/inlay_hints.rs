//! Inlay hints: display inferred types for `let` bindings and parameter
//! names for function call arguments.

use tower_lsp::lsp_types::*;
use zz_checker::CheckResult;
use zz_frontend::ast::*;

/// Collect inlay hints for the given program.
///
/// Two families: parameter-name hints at call sites, and inferred-type
/// hints on unannotated bindings (`y := 1 + 2` → `: int`). When `range`
/// is set, only hints inside it are returned (viewport requests).
pub fn inlay_hints(
    program: &Program,
    source: &str,
    check_result: Option<&CheckResult>,
    range: Option<Range>,
) -> Vec<InlayHint> {
    let mut hints = Vec::new();
    let cr = check_result;

    for stmt in &program.stmts {
        collect_stmt_hints(program, stmt, source, cr, &mut hints);
    }
    if let Some(range) = range {
        // Offset comparison avoids depending on Position ordering.
        let line_index = crate::convert::LineIndex::new(source);
        let start = line_index.position_to_offset(source, range.start);
        let end = line_index.position_to_offset(source, range.end);
        hints.retain(|hint| {
            let offset = line_index.position_to_offset(source, hint.position);
            offset >= start && offset <= end
        });
    }
    hints
}

/// Push an inferred-type hint (`: int`) for an unannotated binding.
fn push_type_hint(
    name: &zz_frontend::ast::Ident,
    value: &Expr,
    program: &Program,
    source: &str,
    cr: &CheckResult,
    out: &mut Vec<InlayHint>,
) {
    // Checker-authoritative first, expression resolution as fallback
    // (stale or partial results may lack the binding).
    let ty = cr
        .bindings
        .get(&name.name)
        .cloned()
        .or_else(|| crate::lookup::resolve_type_of_expr(program, cr, value));
    // Unit and error types are noise, not information.
    let ty = match ty {
        Some(t)
            if !matches!(
                t,
                zz_checker::type_::Type::Unit | zz_checker::type_::Type::Error
            ) =>
        {
            t
        }
        _ => return,
    };
    out.push(InlayHint {
        position: offset_to_position_from_span(source, name.span.end),
        label: InlayHintLabel::String(format!(": {ty}")),
        kind: Some(InlayHintKind::TYPE),
        text_edits: None,
        tooltip: None,
        padding_left: Some(true),
        padding_right: None,
        data: None,
    });
}

fn collect_stmt_hints(
    program: &Program,
    stmt: &Stmt,
    source: &str,
    cr: Option<&CheckResult>,
    out: &mut Vec<InlayHint>,
) {
    match stmt {
        Stmt::Func { params, body, .. } => {
            for param in params {
                if param.ty.is_none() {
                    // No type annotation — try to infer from check result.
                    // (Params without annotations are rare in ZZ but possible.)
                    let _ = (source, cr);
                }
            }
            collect_block_hints(program, body, source, cr, out);
        }
        Stmt::For {
            vars, iter, body, ..
        } => {
            // Hint the iterator element type if no type annotation.
            if cr.is_some() {
                // We could resolve the iter type from CheckResult, but
                // the parser already requires type annotations on `for` vars in ZZ.
                // Still, emit a hint for the iteration.
                let _ = (vars, iter);
            }
            collect_block_hints(program, body, source, cr, out);
        }
        Stmt::Decl {
            name, ty, value, ..
        } => {
            if ty.is_none() {
                if let Some(cr) = cr {
                    push_type_hint(name, value, program, source, cr, out);
                }
            }
            collect_expr_hints(program, value, source, cr, out);
        }
        Stmt::Return { value: Some(v), .. } => {
            collect_expr_hints(program, v, source, cr, out);
        }
        Stmt::Return { .. } => {}
        Stmt::Assign { target, value, .. } => {
            collect_expr_hints(program, target, source, cr, out);
            collect_expr_hints(program, value, source, cr, out);
        }
        Stmt::CompoundAssign { target, value, .. } => {
            collect_expr_hints(program, target, source, cr, out);
            collect_expr_hints(program, value, source, cr, out);
        }
        Stmt::Defer { expr, .. } => {
            collect_expr_hints(program, expr, source, cr, out);
        }
        Stmt::Expr(e) => collect_expr_hints(program, e, source, cr, out),
        _ => {}
    }
}

fn collect_block_hints(
    program: &Program,
    block: &Block,
    source: &str,
    cr: Option<&CheckResult>,
    out: &mut Vec<InlayHint>,
) {
    for stmt in &block.stmts {
        collect_stmt_hints(program, stmt, source, cr, out);
    }
}

fn collect_expr_hints(
    program: &Program,
    expr: &Expr,
    source: &str,
    cr: Option<&CheckResult>,
    out: &mut Vec<InlayHint>,
) {
    match expr {
        Expr::Call { callee, args, .. } => {
            // Emit parameter name hints for function call arguments.
            if let Some(cr) = cr {
                let func_name = match callee.as_ref() {
                    Expr::Ident { name, .. } => Some(name.as_str()),
                    Expr::Path { parts, .. } => parts.last().map(|s| s.as_str()),
                    _ => None,
                };
                if let Some(name) = func_name {
                    if let Some(sig) = zz_checker::lookup_std(&cr.funcs, name) {
                        for (i, arg) in args.iter().enumerate() {
                            if i < sig.params.len() {
                                let (pname, _) = &sig.params[i];
                                // Position: just before the argument expression.
                                let arg_span = arg.span();
                                let pos = offset_to_position_from_span(source, arg_span.start);
                                out.push(InlayHint {
                                    position: pos,
                                    label: InlayHintLabel::String(format!("{pname}: ")),
                                    kind: Some(InlayHintKind::PARAMETER),
                                    text_edits: None,
                                    tooltip: None,
                                    padding_left: None,
                                    padding_right: Some(true),
                                    data: None,
                                });
                            }
                        }
                    }
                }
                // Recurse into arguments.
                for arg in args {
                    collect_expr_hints(program, arg, source, Some(cr), out);
                }
            }
            // Also recurse into callee.
            collect_expr_hints(program, callee, source, cr, out);
        }
        Expr::If {
            cond, then, els, ..
        } => {
            collect_expr_hints(program, cond, source, cr, out);
            collect_block_hints(program, then, source, cr, out);
            if let Some(e) = els {
                collect_expr_hints(program, e, source, cr, out);
            }
        }
        Expr::While { cond, body, .. } => {
            collect_expr_hints(program, cond, source, cr, out);
            collect_block_hints(program, body, source, cr, out);
        }
        Expr::Binary { left, right, .. } => {
            collect_expr_hints(program, left, source, cr, out);
            collect_expr_hints(program, right, source, cr, out);
        }
        Expr::Unary { expr, .. } => collect_expr_hints(program, expr, source, cr, out),
        Expr::Array { elems, .. } => {
            for e in elems {
                collect_expr_hints(program, e, source, cr, out);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                collect_expr_hints(program, k, source, cr, out);
                collect_expr_hints(program, v, source, cr, out);
            }
        }
        Expr::Index { obj, index, .. } => {
            collect_expr_hints(program, obj, source, cr, out);
            collect_expr_hints(program, index, source, cr, out);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            collect_expr_hints(program, obj, source, cr, out);
            if let Some(s) = start {
                collect_expr_hints(program, s, source, cr, out);
            }
            if let Some(e) = end {
                collect_expr_hints(program, e, source, cr, out);
            }
        }
        Expr::Range { start, end, .. } => {
            collect_expr_hints(program, start, source, cr, out);
            collect_expr_hints(program, end, source, cr, out);
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            collect_expr_hints(program, body, source, cr, out);
            collect_expr_hints(program, iter, source, cr, out);
            if let Some(f) = filter {
                collect_expr_hints(program, f, source, cr, out);
            }
        }
        Expr::Try { expr, .. } => collect_expr_hints(program, expr, source, cr, out),
        Expr::Field { obj, .. } => collect_expr_hints(program, obj, source, cr, out),
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_expr_hints(program, scrutinee, source, cr, out);
            for arm in arms {
                collect_expr_hints(program, &arm.body, source, cr, out);
            }
        }
        Expr::Block(b) => collect_block_hints(program, b, source, cr, out),
        Expr::Fmt { parts, .. } => {
            for part in parts {
                if let FmtPart::Expr(e, _) = part {
                    collect_expr_hints(program, e, source, cr, out);
                }
            }
        }
        Expr::Paren { expr, .. } => collect_expr_hints(program, expr, source, cr, out),
        Expr::Closure { body, .. } => collect_expr_hints(program, body, source, cr, out),
        Expr::IfLet {
            value, then, els, ..
        } => {
            collect_expr_hints(program, value, source, cr, out);
            collect_block_hints(program, then, source, cr, out);
            if let Some(e) = els {
                collect_expr_hints(program, e, source, cr, out);
            }
        }
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                collect_expr_hints(program, v, source, cr, out);
            }
        }
        _ => {}
    }
}

/// Convert a byte offset to an LSP Position using a simple line scan.
fn offset_to_position_from_span(source: &str, offset: u32) -> Position {
    crate::convert::offset_to_position(source, offset)
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use zz_checker::check_program;
    use zz_frontend::parse;

    fn hints_for(source: &str) -> Vec<InlayHint> {
        let parsed = parse(source);
        let cr = check_program(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        inlay_hints(&parsed.program, source, Some(&cr), None)
    }

    #[test]
    fn call_has_parameter_hints() {
        let src = "func add(a: int, b: int) -> int { return a + b }\nx := add(1, 2)\n";
        let hints = hints_for(src);
        // Should have parameter name hints for the two arguments.
        let param_hints: Vec<_> = hints
            .iter()
            .filter(|h| h.kind == Some(InlayHintKind::PARAMETER))
            .collect();
        assert!(
            param_hints.len() >= 2,
            "expected at least 2 param hints, got {}",
            param_hints.len()
        );
    }

    #[test]
    fn no_hints_for_unknown_func() {
        let src = "x := unknown_func(1)\n";
        let hints = hints_for(src);
        // No param hints for unknown function.
        let param_hints: Vec<_> = hints
            .iter()
            .filter(|h| h.kind == Some(InlayHintKind::PARAMETER))
            .collect();
        assert!(
            param_hints.is_empty(),
            "should have no param hints for unknown func"
        );
    }

    #[test]
    fn hints_in_nested_expressions() {
        let src = "func add(a: int, b: int) -> int { return a + b }\nx := add(add(1, 2), 3)\n";
        let hints = hints_for(src);
        let param_hints: Vec<_> = hints
            .iter()
            .filter(|h| h.kind == Some(InlayHintKind::PARAMETER))
            .collect();
        // 2 hints for outer add + 2 hints for inner add = 4.
        assert_eq!(
            param_hints.len(),
            4,
            "expected 4 param hints, got {}",
            param_hints.len()
        );
    }

    #[test]
    fn hint_label_contains_param_name() {
        let src = "func greet(name: str) -> str { return name }\ngreet(\"hi\")\n";
        let hints = hints_for(src);
        let param_hints: Vec<_> = hints
            .iter()
            .filter(|h| h.kind == Some(InlayHintKind::PARAMETER))
            .collect();
        assert_eq!(param_hints.len(), 1);
        match &param_hints[0].label {
            InlayHintLabel::String(s) => assert!(s.contains("name")),
            _ => panic!("expected string label"),
        }
    }

    fn type_hints_of(source: &str) -> Vec<(String, u32)> {
        hints_for(source)
            .into_iter()
            .filter(|h| h.kind == Some(InlayHintKind::TYPE))
            .map(|h| match h.label {
                InlayHintLabel::String(s) => (s, h.position.line),
                _ => panic!("expected string label"),
            })
            .collect()
    }

    #[test]
    fn unannotated_let_gets_type_hint() {
        let found = type_hints_of("y := 1 + 2\n");
        assert_eq!(found.len(), 1, "one type hint, got: {found:?}");
        assert_eq!(found[0].0, ": int");
        assert_eq!(found[0].1, 0);
    }

    #[test]
    fn annotated_let_gets_no_type_hint() {
        let found = type_hints_of("y: int = 1 + 2\n");
        assert!(found.is_empty(), "annotated lets stay quiet: {found:?}");
    }

    #[test]
    fn string_and_float_hints() {
        let found = type_hints_of("s := \"hi\"\nf := 1.5\n");
        assert_eq!(found.len(), 2, "got: {found:?}");
        assert!(found.iter().any(|(l, _)| l == ": str"));
        assert!(found.iter().any(|l| l.0 == ": float"));
    }

    #[test]
    fn error_typed_let_gets_no_hint() {
        // `nope` is undefined: no authoritative type, no noise.
        let found = type_hints_of("y := nope\n");
        assert!(found.is_empty(), "error types stay quiet: {found:?}");
    }

    #[test]
    fn range_filters_hints() {
        let parsed = parse("a := 1\nb := 2\n");
        let cr = check_program(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        let range = Range {
            start: Position {
                line: 1,
                character: 0,
            },
            end: Position {
                line: 1,
                character: 100,
            },
        };
        let hints = inlay_hints(&parsed.program, "a := 1\nb := 2\n", Some(&cr), Some(range));
        let lines: Vec<u32> = hints.iter().map(|h| h.position.line).collect();
        assert_eq!(lines, vec![1], "only line 1 served, got: {lines:?}");
    }

    #[test]
    fn empty_program_has_no_hints() {
        let hints = hints_for("");
        assert!(hints.is_empty());
    }
}
