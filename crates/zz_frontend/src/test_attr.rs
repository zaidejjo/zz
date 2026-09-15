//! Compile-time metadata for `@test`, `@setup`, `@teardown`.
//!
//! These decorators are *zero-magic*: they do not lower to wrapper functions.
//! `decorators::expand_program` exempts them (keeps them on the `Func` node)
//! and validates their arguments here. Discovery / runner code then reads the
//! preserved decorators via [`parse_test_meta`].
//!
//! Grammar (all named args, no positional):
//! ```text
//! @test
//! @test(should_panic = true)
//! @test(should_fail = true)   // alias
//! @test(ignore = true, reason = "flaky env")
//! @test(skip = true)          // alias for ignore
//! @test(timeout = 500)        // ms
//! @test(retry = 3)
//! @test(tag = "integration")
//! @test(cases = [[1, 2], [2, 3]])
//! @setup
//! @teardown
//! ```
//! Mixed stacks (`@test` beside ordinary decorators) are rejected.
//! `cases` currently requires an array literal; non-literals are diagnosed
//! and the test is treated as having no cases rather than expanding.

use crate::ast::{Decorator, Expr};
use crate::diag::{error_at, RawDiag};
use crate::span::Span;

/// Parsed `@test` metadata. One per `@test`-decorated function.
#[derive(Debug, Clone, PartialEq)]
pub struct TestMeta {
    pub should_panic: bool,
    pub ignore: bool,
    pub reason: Option<String>,
    pub timeout_ms: Option<u64>,
    pub retry: Option<u32>,
    pub tag: Option<String>,
    /// Each entry is the raw case expression (must be an array literal).
    /// `None` = not a parameterized test.
    pub cases: Option<Vec<Expr>>,
    pub span: Span,
}

impl TestMeta {
    fn empty(span: Span) -> Self {
        Self {
            should_panic: false,
            ignore: false,
            reason: None,
            timeout_ms: None,
            retry: None,
            tag: None,
            cases: None,
            span,
        }
    }
}

// ---------------------------------------------------------------------------
// classifiers

pub fn is_test_decorator(d: &Decorator) -> bool {
    d.path.len() == 1 && d.path[0] == "test"
}

pub fn is_setup_decorator(d: &Decorator) -> bool {
    d.path.len() == 1 && d.path[0] == "setup"
}

pub fn is_teardown_decorator(d: &Decorator) -> bool {
    d.path.len() == 1 && d.path[0] == "teardown"
}

pub fn is_test_like(d: &Decorator) -> bool {
    is_test_decorator(d) || is_setup_decorator(d) || is_teardown_decorator(d)
}

// ---------------------------------------------------------------------------
// parsing

