//! Compile-time metadata for `@bench`.
//!
//! Same mechanism as [`crate::test_attr`]: exempt from decorator lowering,
//! preserved on the `Func` node, validated here, discovered by `zz bench`.
//!
//! Grammar (all named args, no positional):
//! ```text
//! @bench
//! @bench(iters = 1000)
//! @bench(warmup = 100)
//! @bench(iters = 500, warmup = 50)
//! @bench(tag = "math")
//! ```
//! Bench functions take no parameters and must not be generic (diagnosed).
//! Mixed stacks (`@bench` beside ordinary decorators, or beside
//! `@test`/`@setup`/`@teardown`) are rejected.

use crate::ast::{Decorator, Expr};
use crate::span::Span;
use zz_diag::{error_at, RawDiag};

/// Parsed `@bench` metadata. One per `@bench`-decorated function.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchMeta {
    /// Timed iterations (default comes from the runner CLI).
    pub iters: Option<u64>,
    /// Untimed warmup iterations (default comes from the runner CLI).
    pub warmup: Option<u64>,
    /// Category for `--tag` filtering.
    pub tag: Option<String>,
    pub span: Span,
}

impl BenchMeta {
    pub fn empty(span: Span) -> Self {
        Self {
            iters: None,
            warmup: None,
            tag: None,
            span,
        }
    }
}

// ---------------------------------------------------------------------------
// classifiers

pub fn is_bench_decorator(d: &Decorator) -> bool {
    d.path.len() == 1 && d.path[0] == "bench"
}

pub fn is_bench_like(d: &Decorator) -> bool {
    is_bench_decorator(d)
}

// ---------------------------------------------------------------------------
// parsing

/// Parse a single `@bench` decorator. Returns `BenchMeta` on success and
/// pushes diagnostics for invalid arguments. On hard error `None` is
/// returned (caller treats the function as a bench with defaults so
/// discovery is not silently skipped).
pub fn parse_bench_meta(dec: &Decorator, diags: &mut Vec<RawDiag>) -> Option<BenchMeta> {
    if !is_bench_decorator(dec) {
        return None;
    }
    let mut meta = BenchMeta::empty(dec.span);

    if !dec.args.is_empty() {
        diags.push(error_at(
            "@bench does not accept positional arguments (use named arguments like `iters = 1000`)",
            dec.args[0].span(),
        ));
    }

    let mut seen_iters = false;
    let mut seen_warmup = false;
    let mut seen_tag = false;

    for (name, expr) in &dec.named {
        match name.name.as_str() {
            "iters" => {
                if seen_iters {
                    diags.push(error_at("duplicate `@bench` argument `iters`", dec.span));
                    continue;
                }
                seen_iters = true;
                match as_pos_int(expr) {
                    Some(n) => meta.iters = Some(n),
                    None => diags.push(error_at(
                        "`iters` expects a positive integer literal",
                        expr.span(),
                    )),
                }
            }
            "warmup" => {
                if seen_warmup {
                    diags.push(error_at("duplicate `@bench` argument `warmup`", dec.span));
                    continue;
                }
                seen_warmup = true;
                match as_nonneg_int(expr) {
                    Some(n) => meta.warmup = Some(n),
                    None => diags.push(error_at(
                        "`warmup` expects a non-negative integer literal",
                        expr.span(),
                    )),
                }
            }
            "tag" => {
                if seen_tag {
                    diags.push(error_at("duplicate `@bench` argument `tag`", dec.span));
                    continue;
                }
                seen_tag = true;
                match as_nonempty_str(expr) {
                    Some(s) => meta.tag = Some(s),
                    None => diags.push(error_at(
                        "`tag` expects a non-empty string literal",
                        expr.span(),
                    )),
                }
            }
            other => {
                diags.push(error_at(
                    format!(
                        "unknown `@bench` argument `{other}` (expected one of: iters, warmup, tag)"
                    ),
                    expr.span(),
                ));
            }
        }
    }

    Some(meta)
}

fn as_pos_int(e: &Expr) -> Option<u64> {
    match e {
        Expr::Int { value, .. } if *value > 0 => Some(*value as u64),
        Expr::Paren { expr, .. } => as_pos_int(expr),
        _ => None,
    }
}

fn as_nonneg_int(e: &Expr) -> Option<u64> {
    match e {
        Expr::Int { value, .. } if *value >= 0 => Some(*value as u64),
        Expr::Paren { expr, .. } => as_nonneg_int(expr),
        _ => None,
    }
}

