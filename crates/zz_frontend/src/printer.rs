//! Lossless printer.
//!
//! Re-emits each AST node's source slice verbatim. Because every node carries
//! a span covering its exact source text, `print(parse(src)) == src` for any
//! valid program — whitespace, comments, and all.

use crate::ast::{Expr, Program, Stmt};
use crate::span::Span;

pub struct Printer<'a> {
    src: &'a str,
}

impl<'a> Printer<'a> {
    pub fn new(src: &'a str) -> Self {
        Printer { src }
    }

    /// Print the whole program. The program span covers the full buffer, so
    /// this reproduces the input exactly.
    pub fn print_program(&self, program: &Program) -> String {
        self.slice(program.span).to_string()
    }

    /// Print a single statement (used by future tooling).
    pub fn print_stmt(&self, stmt: &Stmt) -> String {
        self.slice(stmt.span()).to_string()
    }

    /// Print a single expression.
    pub fn print_expr(&self, expr: &Expr) -> String {
        self.slice(expr.span()).to_string()
    }

    fn slice(&self, span: Span) -> &'a str {
        &self.src[span.to_range()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    /// Round-trip: parse → print must reproduce the source exactly.
    fn round_trips(src: &str) {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let printed = Printer::new(src).print_program(&parsed.program);
        assert_eq!(printed, src, "round-trip failed");
    }

    #[test]
    fn round_trip_simple() {
        round_trips("let x = 1 + 2");
    }

    #[test]
    fn round_trip_multiple_statements() {
        round_trips("let a = 1\nlet b = 2\nlet c = a + b\n");
    }

    #[test]
    fn round_trip_weird_whitespace() {
        round_trips("let   x   =   1   +   2");
    }

    #[test]
    fn round_trip_comments() {
        round_trips("// header comment\nlet x = 1 // trailing\nlet y = 2\n");
    }

    #[test]
    fn round_trip_nested_parens() {
        round_trips("let x = ((1 + 2) * (3 - 4))");
    }

    #[test]
    fn round_trip_multiline_parens() {
        round_trips("let x = (1 +\n    2)\n");
    }

    #[test]
    fn round_trip_trailing_operator_continuation() {
        round_trips("let x = 1 +\n    2\n");
    }

    #[test]
    fn round_trip_block_comment() {
        round_trips("let x = 1 /* block\ncomment */ + 2\n");
    }

    #[test]
    fn round_trip_blank_lines() {
        round_trips("let a = 1\n\n\nlet b = 2\n");
    }

    #[test]
    fn round_trip_semicolons() {
        round_trips("let a = 1; let b = 2;");
    }

    #[test]
    fn round_trip_floats_and_unary() {
        round_trips("let x = -1.5 + 2.0\n");
    }

    #[test]
    fn round_trip_func() {
        round_trips("func add(a: int, b: int) -> int {\n    return a + b\n}\n");
    }

    #[test]
    fn round_trip_generic_func() {
        round_trips("func id<T>(x: T) -> T {\n    return x\n}\n");
    }

    #[test]
    fn round_trip_match() {
        round_trips("match x {\n    .ok(v) => v\n    .err(e) => 0\n}\n");
    }

    #[test]
    fn round_trip_match_single_line() {
        round_trips("match x { .ok(v) => v, .err(e) => 0 }\n");
    }

    #[test]
    fn round_trip_if_else() {
        round_trips("if x > 5 {\n    1\n} else {\n    2\n}\n");
    }

    #[test]
    fn round_trip_if_let() {
        round_trips("if let .some(x) = opt {\n    x\n} else {\n    0\n}\n");
    }

    #[test]
    fn round_trip_while() {
        round_trips("while x < 10 {\n    f(x)\n}\n");
    }

    #[test]
    fn round_trip_closure() {
        round_trips("let f = |x: int| x + 1\n");
    }

    #[test]
    fn round_trip_block_expr() {
        round_trips("let x = {\n    let y = 1\n    y + 1\n}\n");
    }

    #[test]
    fn round_trip_try_and_variants() {
        round_trips("let a = .ok(1)\nlet b = .none\nlet c = f()?\n");
    }

    #[test]
    fn round_trip_strings_and_bools() {
        round_trips("let s = \"hi\\n\"\nlet b = true && !false\n");
    }

    #[test]
    fn round_trip_annotated_types() {
        round_trips("let a: Option<int> = .none\nlet b: Result<int, str> = .ok(1)\n");
    }

    #[test]
    fn round_trip_multiline_call() {
        round_trips("f(\n    a,\n    b\n)\n");
    }

    #[test]
    fn round_trip_else_if_chain() {
        round_trips("if a {\n    1\n} else if b {\n    2\n} else {\n    3\n}\n");
    }
}