/// Parse a single `@test` decorator. Returns `TestMeta` on success and pushes
/// diagnostics for any invalid arguments. On hard error, `None` is returned
/// (caller should still treat the function as a test with default metadata so
/// discovery is not silently skipped).
pub fn parse_test_meta(dec: &Decorator, diags: &mut Vec<RawDiag>) -> Option<TestMeta> {
    if !is_test_decorator(dec) {
        return None;
    }
    let mut meta = TestMeta::empty(dec.span);

    if !dec.args.is_empty() {
        diags.push(error_at(
            "@test does not accept positional arguments (use named arguments like `should_panic = true`)",
            dec.args[0].span(),
        ));
    }

    // Track duplicates.
    let mut seen_should_panic = false;
    let mut seen_should_fail = false;
    let mut seen_ignore = false;
    let mut seen_skip = false;
    let mut seen_reason = false;
    let mut seen_timeout = false;
    let mut seen_retry = false;
    let mut seen_tag = false;
    let mut seen_cases = false;

    for (name, expr) in &dec.named {
        match name.as_str() {
            "should_panic" => {
                if seen_should_panic {
                    diags.push(error_at(
                        "duplicate `@test` argument `should_panic`",
                        dec.span,
                    ));
                    continue;
                }
                seen_should_panic = true;
                match as_bool(expr) {
                    Some(b) => meta.should_panic = b,
                    None => diags.push(error_at(
                        "`should_panic` expects a boolean (`true` / `false`)",
                        expr.span(),
                    )),
                }
            }
            "should_fail" => {
                if seen_should_fail {
                    diags.push(error_at(
                        "duplicate `@test` argument `should_fail`",
                        dec.span,
                    ));
                    continue;
                }
                seen_should_fail = true;
                match as_bool(expr) {
                    Some(b) => meta.should_panic = b,
                    None => diags.push(error_at(
                        "`should_fail` expects a boolean (`true` / `false`)",
                        expr.span(),
                    )),
                }
            }
            "ignore" => {
                if seen_ignore {
                    diags.push(error_at("duplicate `@test` argument `ignore`", dec.span));
                    continue;
                }
                seen_ignore = true;
                match as_bool(expr) {
                    Some(b) => meta.ignore = b,
                    None => diags.push(error_at(
                        "`ignore` expects a boolean (`true` / `false`)",
                        expr.span(),
                    )),
                }
            }
            "skip" => {
                if seen_skip {
                    diags.push(error_at("duplicate `@test` argument `skip`", dec.span));
                    continue;
                }
                seen_skip = true;
                match as_bool(expr) {
                    Some(b) => meta.ignore = b,
                    None => diags.push(error_at(
                        "`skip` expects a boolean (`true` / `false`)",
                        expr.span(),
                    )),
                }
            }
            "reason" => {
                if seen_reason {
                    diags.push(error_at("duplicate `@test` argument `reason`", dec.span));
                    continue;
                }
                seen_reason = true;
                match as_str(expr) {
                    Some(s) => meta.reason = Some(s),
                    None => diags.push(error_at("`reason` expects a string literal", expr.span())),
                }
            }
            "timeout" => {
                if seen_timeout {
                    diags.push(error_at("duplicate `@test` argument `timeout`", dec.span));
                    continue;
                }
                seen_timeout = true;
                match as_int(expr) {
                    Some(n) if n > 0 => meta.timeout_ms = Some(n as u64),
                    Some(0) => diags.push(error_at(
                        "`timeout` must be > 0 (milliseconds)",
                        expr.span(),
                    )),
                    Some(_) => diags.push(error_at(
                        "`timeout` must be a positive integer (milliseconds)",
                        expr.span(),
                    )),
                    None => diags.push(error_at(
                        "`timeout` expects an integer literal (milliseconds)",
                        expr.span(),
                    )),
                }
            }
            "retry" => {
                if seen_retry {
                    diags.push(error_at("duplicate `@test` argument `retry`", dec.span));
                    continue;
                }
                seen_retry = true;
                match as_int(expr) {
                    Some(n) if n >= 1 => meta.retry = Some(n as u32),
                    Some(_) => diags.push(error_at(
                        "`retry` must be a positive integer (>= 1)",
                        expr.span(),
                    )),
                    None => diags.push(error_at("`retry` expects an integer literal", expr.span())),
                }
            }
            "tag" => {
                if seen_tag {
                    diags.push(error_at("duplicate `@test` argument `tag`", dec.span));
                    continue;
                }
                seen_tag = true;
                match as_str(expr) {
                    Some(s) if !s.is_empty() => meta.tag = Some(s),
                    Some(_) => {
                        diags.push(error_at("`tag` must be a non-empty string", expr.span()))
                    }
                    None => diags.push(error_at("`tag` expects a string literal", expr.span())),
                }
            }
            "cases" => {
                if seen_cases {
                    diags.push(error_at("duplicate `@test` argument `cases`", dec.span));
                    continue;
                }
                seen_cases = true;
                match expr {
                    Expr::Array { elems, .. } => {
                        if elems.is_empty() {
                            diags.push(error_at(
                                "`cases` must be a non-empty array literal",
                                expr.span(),
                            ));
                        } else {
                            meta.cases = Some(elems.clone());
                        }
                    }
                    _ => diags.push(error_at(
                        "`cases` expects an array literal (e.g. `cases = [[1, 2], [3, 4]]`)",
                        expr.span(),
                    )),
                }
            }
            other => {
                diags.push(error_at(
                    format!(
                        "unknown `@test` argument `{other}` (expected one of: should_panic, should_fail, ignore, skip, reason, timeout, retry, tag, cases)"
                    ),
                    expr.span(),
                ));
            }
        }
    }

    // Cross-field validation.
    if seen_should_panic && seen_should_fail {
        // Both aliases set — allow, but note redundancy. Not an error; last wins.
        // We already merged into should_panic, so just warn via diag? Keep silent
        // to avoid noise; document that should_fail is an alias.
    }
    if (seen_ignore || seen_skip) && seen_should_panic {
        // ignore + should_panic is allowed (skip takes precedence) — no error.
        // But should_panic + retry is incompatible per spec.
    }
    if meta.should_panic && meta.retry.is_some() {
        diags.push(error_at(
            "cannot combine `should_panic`/`should_fail` with `retry` (a retried should_panic test is ambiguous)",
            dec.span,
        ));
    }
    if meta.reason.is_some() && !meta.ignore {
        diags.push(error_at(
            "`reason` requires `ignore = true` or `skip = true`",
            dec.span,
        ));
    }
    if seen_skip && seen_ignore {
        // Both aliases present — allowed, last write wins (above).
        // No extra diagnostic.
    }

    Some(meta)
}

