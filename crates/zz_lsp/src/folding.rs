//! Folding ranges: provide folding ranges for function bodies, structs,
//! and multi-line blocks.

use tower_lsp::lsp_types::*;
use zz_frontend::ast::*;

/// Collect folding ranges for the given program.
pub fn folding_ranges(program: &Program, source: &str) -> Vec<FoldingRange> {
    let mut ranges = Vec::new();
    for stmt in &program.stmts {
        collect_stmt_folds(stmt, source, &mut ranges);
    }
    ranges
}

fn collect_stmt_folds(stmt: &Stmt, source: &str, out: &mut Vec<FoldingRange>) {
    match stmt {
        Stmt::Func { body, .. } => {
            push_block_fold(body, source, out);
            collect_block_folds(body, source, out);
        }
        Stmt::Struct { fields, .. } => {
            if !fields.is_empty() {
                let span = stmt.span();
                let start = crate::convert::offset_to_position(source, span.start);
                let end = crate::convert::offset_to_position(source, span.end.saturating_sub(1));
                out.push(FoldingRange {
                    start_line: start.line,
                    start_character: Some(start.character),
                    end_line: end.line,
                    end_character: Some(end.character),
                    kind: Some(FoldingRangeKind::Region),
                    collapsed_text: None,
                });
            }
        }
        Stmt::For { body, .. } => {
            push_block_fold(body, source, out);
            collect_block_folds(body, source, out);
        }
        Stmt::Decl { value, .. } => {
            collect_expr_folds(value, source, out);
        }
        Stmt::Return { value: Some(v), .. } => {
            collect_expr_folds(v, source, out);
        }
        Stmt::Return { .. } => {}
        Stmt::Assign { target, value, .. } => {
            collect_expr_folds(target, source, out);
            collect_expr_folds(value, source, out);
        }
        Stmt::Defer { expr, .. } => collect_expr_folds(expr, source, out),
        Stmt::Expr(e) => collect_expr_folds(e, source, out),
        _ => {}
    }
}

fn collect_block_folds(block: &Block, source: &str, out: &mut Vec<FoldingRange>) {
    for stmt in &block.stmts {
        collect_stmt_folds(stmt, source, out);
    }
}

fn collect_expr_folds(expr: &Expr, source: &str, out: &mut Vec<FoldingRange>) {
    match expr {
        Expr::If { then, els, .. } => {
            push_block_fold(then, source, out);
            collect_block_folds(then, source, out);
            if let Some(e) = els {
                match e.as_ref() {
                    Expr::Block(b) => {
                        push_block_fold(b, source, out);
                        collect_block_folds(b, source, out);
                    }
                    Expr::If { .. } => collect_expr_folds(e, source, out),
                    _ => {}
                }
            }
        }
        Expr::While { body, .. } => {
            push_block_fold(body, source, out);
            collect_block_folds(body, source, out);
        }
        Expr::Match { .. } => {
            // The entire match is a foldable region.
            let span = expr.span();
            let start = crate::convert::offset_to_position(source, span.start);
            let end = crate::convert::offset_to_position(source, span.end.saturating_sub(1));
            out.push(FoldingRange {
                start_line: start.line,
                start_character: Some(start.character),
                end_line: end.line,
                end_character: Some(end.character),
                kind: Some(FoldingRangeKind::Region),
                collapsed_text: None,
            });
        }
        Expr::IfLet { then, els, .. } => {
            push_block_fold(then, source, out);
            collect_block_folds(then, source, out);
            if let Some(e) = els {
                if let Expr::Block(b) = e.as_ref() {
                    push_block_fold(b, source, out);
                    collect_block_folds(b, source, out);
                }
            }
        }
        Expr::Block(b) => {
            push_block_fold(b, source, out);
            collect_block_folds(b, source, out);
        }
        Expr::Call { args, named, .. } => {
            for arg in args {
                collect_expr_folds(arg, source, out);
            }
            for (_, arg) in named {
                collect_expr_folds(arg, source, out);
            }
        }
        Expr::Binary { left, right, .. } => {
            collect_expr_folds(left, source, out);
            collect_expr_folds(right, source, out);
        }
        Expr::Unary { expr, .. } => collect_expr_folds(expr, source, out),
        Expr::Array { elems, .. } => {
            for e in elems {
                collect_expr_folds(e, source, out);
            }
        }
        Expr::Dict { entries, .. } => {
            for (k, v) in entries {
                collect_expr_folds(k, source, out);
                collect_expr_folds(v, source, out);
            }
        }
        Expr::Field { obj, .. } => collect_expr_folds(obj, source, out),
        Expr::Index { obj, index, .. } => {
            collect_expr_folds(obj, source, out);
            collect_expr_folds(index, source, out);
        }
        Expr::Slice {
            obj, start, end, ..
        } => {
            collect_expr_folds(obj, source, out);
            if let Some(s) = start {
                collect_expr_folds(s, source, out);
            }
            if let Some(e) = end {
                collect_expr_folds(e, source, out);
            }
        }
        Expr::Range { start, end, .. } => {
            collect_expr_folds(start, source, out);
            collect_expr_folds(end, source, out);
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            collect_expr_folds(body, source, out);
            collect_expr_folds(iter, source, out);
            if let Some(f) = filter {
                collect_expr_folds(f, source, out);
            }
        }
        Expr::Fmt { parts, .. } => {
            for part in parts {
                if let FmtPart::Expr(e, _) = part {
                    collect_expr_folds(e, source, out);
                }
            }
        }
        Expr::Try { expr, .. } => collect_expr_folds(expr, source, out),
        Expr::Paren { expr, .. } => collect_expr_folds(expr, source, out),
        Expr::StructInit { fields, .. } => {
            for (_, v) in fields {
                collect_expr_folds(v, source, out);
            }
        }
        Expr::Closure { body, .. } => collect_expr_folds(body, source, out),
        Expr::Variant { arg: Some(a), .. } => {
            collect_expr_folds(a, source, out);
        }
        Expr::Variant { .. } => {}
        _ => {}
    }
}