fn as_nonempty_str(e: &Expr) -> Option<String> {
    match e {
        Expr::Str { value, .. } if !value.is_empty() => Some(value.clone()),
        Expr::Paren { expr, .. } => as_nonempty_str(expr),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// discovery helpers (used by runner / lint)

/// True if `func` has at least one `@bench` decorator.
pub fn is_bench_func(decorators: &[Decorator]) -> bool {
    decorators.iter().any(is_bench_decorator)
}

/// Validate a whole decorator list attached to a func. Returns parsed
/// `BenchMeta` if the func is a bench; pushes diagnostics for invalid
/// combinations (multiple `@bench`, mixing with other decorators,
/// parameters, generics).
#[allow(clippy::too_many_arguments)]
pub fn validate_bench_decorator_list(
    decorators: &[Decorator],
    func_span: Span,
    generics_empty: bool,
    params_empty: bool,
    diags: &mut Vec<RawDiag>,
) -> Option<BenchMeta> {
    use crate::test_attr as test_attr_mod;

    let bench_count = decorators.iter().filter(|d| is_bench_decorator(d)).count();
    if bench_count == 0 {
        return None;
    }

    if bench_count > 1 {
        diags.push(error_at("only one `@bench` per function", func_span));
    }

    let test_like_count = decorators
        .iter()
        .filter(|d| test_attr_mod::is_test_like(d))
        .count();
    let ordinary_count = decorators.len() - bench_count - test_like_count;
    if test_like_count > 0 || ordinary_count > 0 {
        diags.push(error_at(
            "cannot mix `@bench` with `@test`/`@setup`/`@teardown` or ordinary decorators on the same function",
            func_span,
        ));
    }

    if !generics_empty {
        diags.push(error_at(
            "bench functions cannot be generic (benchmark a concrete wrapper instead)",
            func_span,
        ));
    }

    if !params_empty {
        diags.push(error_at(
            "bench functions take no parameters (move inputs inside the function)",
            func_span,
        ));
    }

    let mut bench_meta: Option<BenchMeta> = None;
    for dec in decorators {
        if is_bench_decorator(dec) && bench_meta.is_none() {
            bench_meta = parse_bench_meta(dec, diags);
        }
    }
    bench_meta
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Decorator;

    fn dec_bench(named: Vec<(&str, Expr)>, args: Vec<Expr>) -> Decorator {
        use crate::ast::Ident;
        Decorator {
            path: vec!["bench".into()],
            args,
            named: named
                .into_iter()
                .map(|(k, v)| {
                    (
                        Ident {
                            name: k.into(),
                            span: Span::new(0, 1),
                        },
                        v,
                    )
                })
                .collect(),
            span: Span::new(0, 5),
        }
    }

    fn lit_int(n: i64) -> Expr {
        Expr::Int {
            value: n,
            span: Span::new(0, 1),
        }
    }

    #[test]
    fn parse_empty_bench_ok() {
        let mut diags = Vec::new();
        let m = parse_bench_meta(&dec_bench(vec![], vec![]), &mut diags).unwrap();
        assert!(diags.is_empty());
        assert!(m.iters.is_none());
        assert!(m.warmup.is_none());
    }

    #[test]
    fn parse_iters_warmup() {
        let mut diags = Vec::new();
        let dec = dec_bench(
            vec![("iters", lit_int(500)), ("warmup", lit_int(50))],
            vec![],
        );
        let m = parse_bench_meta(&dec, &mut diags).unwrap();
        assert!(diags.is_empty());
        assert_eq!(m.iters, Some(500));
        assert_eq!(m.warmup, Some(50));
    }

    #[test]
    fn iters_must_be_positive() {
        let mut diags = Vec::new();
        let dec = dec_bench(vec![("iters", lit_int(0))], vec![]);
        let _ = parse_bench_meta(&dec, &mut diags);
        assert!(diags.iter().any(|d| d.message.contains("iters")));
    }

    #[test]
    fn warmup_allows_zero() {
        let mut diags = Vec::new();
        let dec = dec_bench(vec![("warmup", lit_int(0))], vec![]);
        let m = parse_bench_meta(&dec, &mut diags).unwrap();
        assert!(diags.is_empty());
        assert_eq!(m.warmup, Some(0));
    }

    #[test]
    fn unknown_arg_errors() {
        let mut diags = Vec::new();
        let dec = dec_bench(vec![("bogus", lit_int(1))], vec![]);
        let _ = parse_bench_meta(&dec, &mut diags);
        assert!(diags.iter().any(|d| d.message.contains("unknown")));
    }

    #[test]
    fn positional_rejected() {
        let mut diags = Vec::new();
        let dec = dec_bench(vec![], vec![lit_int(1)]);
        let _ = parse_bench_meta(&dec, &mut diags);
        assert!(diags.iter().any(|d| d.message.contains("positional")));
    }
}
