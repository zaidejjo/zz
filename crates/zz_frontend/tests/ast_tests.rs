//! AST tests.

use zz_frontend::ast::{BinOp, Expr as E, UnOp};
use zz_frontend::tests::common::parse_ok;

#[test]
fn ast_span_access() {
    let p = parse_ok("x := 1 + 2");
    let stmt = &p.stmts[0];
    let span = stmt.span();
    assert!(span.start < span.end);
}

#[test]
fn expr_span_access() {
    let p = parse_ok("1 + 2");
    let stmt = &p.stmts[0];
    if let zz_frontend::ast::Stmt::Expr(e) = stmt {
        let span = e.span();
        assert!(span.start < span.end);
    } else {
        panic!("expected expr stmt");
    }
}

#[test]
fn binary_op_symbols() {
    assert_eq!(BinOp::Add.symbol(), "+");
    assert_eq!(BinOp::Sub.symbol(), "-");
    assert_eq!(BinOp::Mul.symbol(), "*");
    assert_eq!(BinOp::Div.symbol(), "/");
    assert_eq!(BinOp::Rem.symbol(), "%");
    assert_eq!(BinOp::Pow.symbol(), "**");
    assert_eq!(BinOp::Eq.symbol(), "==");
    assert_eq!(BinOp::Ne.symbol(), "!=");
    assert_eq!(BinOp::Lt.symbol(), "<");
    assert_eq!(BinOp::Gt.symbol(), ">");
    assert_eq!(BinOp::Le.symbol(), "<=");
    assert_eq!(BinOp::Ge.symbol(), ">=");
    assert_eq!(BinOp::And.symbol(), "&&");
    assert_eq!(BinOp::Or.symbol(), "||");
    assert_eq!(BinOp::Elvis.symbol(), "??");
}

#[test]
fn unary_op_symbols() {
    assert_eq!(UnOp::Neg.symbol(), "-");
    assert_eq!(UnOp::Pos.symbol(), "+");
    assert_eq!(UnOp::Not.symbol(), "!");
}

#[test]
fn pattern_span_access() {
    let p = parse_ok("match x { .some(v) => v }");
    if let zz_frontend::ast::Stmt::Expr(E::Match { arms, .. }) = &p.stmts[0] {
        let pat_span = arms[0].pat.span();
        assert!(pat_span.start < pat_span.end);
    } else {
        panic!("expected match");
    }
}