/// Push a folding range for a block body.
fn push_block_fold(block: &Block, source: &str, out: &mut Vec<FoldingRange>) {
    let span = block.span;
    let start = crate::convert::offset_to_position(source, span.start);
    let end = crate::convert::offset_to_position(source, span.end.saturating_sub(1));
    out.push(FoldingRange {
        start_line: start.line,
        start_character: Some(start.character),
        end_line: end.line,
        end_character: Some(end.character),
        kind: Some(FoldingRangeKind::Region),
        collapsed_text: None,
    });
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zz_frontend::parse;

    #[test]
    fn func_body_fold() {
        let src = "func f() -> int {\n  return 1\n}\n";
        let parsed = parse(src);
        let folds = folding_ranges(&parsed.program, src);
        assert!(!folds.is_empty(), "should have fold for function body");
        let f: Vec<_> = folds
            .iter()
            .filter(|f| f.kind == Some(FoldingRangeKind::Region))
            .collect();
        assert!(!f.is_empty(), "should have region folds");
    }

    #[test]
    fn struct_fold() {
        let src = "struct Point {\n  x: int,\n  y: int\n}\n";
        let parsed = parse(src);
        let folds = folding_ranges(&parsed.program, src);
        assert!(!folds.is_empty(), "should have fold for struct");
    }

    #[test]
    fn if_else_fold() {
        let src = "if true {\n  return 1\n} else {\n  return 2\n}\n";
        let parsed = parse(src);
        let folds = folding_ranges(&parsed.program, src);
        // Should have folds for both blocks.
        assert!(
            folds.len() >= 2,
            "expected >= 2 folds for if/else, got {}",
            folds.len()
        );
    }

    #[test]
    fn for_loop_fold() {
        let src = "for x in items {\n  print(x)\n}\n";
        let parsed = parse(src);
        let folds = folding_ranges(&parsed.program, src);
        assert!(!folds.is_empty(), "should have fold for for-loop body");
    }

    #[test]
    fn nested_folds() {
        let src = "func f() {\n  if true {\n    for x in items {\n      print(x)\n    }\n  }\n}\n";
        let parsed = parse(src);
        let folds = folding_ranges(&parsed.program, src);
        // Should have folds for: func body, if body, for body.
        assert!(
            folds.len() >= 3,
            "expected >= 3 nested folds, got {}",
            folds.len()
        );
    }

    #[test]
    fn empty_program_no_folds() {
        let parsed = parse("");
        let folds = folding_ranges(&parsed.program, "");
        assert!(folds.is_empty());
    }

    #[test]
    fn while_loop_fold() {
        let src = "while true {\n  break\n}\n";
        let parsed = parse(src);
        let folds = folding_ranges(&parsed.program, src);
        assert!(!folds.is_empty(), "should have fold for while body");
    }
}