/// Validate a `@setup` or `@teardown` decorator: no args allowed.
pub fn validate_setup_teardown(dec: &Decorator, diags: &mut Vec<RawDiag>) {
    if !dec.args.is_empty() {
        diags.push(error_at(
            format!("@{} does not accept positional arguments", dec.path[0]),
            dec.args[0].span(),
        ));
    }
    if !dec.named.is_empty() {
        let (name, expr) = &dec.named[0];
        diags.push(error_at(
            format!(
                "@{} does not accept arguments (found `{name} = ...`)",
                dec.path[0]
            ),
            expr.span(),
        ));
    }
}

// ---------------------------------------------------------------------------
// literal helpers

fn as_bool(e: &Expr) -> Option<bool> {
    match e {
        Expr::Bool { value, .. } => Some(*value),
        _ => None,
    }
}

fn as_int(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int { value, .. } => Some(*value),
        // Allow parenthesized literals: `(500)`
        Expr::Paren { expr, .. } => as_int(expr),
        _ => None,
    }
}

fn as_str(e: &Expr) -> Option<String> {
    match e {
        Expr::Str { value, .. } => Some(value.clone()),
        Expr::Paren { expr, .. } => as_str(expr),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// discovery helpers (used by runner / lint)

/// True if `func` has at least one `@test` decorator.
pub fn is_test_func(decorators: &[Decorator]) -> bool {
    decorators.iter().any(is_test_decorator)
}

/// Collect test-like validation for a whole decorator list attached to a func.
/// Returns parsed `TestMeta` if the func is a test; pushes diagnostics for all
/// invalid combinations (including mixed stacks).
pub fn validate_test_decorator_list(
    decorators: &[Decorator],
    func_span: Span,
    generics_empty: bool,
    diags: &mut Vec<RawDiag>,
) -> Option<TestMeta> {
    let test_like_count = decorators.iter().filter(|d| is_test_like(d)).count();
    let ordinary_count = decorators.len() - test_like_count;

    if test_like_count == 0 {
        return None;
    }

    // Exactly one test-like decorator per func.
    if test_like_count > 1 {
        diags.push(error_at(
            "only one of `@test` / `@setup` / `@teardown` per function",
            func_span,
        ));
        // Continue to parse the first test decorator for recovery.
    }

    if ordinary_count > 0 {
        diags.push(error_at(
            "cannot mix `@test`/`@setup`/`@teardown` with ordinary decorators on the same function",
            func_span,
        ));
    }

    if !generics_empty {
        diags.push(error_at(
            "test/setup/teardown functions cannot be generic (decorate a concrete wrapper instead)",
            func_span,
        ));
    }

    // Find the test decorator (if any) and parse it.
    let mut test_meta: Option<TestMeta> = None;
    for dec in decorators {
        if is_test_decorator(dec) {
            if test_meta.is_none() {
                test_meta = parse_test_meta(dec, diags);
            }
        } else if is_setup_decorator(dec) || is_teardown_decorator(dec) {
            validate_setup_teardown(dec, diags);
        }
    }

    // Recurse into validation for cases: each case element must be literal-ish.
    // We already required Array literal; optionally lint that elements are literals
    // is left to the runner (non-literal cases error at harness time, not parse time,
    // to keep parser tolerant). But we can soft-check here.
    test_meta
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Decorator;
    use crate::span::Span;

    fn dec_test(named: Vec<(&str, Expr)>, args: Vec<Expr>) -> Decorator {
        Decorator {
            path: vec!["test".into()],
            args,
            named: named.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            span: Span::new(0, 5),
        }
    }

    fn lit_int(n: i64) -> Expr {
        Expr::Int {
            value: n,
            span: Span::new(0, 1),
        }
    }
    fn lit_bool(b: bool) -> Expr {
        Expr::Bool {
            value: b,
            span: Span::new(0, 1),
        }
    }
    fn lit_str(s: &str) -> Expr {
        Expr::Str {
            value: s.into(),
            span: Span::new(0, 1),
        }
    }
    fn lit_array(elems: Vec<Expr>) -> Expr {
        Expr::Array {
            elems,
            span: Span::new(0, 1),
        }
    }

    #[test]
    fn parse_empty_test_ok() {
        let mut diags = Vec::new();
        let m = parse_test_meta(&dec_test(vec![], vec![]), &mut diags).unwrap();
        assert!(diags.is_empty());
        assert!(!m.should_panic);
        assert!(m.timeout_ms.is_none());
    }

    #[test]
    fn parse_all_flags() {
        let mut diags = Vec::new();
        let dec = dec_test(
            vec![
                ("should_panic", lit_bool(true)),
                ("timeout", lit_int(500)),
                ("retry", lit_int(3)),
            ],
            vec![],
        );
        // should_panic + retry should error.
        let _ = parse_test_meta(&dec, &mut diags);
        assert!(diags.iter().any(|d| d.message.contains("retry")));
    }

    #[test]
    fn reason_without_ignore_errors() {
        let mut diags = Vec::new();
        let dec = dec_test(vec![("reason", lit_str("because"))], vec![]);
        let _ = parse_test_meta(&dec, &mut diags);
        assert!(diags.iter().any(|d| d.message.contains("reason")));
    }

    #[test]
    fn cases_requires_array() {
        let mut diags = Vec::new();
        let dec = dec_test(vec![("cases", lit_int(1))], vec![]);
        let _ = parse_test_meta(&dec, &mut diags);
        assert!(diags.iter().any(|d| d.message.contains("cases")));
    }

    #[test]
    fn cases_empty_array_errors() {
        let mut diags = Vec::new();
        let dec = dec_test(vec![("cases", lit_array(vec![]))], vec![]);
        let _ = parse_test_meta(&dec, &mut diags);
        assert!(diags.iter().any(|d| d.message.contains("non-empty")));
    }

    #[test]
    fn unknown_arg_errors() {
        let mut diags = Vec::new();
        let dec = dec_test(vec![("bogus", lit_int(1))], vec![]);
        let _ = parse_test_meta(&dec, &mut diags);
        assert!(diags.iter().any(|d| d.message.contains("unknown")));
    }

    #[test]
    fn positional_rejected() {
        let mut diags = Vec::new();
        let dec = dec_test(vec![], vec![lit_int(1)]);
        let _ = parse_test_meta(&dec, &mut diags);
        assert!(diags.iter().any(|d| d.message.contains("positional")));
    }
}
