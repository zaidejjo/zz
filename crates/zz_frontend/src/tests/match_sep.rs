//! Match-arm separator recovery tests: a forgotten `,`/`}` between arms
//! names the separator, offers the comma, and keeps parsing.

#[test]
fn missing_comma_between_arms_names_separator_and_fixit() {
    let parsed = crate::parse("match .ok(1) { .ok(n) => { n } .err(_) => { 0 } }");
    assert_eq!(
        parsed.errors.len(),
        1,
        "single error, no cascade: {:?}",
        parsed.errors
    );
    let err = &parsed.errors[0];
    assert!(
        err.message.contains("between match arms"),
        "message: {}",
        err.message
    );
    let fix = err
        .fixits
        .iter()
        .find(|f| f.replacement == ", ")
        .expect("expected comma-insert fixit");
    assert_eq!(fix.message, "insert missing comma");
}

#[test]
fn missing_comma_recovers_both_arms() {
    let parsed = crate::parse("match .ok(1) { .ok(n) => { n } .err(_) => { 0 } }");
    let stmt = parsed.program.stmts.first().expect("one statement");
    let body = match stmt {
        crate::ast::Stmt::Expr(e) => e,
        other => panic!("expected expr stmt, got {other:?}"),
    };
    match body {
        crate::ast::Expr::Match { arms, .. } => {
            assert_eq!(arms.len(), 2, "both arms recovered");
        }
        other => panic!("expected match, got {other:?}"),
    }
}

#[test]
fn dot_chain_with_nested_match_still_chains() {
    // The arm-boundary guard must not fire on `=>` nested inside
    // brackets: this method chain with a nested match parses cleanly.
    let parsed = crate::parse("x := foo(a.b, match y { .c => 1, _ => 0 })");
    assert!(parsed.errors.is_empty(), "errors: {:?}", parsed.errors);
}

#[test]
fn empty_match_arm_body_is_noop() {
    // `.none => ,` / `.none => }` (empty body) parses as a unit noop arm.
    let parsed = crate::parse("match .none { .none => , .some(x) => x }");
    assert!(parsed.errors.is_empty(), "errors: {:?}", parsed.errors);
    let stmt = parsed.program.stmts.first().expect("one statement");
    let body = match stmt {
        crate::ast::Stmt::Expr(e) => e,
        other => panic!("expected expr stmt, got {other:?}"),
    };
    match body {
        crate::ast::Expr::Match { arms, .. } => {
            assert_eq!(arms.len(), 2, "both arms parsed");
            // First arm body is an empty tuple (unit).
            match &arms[0].body {
                crate::ast::Expr::Tuple { items, .. } => {
                    assert!(items.is_empty(), "empty arm body is unit");
                }
                other => panic!("expected unit body, got {other:?}"),
            }
        }
        other => panic!("expected match, got {other:?}"),
    }
}
