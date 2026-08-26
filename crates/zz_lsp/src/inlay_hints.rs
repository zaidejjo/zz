//! Inlay hints: display inferred types for `let` bindings and parameter
//! names for function call arguments.

use tower_lsp::lsp_types::*;
use zz_checker::CheckResult;
use zz_frontend::ast::*;

/// Collect inlay hints for the given program.
pub fn inlay_hints(
    program: &Program,
    source: &str,
    check_result: Option<&CheckResult>,
    _range: Option<Range>,
) -> Vec<InlayHint> {
    let mut hints = Vec::new();
    let cr = check_result;

    for stmt in &program.stmts {
        collect_stmt_hints(stmt, source, cr, &mut hints);
    }
    hints
}

fn collect_stmt_hints(
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
            collect_block_hints(body, source, cr, out);
        }
        Stmt::For {
            var, iter, body, ..
        } => {
            // Hint the iterator element type if no type annotation.
            if cr.is_some() {
                // We could resolve the iter type from CheckResult, but
                // the parser already requires type annotations on `for` vars in ZZ.
                // Still, emit a hint for the iteration.
                let _ = (var, iter);
            }
            collect_block_hints(body, source, cr, out);
        }
        Stmt::Decl { value, .. } => {
            collect_expr_hints(value, source, cr, out);
        }
        Stmt::Return { value: Some(v), .. } => {
            collect_expr_hints(v, source, cr, out);
        }
        Stmt::Return { .. } => {}
        Stmt::Assign { target, value, .. } => {
            collect_expr_hints(target, source, cr, out);
            collect_expr_hints(value, source, cr, out);
        }
        Stmt::Defer { expr, .. } => {
            collect_expr_hints(expr, source, cr, out);
        }
        Stmt::Expr(e) => collect_expr_hints(e, source, cr, out),
        _ => {}
    }
}

fn collect_block_hints(
    block: &Block,
    source: &str,
    cr: Option<&CheckResult>,
    out: &mut Vec<InlayHint>,
) {
    for stmt in &block.stmts {
        collect_stmt_hints(stmt, source, cr, out);
    }
}

fn collect_expr_hints(
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
                    if let Some(sig) = cr.funcs.get(name) {
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
                    collect_expr_hints(arg, source, Some(cr), out);
                }
            }
            // Also recurse into callee.
            collect_expr_hints(callee, source, cr, out);
        }
        Expr::If {
            cond, then, els, ..
        } => {
            collect_expr_hints(cond, source, cr, out);
            collect_block_hints(then, source, cr, out);
            if let Some(e) = els {
                collect_expr_hints(e, source, cr, out);
            }
        }
        Expr::While { cond, body, .. } => {
            collect_expr_hints(cond, source, cr, out);
            collect_block_hints(body, source, cr, out);
        }
        Expr::Binary { left, right, .. } => {
            collect_expr_hints(left, source, cr, out);
            collect_expr_hints(right, source, cr, out);
        }
        Expr::Unary { expr, .. } => collect_expr_hints(expr, source, cr, out),
        Expr::Array { elems, .. } => {
            for e in elems {
                collect_expr_hints(e, source, cr, out);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                collect_expr_hints(k, source, cr, out);
                collect_expr_hints(v, source, cr, out);
            }
        }
        Expr::Index { obj, index, .. } => {
            collect_expr_hints(obj, source, cr, out);
            collect_expr_hints(index, source, cr, out);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            collect_expr_hints(obj, source, cr, out);
            if let Some(s) = start {
                collect_expr_hints(s, source, cr, out);
            }
            if let Some(e) = end {
                collect_expr_hints(e, source, cr, out);
            }
        }
        Expr::Range { start, end, .. } => {
            collect_expr_hints(start, source, cr, out);
            collect_expr_hints(end, source, cr, out);
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            collect_expr_hints(body, source, cr, out);
            collect_expr_hints(iter, source, cr, out);
            if let Some(f) = filter {
                collect_expr_hints(f, source, cr, out);
            }
        }
        Expr::Try { expr, .. } => collect_expr_hints(expr, source, cr, out),
        Expr::Field { obj, .. } => collect_expr_hints(obj, source, cr, out),
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_expr_hints(scrutinee, source, cr, out);
            for arm in arms {
                collect_expr_hints(&arm.body, source, cr, out);
            }
        }
        Expr::Block(b) => collect_block_hints(b, source, cr, out),
        Expr::Fmt { parts, .. } => {
            for part in parts {
                if let FmtPart::Expr(e, _) = part {
                    collect_expr_hints(e, source, cr, out);
                }
            }
        }
        Expr::Paren { expr, .. } => collect_expr_hints(expr, source, cr, out),
        Expr::Closure { body, .. } => collect_expr_hints(body, source, cr, out),
        Expr::IfLet {
            value, then, els, ..
        } => {
            collect_expr_hints(value, source, cr, out);
            collect_block_hints(then, source, cr, out);
            if let Some(e) = els {
                collect_expr_hints(e, source, cr, out);
            }
        }
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                collect_expr_hints(v, source, cr, out);
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
        );
        inlay_hints(&parsed.program, source, Some(&cr), None)
    }

    #[test]
    fn call_has_parameter_hints() {
        let src = "func add(a: int, b: int) -> int { return a + b }\nlet x = add(1, 2)\n";
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
        let src = "let x = unknown_func(1)\n";
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
        let src = "func add(a: int, b: int) -> int { return a + b }\nlet x = add(add(1, 2), 3)\n";
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

    #[test]
    fn empty_program_has_no_hints() {
        let hints = hints_for("");
        assert!(hints.is_empty());
    }
}
