//! Printer tests (lossless round-trip).

use zz_frontend::tests::common::parse_ok;
use zz_frontend::Printer;

fn round_trips(src: &str) {
    let parsed = parse_ok(src);
    let printed = Printer::new(src).print_program(&parsed);
    assert_eq!(printed, src, "round-trip failed");
}

#[test]
fn round_trip_simple() {
    round_trips("x := 1 + 2");
}

#[test]
fn round_trip_short_decl() {
    round_trips("x := 10");
}

#[test]
fn round_trip_strings_and_bools() {
    round_trips("s := \"hello\"\nb := true");
}

#[test]
fn round_trip_struct() {
    round_trips("struct Point { x: int, y: int }");
}

#[test]
fn round_trip_struct_init() {
    round_trips("p := Point { x: 1, y: 2 }");
}

#[test]
fn round_trip_if_else() {
    round_trips("if x > 5 { 1 } else { 2 }");
}

#[test]
fn round_trip_if_let() {
    round_trips("if let .some(x) = opt { x } else { 0 }");
}

#[test]
fn round_trip_while() {
    round_trips("while x < 10 { f(x) }");
}

#[test]
fn round_trip_for_loop() {
    round_trips("for x in xs { y := 1 }");
}

#[test]
fn round_trip_func() {
    round_trips("func add(a: int, b: int) -> int { return a + b }");
}

#[test]
fn round_trip_generic_func() {
    round_trips("func id<T>(x: T) -> T { return x }");
}

#[test]
fn round_trip_import() {
    round_trips("import std.io");
}

#[test]
fn round_trip_import_alias() {
    round_trips("import std.io as console");
}

#[test]
fn round_trip_match() {
    round_trips("match x { .ok(v) => v, .err(e) => 0 }");
}

#[test]
fn round_trip_match_single_line() {
    round_trips("match x {\n    .some(v) => v\n    .none => 0\n}");
}

#[test]
fn round_trip_try_and_variants() {
    round_trips("a := .ok(1)\nb := .none\nc := f()?\n");
}

#[test]
fn round_trip_range() {
    round_trips("for i in 0..10 { i }");
}

#[test]
fn round_trip_semicolons() {
    round_trips("a := 1; b := 2");
}

#[test]
fn round_trip_multiple_statements() {
    round_trips("a := 1\nb := 2\nc := a + b");
}

#[test]
fn round_trip_nested_parens() {
    round_trips("((1 + 2) * 3)");
}

#[test]
fn round_trip_multiline_parens() {
    round_trips("(\n  1 +\n  2\n)");
}

#[test]
fn round_trip_multiline_call() {
    round_trips("f(\n  1,\n  2\n)");
}

#[test]
fn round_trip_trailing_operator_continuation() {
    round_trips("1 +\n2");
}

#[test]
fn round_trip_weird_whitespace() {
    // Printer preserves exact whitespace
    round_trips("x  :=  1  +  2");
}

#[test]
fn round_trip_union_dict() {
    round_trips("user: {str: str | int} = {\"name\": \"Zaid\", \"age\": 20}");
}

#[test]
fn round_trip_fstring() {
    round_trips("s := \"Hello {name}!\"");
}
