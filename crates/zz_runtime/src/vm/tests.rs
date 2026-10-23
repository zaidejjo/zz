use std::sync::Arc;

use zz_frontend::ast::{BinOp, Block, Expr, Ident, Param};
use zz_frontend::parse;
use zz_frontend::span::Span;

use super::{Compiler, Op, Vm};
use crate::env::{Env, EnvLink};
use crate::eval::Interp;
use crate::value::{FuncValue, Value};
use crate::EvalError;

fn run_src(src: &str) -> Result<Value, EvalError> {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let mut interp = Interp::new();
    interp.run(&parsed.program)
}

fn run_tree(src: &str) -> Result<Value, EvalError> {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let mut interp = Interp::new();
    interp.run_tree_walker(&parsed.program)
}

/// Differential test: the VM and the tree-walker must agree.
fn assert_same(src: &str) {
    let vm = run_src(src);
    let tree = run_tree(src);
    match (&vm, &tree) {
        (Ok(a), Ok(b)) => assert_eq!(a, b, "VM and tree-walker disagree on: {src}"),
        (Err(a), Err(b)) => assert_eq!(
            a.message, b.message,
            "VM and tree-walker disagree on error for: {src}"
        ),
        _ => panic!("VM and tree-walker disagree on: {src}\nVM: {vm:?}\ntree: {tree:?}"),
    }
}

#[test]
fn vm_nested_path_assignment_keeps_shape() {
    for src in [
        "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x = 9\nr.p.x",
        "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x = 9\nr.w",
        "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x = 9\nr.p.y",
        "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x = r.p.x + 8\nr.p.x",
        "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nfunc f(r: Rect) -> int { r.p.x = 9\nreturn r.p.x + r.w }\nf(Rect{ p: Point{ x: 1, y: 2 }, w: 3 })",
        "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nfunc f(r: Rect) -> int { r.p.x = 9\nreturn r.p.x }\nf(Rect{ p: Point{ x: 1, y: 2 }, w: 3 })",
        "struct A { b: B }\nstruct B { c: C }\nstruct C { v: int }\na := A{ b: B{ c: C{ v: 1 } } }\na.b.c.v = 42\na.b.c.v",
        "struct A { b: B }\nstruct B { c: C }\nstruct C { v: int }\nfunc f(a: A) -> int { a.b.c.v = 42\nreturn a.b.c.v }\nf(A{ b: B{ c: C{ v: 1 } } })",
        "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x = 9\nr.p.x + r.p.y + r.w",
    ] {
        assert_same(src);
    }
    assert_eq!(
        run_src(
            "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x = 9\nr.w"
        )
        .unwrap(),
        Value::Int(3)
    );
}

#[test]
fn vm_chained_index_store_writes_through() {
    // `m[0][0] = v` must reach the root binding (write-through, matching
    // the AOT engine) instead of being dropped with the temp clone.
    for src in [
        "m := [[1, 2], [3, 4]]\nm[0][0] = 99\nm[0][0]",
        "m := [[1, 2], [3, 4]]\nm[0][1] = m[0][1] + 10\nm[0][1]",
        "m := [[1, 2], [3, 4]]\nm[0][1] += 10\nm[0][1]",
        "a := [[[1]]]\na[0][0][0] = 7\na[0][0][0]",
        "d := {\"a\": [1, 2]}\nd[\"a\"][1] = 9\nd[\"a\"][1]",
        "m := [[1, 2], [3, 4]]\nm[1] = [8, 9]\nm[1][0]",
        "a := [1, 2, 3]\na[1] = 9\na[1]",
        "m := [[1, 2], [3, 4]]\nm[0][0] = 1\nm[1][1] = 2\nm[0][0] + m[1][1]",
    ] {
        assert_same(src);
    }
    assert_eq!(
        run_src("m := [[1, 2], [3, 4]]\nm[0][0] = 99\nm[0][0]").unwrap(),
        Value::Int(99)
    );
    assert_eq!(
        run_src("m := [[1, 2], [3, 4]]\nm[0][1] += 10\nm[0][1]").unwrap(),
        Value::Int(12)
    );
    assert_eq!(
        run_src("a := [[[1]]]\na[0][0][0] = 7\na[0][0][0]").unwrap(),
        Value::Int(7)
    );
}

#[test]
fn vm_slot_locals_match_tree_walker() {
    for src in [
        "x := 1\n{ x := 2\nx }\nx",
        "x := 1\n{ y := 2\n{ z := 3\nx + y + z } }",
        "x := 1\n{ x := 2\n{ x := 3\nx } }",
        "x := 1\ny := 2\n{ y := 3\ny }\ny",
        "func f() -> int { a := 1\nb := 2\nc := a + b\nreturn c }\nf()",
        "func f(n: int) -> int { m := n * 2\nreturn m + n }\nf(5)",
        "func f(n: int) -> int { n := n + 1\nreturn n }\nf(5)",
        "func f() -> int { x := 1\n{ x := 2\nx }\nreturn x }\nf()",
        "match 5 { n => { m := n * 2\nm } }",
        "match 5 { 1 => 10, n => { m := n * 2\nm } }",
        "x := .some(5)\nif let .some(n) = x { m := n + 1\nm } else { 0 }",
        "x := .none\nif let .some(n) = x { m := n + 1\nm } else { 0 }",
        "x := .some(5)\nif let .some(n) = x { m := n + 1\nm }",
        "struct Point { x: int }\nfunc f(p: Point) -> int { return p.x }\nf(Point{ x: 7 })",
        "struct Point { x: int }\nfunc dist(p: Point) -> int { return p.x }\np := Point{ x: 9 }\np.dist()",
        "struct Point { x: int }\nstruct Holder { p: Point }\nfunc dist(p: Point) -> int { return p.x }\nh := Holder{ p: Point{ x: 9 } }\nh.p.dist()",
        "struct Point { x: int }\nfunc f(p: Point) -> int { p.x = 5\nreturn p.x }\nf(Point{ x: 1 })",
        "func outer() { x := 10\nreturn |x| x + 1 }\ng := outer()\ng(5)",
        "func outer() { x := 10\ny := 20\nreturn |x| x + y }\ng := outer()\ng(5)",
        "func outer() { x := 10\nreturn { y := x + 1\ny } }\ng := outer()\ng(5)",
        "func counter() { n := 0\nreturn |inc| { n = n + inc\nn } }\nc := counter()\nc(1)\nc(2)",
        "x := 0\nfor x in 0..3 { x }\nx",
        "x := 5\nfor i in 0..3 { x := i\nx }\nx",
        "sum := 0\nfor i in 0..3 { j := i * 2\nsum = sum + j }\nsum",
        // A type alias statement owns no value in either engine.
        "type A = int\nx := 41\nx",
        "type A = int\nx := 41\n{ type B = str\ny := 1\nx + y }",
    ] {
        assert_same(src);
    }
}

#[test]
fn vm_matches_tree_walker_on_basics() {
    for src in [
        "1 + 2 * 3",
        "(1 + 2) * 3",
        "10 / 3",
        "10 % 3",
        "-5 + 2",
        "1 + 2.5",
        "\"a\" + \"b\"",
        "1 < 2",
        "1 == 1",
        "true && false",
        "true || false",
        "!true",
        "x := 1 + 2\nx * 3",
        "a := 10\nb := 20\nc := a + b\nc",
        "x := 1\nx := x + 1\nx",
        "if true { 1 } else { 2 }",
        "if false { 1 } else { 2 }",
        "if true { 1 }",
        "if false { 1 }",
        "if 1 < 2 { \"yes\" } else { \"no\" }",
        "name := \"World\"\n\"Hello {name}\"",
        "\"sum: {1 + 2}\"",
        "func dbl(n: int) -> int { return n * 2 }\ndbl(21)",
        "func fib(n: int) -> int { return if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\nfib(5)",
        "func f() -> int { return 5 }\nf()",
        "func f() -> int { if true { return 5 }\nreturn 3 }\nf()",
        "func add(a: int, b: int) -> int { return a + b }\nadd(2, 3)",
        "x := 1\nx = 5\nx",
        "x := 1\nx = x + 1\nx",
        "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x",
        "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { return p.x + p.y }\ndist(Point{ x: 3, y: 4 })",
        "v := [1, 2, 3]\nv[1]",
        "s := \"hello\"\ns[1:3]",
        "m := {1: 2}\nm[1]",
        "x := .some(1)\nmatch x { .some(n) => n, .none => 0 }",
        "x := .none\nmatch x { .some(n) => n, .none => 0 }",
        "f := |x| x * 2\nf(5)",
        "sum := 0\nfor i in 0..5 { sum = sum + i }\nsum",
        "total := 0\nfor n in [10, 20, 30] { total = total + n }\ntotal",
        "found := 0\nfor i in 0..10 { if i == 3 { found = i; break } }\nfound",
        "count := 0\nfor i in 0..5 { if i == 2 { continue }; count = count + 1 }\ncount",
        "x := 1\nif x > 0 { x = 5 }\nx",
        "a := 1\nb := 2\nc := a + b\nc",
        "func even(n: int) -> bool { return if n == 0 { true } else { odd(n - 1) } }\nfunc odd(n: int) -> bool { return if n == 0 { false } else { even(n - 1) } }\neven(4)",
        "func apply(f, x) { return f(x) }\napply(|n| n + 1, 41)",
        "func outer() { func inner(n: int) -> int { return n * 3 }\nreturn inner }\ng := outer()\ng(7)",
        "x := 1\n{ y := 2\nx + y }",
        "func f() -> int { { return 7 }\nreturn 0 }\nf()",
        "func f() -> int { x := .none\nx? }\nf()",
        "func f() -> result<int, str> { x := .none\nx? }\nf()",
        "func f() -> result<int, str> { x := .ok(5)\nx? }\nf()",
        "sum := 0\nfor i in 0..5 { sum = sum + i }\nsum",
        "total := 0\nfor n in [10, 20, 30] { total = total + n }\ntotal",
        "found := 0\nfor i in 0..10 { if i == 3 { found = i; break } }\nfound",
        "count := 0\nfor i in 0..5 { if i == 2 { continue }; count = count + 1 }\ncount",
        "for i in 0..3 { i }",
        "for i in 0..0 { i }",
        "for i in [] { i }",
        "sum := 0\nfor i in 0..5 { if i == 2 { continue }\nsum = sum + i }\nsum",
        "sum := 0\nfor i in 0..5 { if i == 2 { break }\nsum = sum + i }\nsum",
        "out := 0\nfor i in 0..3 { for j in 0..3 { out = out + 1 } }\nout",
        "out := 0\nfor i in 0..3 { for j in 0..3 { if j == 1 { break }; out = out + 1 } }\nout",
        "out := 0\nfor i in 0..3 { for j in 0..3 { if j == 1 { continue }; out = out + 1 } }\nout",
        "func f() -> int { for i in 0..3 { if i == 1 { return 42 } }\nreturn 0 }\nf()",
        "x := 0\nwhile x < 3 { x = x + 1 }\nx",
        "x := 0\nwhile true { x = x + 1\nif x == 3 { break } }\nx",
        "x := 0\nwhile x < 3 { x = x + 1\nif x == 2 { continue } }\nx",
        "x := 0\nwhile x < 3 { if x == 1 { break }\nx = x + 1 }\nx",
        "func f() -> int { while true { return 7 } }\nf()",
        "[1, 2, 3][1]",
        "[[1, 2], [3, 4]][1][0]",
        "[1, 2, 3][-1]",
        "{\"a\": 1, \"b\": 2}[\"b\"]",
        "{1: \"one\", 2: \"two\"}[2]",
        "m := {\"k\": 1}\nm[\"k\"] = 5\nm[\"k\"]",
        "m := {}\nm[\"new\"] = 42\nm[\"new\"]",
        "a := [1, 2, 3]\na[0] = 9\na[0]",
        "a := [1, 2, 3]\na[1] = a[1] * 10\na[1]",
        "[1, 2, 3, 4][1:3]",
        "[1, 2, 3, 4][:2]",
        "[1, 2, 3, 4][2:]",
        "[1, 2, 3, 4][:]",
        "[1, 2, 3, 4][-3:-1]",
        "\"hello\"[1:3]",
        "\"hello\"[:2]",
        "\"hello\"[2:]",
        "\"hello\"[-3:]",
        "\"abc\"[0]",
        "1..5",
        "a := 2\nb := 5\na..b",
        "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x",
        "struct Point { x: int, y: int }\nPoint{ x: 1, y: 2 }.y",
        "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x = 10\np.x",
        "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x = p.x + 1\np.x",
        "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x",
        "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nr := Rect{ p: Point{ x: 1, y: 2 }, w: 3 }\nr.p.x = 9\nr.p.x",
        "struct Bag { items: [int] }\nb := Bag{ items: [1, 2, 3] }\nb.items[1]",
        "struct Bag { items: [int] }\nb := Bag{ items: [1, 2, 3] }\nb.items[1] = 9\nb.items[1]",
        "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { return p.x + p.y }\ndist(Point{ x: 3, y: 4 })",
        "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\nfunc sum(p: Point) -> int { return p.x + p.y }\nsum(p)",
        "struct Point { x: int, y: int }\nfunc mk() -> Point { return Point{ x: 7, y: 8 } }\nmk().x",
        "f := |x| x * 2\nf(5)",
        "f := |x, y| x + y\nf(2, 3)",
        "n := 10\nf := |x| x + n\nf(5)",
        "f := |x| |y| x + y\nf(2)(3)",
        "f := |x| { y := x * 2\ny }\nf(5)",
        "f := |n| if n < 2 { n } else { f(n - 1) + f(n - 2) }\nf(5)",
        "func apply(f, x) { return f(x) }\napply(|n| n + 1, 41)",
        "func outer() { func inner(n: int) -> int { return n * 3 }\nreturn inner }\ng := outer()\ng(7)",
        ".ok(5)",
        ".err(\"boom\")",
        ".some(1)",
        ".none",
        "x := .ok(5)\nmatch x { .ok(n) => n, .err(e) => e }",
        "x := .err(\"boom\")\nmatch x { .ok(n) => n, .err(e) => e }",
        "x := .some(1)\nmatch x { .some(n) => n, .none => 0 }",
        "x := .none\nmatch x { .some(n) => n, .none => 0 }",
        "match 5 { 5 => \"five\", _ => \"other\" }",
        "match 3 { 5 => \"five\", _ => \"other\" }",
        "match 5 { n => n * 2 }",
        "match 5 { 1 => 10, 2 => 20, n => n }",
        "match true { true => 1, false => 0 }",
        "match 1.5 { 1.5 => \"one five\", _ => \"other\" }",
        "match .some(1) { .some(n) => n + 1, .none => 0 }",
        "match .some(.ok(2)) { .some(.ok(n)) => n, _ => 0 }",
        "match .none { .some(n) => n, .none => 0 }",
        "match .err(9) { .ok(n) => n, .err(e) => e }",
        "match 5 { n => { m := n * 2\nm } }",
        "match 5 { 1 => 10, _ => 20 }",
        "match 5 { 5 => 1, 5 => 2, _ => 3 }",
        "x := .some(5)\nif let .some(n) = x { n } else { 0 }",
        "x := .none\nif let .some(n) = x { n } else { 0 }",
        "x := .ok(5)\nif let .ok(n) = x { n } else { 0 }",
        "x := .err(7)\nif let .ok(n) = x { n } else { 0 }",
        "x := .some(5)\nif let .some(n) = x { n }",
        "x := .none\nif let .some(n) = x { n }",
        "x := 42\nif let n = x { n } else { 0 }",
        "func f() -> result<int, str> { x := .ok(5)\nx? }\nf()",
        "func f() -> result<int, str> { x := .err(\"no\")\nx? }\nf()",
        "func f() -> option<int> { x := .some(3)\nx? }\nf()",
        "func f() -> option<int> { x := .none\nx? }\nf()",
        "func f() -> result<int, str> { x := .ok(5)\ny := .ok(6)\nx? + y? }\nf()",
        // String comparison (Eq/Ne/Lt/Gt/Le/Ge)
        "\"hello\" == \"hello\"",
        "\"hello\" != \"world\"",
        "\"abc\" < \"def\"",
        "\"abc\" > \"aaa\"",
        "\"abc\" <= \"abc\"",
        "\"abc\" >= \"abc\"",
    ] {
        assert_same(src);
    }
}

#[test]
fn vm_interpolation_unwraps_option_parity() {
    // VM and tree-walker must render interpolated Options identically:
    // `.some(v)` unwraps to `v`, `.none` renders as `none`, while `:?`
    // preserves the debug wrappers.
    for src in [
        "x := .some(42)\n\"{x}\"",
        "x := .some(\"hi\")\n\"{x}\"",
        "x := .none\n\"{x}\"",
        "x := .some(.some(7))\n\"{x}\"",
        "x := .some(42)\n\"val={x}!\"",
        "x := .some(255)\n\"{x:x}\"",
        "x := .some(3.14)\n\"{x:.2f}\"",
        "x := .some(42)\n\"{x:?}\"",
        "x := .none\n\"{x:?}\"",
        "x := .some(42)\n\"{x:debug}\"",
    ] {
        assert_same(src);
    }
    // Expected display values (not just agreement).
    assert_eq!(
        run_src("x := .some(42)\n\"{x}\"").unwrap(),
        Value::Str("42".to_string().into())
    );
    assert_eq!(
        run_src("x := .none\n\"{x}\"").unwrap(),
        Value::Str("none".to_string().into())
    );
    assert_eq!(
        run_src("x := .some(.some(7))\n\"{x}\"").unwrap(),
        Value::Str("7".to_string().into())
    );
    assert_eq!(
        run_src("x := .some(42)\n\"{x:?}\"").unwrap(),
        Value::Str(".some(42)".to_string().into())
    );
    assert_eq!(
        run_src("x := .none\n\"{x:?}\"").unwrap(),
        Value::Str(".none".to_string().into())
    );
}

#[test]
fn vm_multiline_pipe() {
    for src in [
        "func inc(n: int) -> int { return n + 1 }\nfunc dbl(n: int) -> int { return n * 2 }\n5\n  |> inc\n  |> dbl",
        "func inc(n: int) -> int { return n + 1 }\nfunc dbl(n: int) -> int { return n * 2 }\nval := 10\nval\n  |> inc\n  |> dbl\n  |> inc",
        "func inc(n: int) -> int { return n + 1 }\nresult := {\n  5\n    |> inc\n    |> inc\n}\nresult",
    ] {
        assert_same(src);
    }
}

#[test]
fn vm_deep_recursion_no_rust_stack_overflow() {
    assert_eq!(
        run_src(
            "func fib(n: int) -> int { return if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\nfib(20)"
        )
        .unwrap(),
        Value::Int(6765)
    );
    assert_eq!(
        run_src(
            "func count(n: int) -> int { return if n == 0 { 0 } else { count(n - 1) + 1 } }\ncount(100000)"
        )
        .unwrap(),
        Value::Int(100000)
    );
}

#[test]
fn vm_if_condition_must_be_bool() {
    let err = run_src("if 1 { 2 }").unwrap_err();
    assert_eq!(err.message, "`if` condition must be a bool");
}

#[test]
fn vm_undefined_variable_errors() {
    let err = run_src("nope + 1").unwrap_err();
    assert_eq!(err.message, "undefined variable `nope`");
}

#[test]
fn vm_division_by_zero_errors() {
    let err = run_src("1 / 0").unwrap_err();
    assert_eq!(err.message, "division by zero");
}

#[test]
fn vm_return_outside_function_errors() {
    let err = run_src("return 5").unwrap_err();
    assert_eq!(err.message, "`return` outside of a function");
}

#[test]
fn vm_break_outside_loop_errors() {
    let err = run_src("break").unwrap_err();
    assert_eq!(err.message, "`break` outside of a loop");
}

#[test]
fn vm_short_circuit_skips_side_effects() {
    assert_eq!(run_src("false && nope()").unwrap(), Value::Bool(false));
    assert_eq!(run_src("true || nope()").unwrap(), Value::Bool(true));
    assert_eq!(
        run_src("true && nope()").unwrap_err().message,
        "undefined variable `nope`"
    );
    assert_eq!(
        run_src("false || nope()").unwrap_err().message,
        "undefined variable `nope`"
    );
    assert_eq!(run_src("true && 1 < 2").unwrap(), Value::Bool(true));
    assert_eq!(run_src("false || 1 < 2").unwrap(), Value::Bool(true));
}

#[test]
fn vm_method_call_and_cross_module() {
    assert_same(
        "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { return p.x + p.y }\np := Point{ x: 3, y: 4 }\np.dist()",
    );
    let parsed = parse("p := shapes.Point{ x: 3, y: 4 }\np.dist()");
    let mut interp = Interp::new();
    Arc::make_mut(&mut interp.structs).insert("shapes.Point".into(), vec!["x".into(), "y".into()]);
    let body = parse("p.x + p.y");
    let mut chunk = Compiler::compile_program(&body.program);
    chunk.params = vec![Param {
        name: Ident {
            name: "p".into(),
            span: Span::new(0, 0),
        },
        ty: None,
        default: None,
        span: Span::new(0, 0),
    }];
    let fv = FuncValue {
        params: chunk.params.clone(),
        body: Expr::Block(Block {
            stmts: Vec::new(),
            span: Span::new(0, 0),
        }),
        env: interp.env.clone(),
        chunk: Some(Arc::new(chunk)),
        chunk_defaults: Vec::new(),
        // Named function from a chunk: explicit `return` only.
        tail_value: false,
    };
    interp
        .funcs
        .insert("shapes.dist".into(), std::rc::Rc::new(fv));
    let v = interp.run(&parsed.program).unwrap();
    assert_eq!(v, Value::Int(7));
}

#[test]
fn vm_compiles_expected_opcodes() {
    let parsed = parse("1 + 2 * 3");
    let chunk = Compiler::compile_program(&parsed.program);
    assert!(matches!(
        chunk.code.as_slice(),
        [
            Op::PushConst(_),
            Op::PushConst(_),
            Op::PushConst(_),
            Op::BinOp(BinOp::Mul, _),
            Op::BinOp(BinOp::Add, _),
        ]
    ));
}

#[test]
#[ignore]
fn bench_loop_vm_vs_tree() {
    let src = "sum := 0\nfor i in 0..100000 { sum = sum + i }\nsum";
    let parsed = parse(src);
    let start = std::time::Instant::now();
    let mut interp = Interp::new();
    let v = interp.run(&parsed.program).unwrap();
    let vm_time = start.elapsed();
    let tree_time = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let start = std::time::Instant::now();
            let mut interp = Interp::new();
            let t = interp.run_tree_walker(&parsed.program).unwrap();
            (t.to_string(), start.elapsed())
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(v.to_string(), tree_time.0);
    println!("loop(100k) VM: {vm_time:?}  tree-walker: {:?}", tree_time.1);
}

#[test]
#[ignore]
fn bench_fib_vm_vs_tree() {
    let src =
        "func fib(n: int) -> int { return if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\nfib(20)";
    let parsed = parse(src);
    let start = std::time::Instant::now();
    let mut interp = Interp::new();
    let v = interp.run(&parsed.program).unwrap();
    let vm_time = start.elapsed();
    let tree_time = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let start = std::time::Instant::now();
            let mut interp = Interp::new();
            let t = interp.run_tree_walker(&parsed.program).unwrap();
            (t.to_string(), start.elapsed())
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(v.to_string(), tree_time.0);
    println!("fib(20) VM: {vm_time:?}  tree-walker: {:?}", tree_time.1);
}

#[test]
fn vm_bitwise_matches_tree_walker() {
    // Differential: VM and tree-walker must agree on values and errors.
    for src in [
        "6 & 3",
        "6 | 3",
        "6 ^ 3",
        "~6",
        "~0",
        "1 << 10",
        "1024 >> 3",
        "-8 >> 2",
        "1 << 63",
        "1 << 64",
        "1 | 2 ^ 3 & 5",
        "1 + 2 << 3",
        "8 >> 1 + 1",
        "15 & 7 == 7",
        "1 << -1",
        "1 >> -5",
        "func rotl(x: int, k: int) -> int { return (x << k) | (x >> (64 - k)) }\nrotl(305419896, 4)",
        "s := 123456789\ni := 0\nwhile i < 10 { s = (s << 13) ^ s\ns = s ^ (s >> 17)\ns = s ^ (s << 5)\ni = i + 1 }\ns",
    ] {
        assert_same(src);
    }
    // Spot-check absolute values through the VM.
    assert_eq!(run_src("6 & 3").unwrap(), Value::Int(2));
    assert_eq!(run_src("1 << 63").unwrap(), Value::Int(i64::MIN));
    assert_eq!(run_src("~6").unwrap(), Value::Int(-7));
}

/// Compile `src` with `vec.push` registered as a native (like the real
/// `run_typed` pipeline does via `Interp::natives`).
fn compile_with_push_native(src: &str) -> super::Chunk {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let natives: Arc<std::collections::HashSet<String>> =
        Arc::new(["vec.push".to_string()].into_iter().collect());
    Compiler::compile_program_with_natives(&parsed.program, natives)
}

fn has_op(chunk: &super::Chunk, pred: impl FnMut(&Op) -> bool) -> bool {
    chunk.code.iter().any(pred)
}

/// Regression: the free form `b = vec.push(b, e)` (2 args) must fuse to
/// [`Op::VecPush`]. An earlier revision routed every callee ending in
/// `push` through the method-take handler, which rejected the free form
/// (`rx_key "vec" != target`) and silently disabled fusion.
#[test]
fn vm_fused_free_push_emits_vec_push_not_method() {
    let chunk = compile_with_push_native("b := []\nb = vec.push(b, 1)\n");
    assert!(
        has_op(&chunk, |op| matches!(op, Op::VecPush { .. })),
        "expected fused VecPush, got {:?}",
        chunk.code
    );
    assert!(
        !has_op(&chunk, |op| matches!(op, Op::VecPushMethod { .. })),
        "free push must not take the method path: {:?}",
        chunk.code
    );
}

/// The method form `b = b.push(e)` (1 arg) fuses through the
/// runtime-checked [`Op::VecPushMethod`].
#[test]
fn vm_fused_method_push_emits_vec_push_method() {
    let chunk = compile_with_push_native("b := []\nb = b.push(1)\n");
    assert!(
        has_op(&chunk, |op| matches!(op, Op::VecPushMethod { .. })),
        "expected fused VecPushMethod, got {:?}",
        chunk.code
    );
}

/// The field form `s.f = vec.push(s.f, e)` fuses to [`Op::VecPushField`].
#[test]
fn vm_fused_field_push_emits_vec_push_field() {
    let chunk =
        compile_with_push_native("struct W { f: [int] }\ns := W{f: []}\ns.f = vec.push(s.f, 1)\n");
    assert!(
        has_op(&chunk, |op| matches!(op, Op::VecPushField { .. })),
        "expected fused VecPushField, got {:?}",
        chunk.code
    );
}

/// `a[i] = v` takes the container out of its home so `set_index` sees
/// refcount 1 and mutates in place ([`Op::StoreIndexTake`]). Without it the
/// value shared with the slot forces `Arc::make_mut` to deep-copy the
/// array/dict on every element write — an O(n²) index fill.
#[test]
fn vm_index_store_emits_take() {
    let chunk = compile_with_push_native("a := [0, 0]\na[1] = 5\na[1]\n");
    assert!(
        has_op(&chunk, |op| matches!(op, Op::StoreIndexTake { .. })),
        "expected StoreIndexTake, got {:?}",
        chunk.code
    );
    let chunk = compile_with_push_native("d := {\"k\": 0}\nd[\"k\"] = 7\nd[\"k\"]\n");
    assert!(
        has_op(&chunk, |op| matches!(op, Op::StoreIndexTake { .. })),
        "expected StoreIndexTake for a dict store, got {:?}",
        chunk.code
    );
}

/// `a[i] += v` takes the home too ([`Op::CompoundIndexTake`]) — the
/// counting loop is the sharpest case: without the take the read-modify-write
/// pair deep-copies the whole container per iteration.
#[test]
fn vm_compound_index_store_emits_take() {
    let chunk = compile_with_push_native("a := [0, 0]\na[1] += 3\na[1]\n");
    assert!(
        has_op(&chunk, |op| matches!(op, Op::CompoundIndexTake { .. })),
        "expected CompoundIndexTake, got {:?}",
        chunk.code
    );
    let chunk = compile_with_push_native("d := {\"k\": 1}\nd[\"k\"] += 2\nd[\"k\"]\n");
    assert!(
        has_op(&chunk, |op| matches!(op, Op::CompoundIndexTake { .. })),
        "expected CompoundIndexTake for a dict update, got {:?}",
        chunk.code
    );
}

/// A compound store whose rhs reads the base keeps the generic path (the
/// take would apply the update to the aliased container).
#[test]
fn vm_compound_index_store_keeps_generic_when_rhs_reads_base() {
    let chunk = compile_with_push_native("a := [1, 2]\na[0] += a[1]\na[0]\n");
    assert!(
        has_op(&chunk, |op| matches!(op, Op::CompoundIndexOp { .. })),
        "self-reading compound store must stay generic, got {:?}",
        chunk.code
    );
    assert!(
        !has_op(&chunk, |op| matches!(op, Op::CompoundIndexTake { .. })),
        "self-reading compound store must not take the home, got {:?}",
        chunk.code
    );
}

/// Compound stores keep tree-walker semantics: repeated updates, dict
/// counters, string accumulation, rhs from a variable, and the OOB error.
#[test]
fn vm_compound_index_store_take_matches_tree_walker() {
    for src in [
        "a := [1, 2]\nfor i in 0..2 { a[i] += 10 }\na[0] + a[1]",
        "d := {\"a\": 1}\nfor i in 0..3 { d[\"a\"] += i }\nd[\"a\"]",
        "a := [0, 0]\nk := 4\na[1] += k\na[1]",
        "parts := [\"x\"]\nparts[0] += \"y\"\nparts[0]",
        "a := [1]\na[2] += 1",
        "a := [1, 2]\na[0] += a[1] * 2\na[0]",
        // copies stay independent (detach on write)
        "a := [1, 2]\nb := a\na[0] += 5\nb[0] * 10 + a[0]",
        "d := {\"k\": 1}\ne := d\nd[\"k\"] += 2\ne[\"k\"] * 10 + d[\"k\"]",
    ] {
        assert_same(src);
    }
}

/// Loop fills (`for i in 0..n { a[i] = i }`) take the home too: the loop
/// header must not turn the container into an env/captured binding that
/// defeats the take (that would silently restore the O(n²) copy).
#[test]
fn vm_index_store_in_loop_emits_take() {
    let chunk = compile_with_push_native(
        "func fill(n: int) -> [int] {\n    a := [0, 0, 0, 0]\n    for i in 0..n {\n        a[i] = i\n    }\n    a\n}\nfill(2)\n",
    );
    let body = chunk
        .code
        .iter()
        .find_map(|op| match op {
            Op::MakeFunc { name, chunk, .. } if name == "fill" => Some(chunk.code.clone()),
            _ => None,
        })
        .expect("function fill chunk");
    assert!(
        body.iter()
            .any(|op| matches!(op, Op::StoreIndexTake { .. })),
        "expected StoreIndexTake in fill, got {body:?}"
    );
    let chunk = compile_with_push_native(
        "func fill(n: int) -> int {\n    d: {int: int} = {}\n    for i in 0..n {\n        d[i] = i * 3\n    }\n    d[n - 1]\n}\nfill(2)\n",
    );
    let body = chunk
        .code
        .iter()
        .find_map(|op| match op {
            Op::MakeFunc { name, chunk, .. } if name == "fill" => Some(chunk.code.clone()),
            _ => None,
        })
        .expect("function fill chunk");
    assert!(
        body.iter()
            .any(|op| matches!(op, Op::StoreIndexTake { .. })),
        "expected StoreIndexTake in the dict fill, got {body:?}"
    );
}

/// Stores whose index/value read the base keep the generic copy path: the
/// take would hand the write the aliased container, and a self-store
/// (`a[0] = a`) must store a copy, exactly like the tree-walker.
#[test]
fn vm_index_store_keeps_generic_when_operands_read_base() {
    let chunk = compile_with_push_native("a := [1, 2]\na[0] = a[1] + 1\na[0]\n");
    assert!(
        has_op(&chunk, |op| matches!(op, Op::StoreIndexOp(_))),
        "self-reading store must stay generic, got {:?}",
        chunk.code
    );
    assert!(
        !has_op(&chunk, |op| matches!(op, Op::StoreIndexTake { .. })),
        "self-reading store must not take the home, got {:?}",
        chunk.code
    );
}

/// Taking the home must be invisible: value semantics, in-place updates,
/// index stores from variables, loop fills, dict growth, nested stores and
/// the out-of-bounds error all match the tree-walker.
#[test]
fn vm_index_store_take_matches_tree_walker() {
    for src in [
        // copies stay independent (detach on write)
        "a := [1, 2]\nb := a\na[0] = 9\nb[0] * 10 + a[0]",
        "d := {\"k\": 1}\ne := d\nd[\"k\"] = 2\ne[\"k\"] * 10 + d[\"k\"]",
        // repeated in-place writes with a variable index
        "a := [0, 0, 0]\ni := 1\na[i] = 7\na[0] + a[1] + a[2]",
        // self-reading store keeps the generic copy path
        "a := [1, 2]\na[0] = a[1] + 3\na[0]",
        "a := [1, 2]\na[0] = a\na[0][1]",
        // out-of-bounds still errors identically
        "a := [1]\na[3] = 1",
        // loop fill by index
        "a := [0, 0, 0, 0]\nfor i in 0..4 { a[i] = i * i }\na[3]",
        // dict insert by index store
        "d := {1: 1}\nd[2] = 2\nd[3] = 3\nd[1] + d[2] + d[3]",
        // nested element stores
        "m := [[0], [0]]\nm[1][0] = 5\nm[1][0]",
    ] {
        assert_same(src);
    }
}

/// `b = f(b, x)` inside a function body takes the single `b` load
/// ([`Op::TakeSlot`]) instead of cloning it into the call.
#[test]
fn vm_thread_call_emits_take_slot() {
    let chunk = compile_with_push_native(
        "func f(x: [int], y: int) -> [int] { return x }\nfunc g() -> [int] {\nb := [1]\nb = f(b, 2)\nreturn b\n}\n",
    );
    let body_code = chunk
        .code
        .iter()
        .find_map(|op| match op {
            Op::MakeFunc { name, chunk, .. } if name == "g" => Some(chunk.code.clone()),
            _ => None,
        })
        .expect("function g chunk");
    assert!(
        body_code.iter().any(|op| matches!(op, Op::TakeSlot(_))),
        "expected TakeSlot in g, got {:?}",
        body_code
    );
}

/// Regression: tail `return x` / bare `x` moves the frame local out
/// (NRVO-equivalent) instead of deep-cloning. Values must match the
/// tree-walker, and outer bindings read through a tail closure must
/// survive the call (only params take).
#[test]
fn vm_tail_take_moves_param_and_spares_outer() {
    for src in [
        "func f(x: [int]) -> [int] { return x }\nf([1, 2])[1]",
        "func f(x: [int]) -> [int] { return x }\nf([1, 2])[0]",
        "func f(n: int) -> int { m := n * 2\nreturn m }\nf(20)",
        "struct W { f: [int] }\nfunc g(w: W) -> W { return w }\ng(W{f: [7]}).f[0]",
        "x := 10\nf := |u: int| x + u\nf(0) + x",
        "x := [1, 2]\nf := |u: int| x[u]\nf(1) + x[0]",
        "func f(x: int) -> int { g := |u: int| x + u\nreturn g(0) + x }\nf(3)",
        "func f(x: int) -> int { g := |u: int| x + u\nreturn g(1) + g(2) + x }\nf(3)",
        "f := |u: int| u + 1\nf(1) + f(2)",
        "func f(n: int) -> int { defer println(\"dd\")\nreturn n }\nf(41)",
        "outer := 99\nfunc f() -> int { return outer }\nf() + outer",
    ] {
        assert_same(src);
    }
    assert_eq!(
        run_src("func f(x: [int]) -> [int] { return x }\nf([1, 2, 3])[2]").unwrap(),
        Value::Int(3)
    );
    assert_eq!(
        run_src("x := 10\nf := |u: int| x + u\nf(0) + x").unwrap(),
        Value::Int(20)
    );
}

#[allow(clippy::ptr_arg)]
fn len_fake(_interp: &mut Interp, args: &mut Vec<Value>, span: Span) -> Result<Value, EvalError> {
    match args.first() {
        Some(Value::Array(vs)) => Ok(Value::Int(vs.len() as i64)),
        other => Err(EvalError::new(
            format!("`len` expects an array, found `{other:?}`"),
            span,
        )),
    }
}

/// Bare-native stand-in with no VM specialisation, so the fused-dispatch
/// test can prove the registry entry itself ran.
#[allow(clippy::ptr_arg)]
fn tally_fake(_interp: &mut Interp, args: &mut Vec<Value>, span: Span) -> Result<Value, EvalError> {
    match args.first() {
        Some(Value::Int(n)) => Ok(Value::Int(n * 2 + 1)),
        other => Err(EvalError::new(
            format!("`tally` expects an int, found `{other:?}`"),
            span,
        )),
    }
}

/// Minimal `vec.push` stand-in (clone + push, like the real native).
/// The bare test `Interp` ships no natives; registering the real
/// `zz_stdlib` table would pull a dependency cycle into unit tests.
#[allow(clippy::ptr_arg)]
fn vec_push_fake(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let mut vs = match args.first() {
        Some(Value::Array(vs)) => (**vs).clone(),
        other => {
            return Err(EvalError::new(
                format!("`vec.push` expects an array, found `{other:?}`"),
                span,
            ));
        }
    };
    let x = args
        .get(1)
        .cloned()
        .ok_or_else(|| EvalError::new("missing argument `x` for vec.push".to_string(), span))?;
    vs.push(x);
    Ok(Value::Array(std::sync::Arc::new(vs)))
}

/// Run `src` on the VM with `vec.push` registered (like `run_typed`).
fn run_vm_native(src: &str) -> Result<Value, EvalError> {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let mut natives = std::collections::HashMap::new();
    natives.insert(
        "vec.push".to_string(),
        crate::runtime::NativeEntry {
            arity: 2,
            f: vec_push_fake,
        },
    );
    natives.insert(
        "len".to_string(),
        crate::runtime::NativeEntry {
            arity: 1,
            f: len_fake,
        },
    );
    natives.insert(
        "tally".to_string(),
        crate::runtime::NativeEntry {
            arity: 1,
            f: tally_fake,
        },
    );
    let mut interp = Interp::with_natives(natives);
    let names: Arc<std::collections::HashSet<String>> =
        Arc::new(interp.natives.keys().cloned().collect());
    let chunk = Arc::new(Compiler::compile_program_with_natives(
        &parsed.program,
        names,
    ));
    let mut vm = Vm::new();
    match vm.run_chunk(&chunk, &mut interp) {
        Ok(crate::runtime::Flow::Value(v)) => Ok(v),
        Ok(_) => Err(EvalError::new("unexpected flow", Span::new(0, 0))),
        Err(e) => Err(e),
    }
}

/// Struct copy plus user-defined `push` method: the fused method op must
/// take the generic (synchronous) call path with write-back, agreeing
/// with the tree-walker. Guards the `VecPushMethod` fallback frame
/// discipline (a prior revision awaited an async frame push on the
/// stack and panicked out of bounds).
#[test]
fn vm_method_fallback_struct_copy_matches_tree_walker() {
    // NOTE: `run_tree` can't serve here — the bare test `Interp` has no
    // loader prelude, so the tree-walker's path eval for `vec.push`
    // fails; `run_vm_native` wires the real native table like `run_typed`.
    // VM/tree agreement for this shape is covered by the
    // `move_append_*` e2e + parity fixtures (full loader pipeline).
    let src = "struct W { f: [int] }\nimpl W {\n func push(w: W, x: int) -> W {\n return W{f: vec.push(w.f, x)}\n }\n}\nfunc go(s: W) -> int {\nt := s\nt = t.push(2)\nreturn len(t.f)\n}\ngo(W{f: [1]})\n";
    assert_eq!(run_vm_native(src).unwrap(), Value::Int(2));
    let src2 = "struct W { f: [int] }\nimpl W {\n func push(w: W, x: int) -> W {\n return W{f: vec.push(w.f, x)}\n }\n}\nt := W{f: [1]}\nt = t.push(2)\nlen(t.f)\n";
    assert_eq!(run_vm_native(src2).unwrap(), Value::Int(2));
}

/// `Op::CallMethod` probes fields with the non-erroring lookup and resolves
/// the callee from the shared function table, so the observable semantics —
/// field-before-method order, receiver passing, chained calls — must match
/// the tree-walker.
#[test]
fn vm_method_call_matches_tree_walker() {
    for src in [
        // plain receiver method
        "struct C { n: int }\nimpl C {\n func add(c: C, x: int) -> C {\n C{ n: c.n + x }\n }\n}\nc := C{ n: 0 }\nc = c.add(2)\nc.n",
        // chained calls through the returned struct
        "struct C { n: int }\nimpl C {\n func add(c: C, x: int) -> C {\n C{ n: c.n + x }\n }\n func get(c: C) -> int {\n c.n\n }\n}\nc := C{ n: 1 }\nc.add(2).add(3).get()",
        // a miss in the field table must not change the result
        "struct C { n: int }\nimpl C {\n func get(c: C) -> int {\n c.n * 2\n }\n}\nc := C{ n: 3 }\nc.get() + c.get()",
    ] {
        assert_same(src);
    }
}

/// A struct receiver whose `impl` was compiled here and whose struct has no
/// field of that name resolves to the path-call form `Type.method(recv, ...)`,
/// which `CallPath` enters directly. Everything the compile-time proof cannot
/// establish must keep the dynamic op — a wrong proof would change which
/// function runs.
#[test]
fn struct_method_call_resolves_at_compile_time() {
    use std::collections::{HashMap, HashSet};
    use zz_checker::{StructSig, Type};

    fn structs(fields: Vec<(&str, Type)>) -> HashMap<String, StructSig> {
        let mut m = HashMap::new();
        m.insert(
            "C".to_string(),
            StructSig {
                generics: Vec::new(),
                fields: fields
                    .into_iter()
                    .map(|(n, t)| (n.to_string(), t))
                    .collect(),
            },
        );
        m
    }

    fn ops_for(src: &str, structs: HashMap<String, StructSig>) -> Vec<Op> {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let (_res, types) = zz_checker::check_program_typed(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        Compiler::compile_program_typed(
            &parsed.program,
            Arc::new(types),
            structs,
            HashMap::new(),
            Arc::new(HashSet::new()),
        )
        .code
    }

    // Calls live in the enclosing function's chunk, not the top level.
    fn resolved(ops: &[Op]) -> bool {
        ops.iter().any(|op| match op {
            Op::CallPath { joined, argc, .. } => joined == "C.add" && *argc == 2,
            Op::MakeFunc { chunk, .. } => resolved(&chunk.code),
            _ => false,
        })
    }

    let src = "struct C { n: int }\nimpl C {\n func add(c: C, x: int) -> C {\n C{ n: c.n + x }\n }\n}\nfunc go() -> int {\n c := C{ n: 0 }\n c = c.add(2)\n c.n\n}\ngo()";
    assert!(
        resolved(&ops_for(src, structs(vec![("n", Type::Int)]))),
        "plain struct receiver must compile to the resolved path call"
    );
    // A same-named field wins at run time: keep the dynamic op.
    assert!(!resolved(&ops_for(
        src,
        structs(vec![("n", Type::Int), ("add", Type::Int)])
    )));
    // Embedded-shaped field: promotion could shadow the method.
    assert!(!resolved(&ops_for(
        src,
        structs(vec![("n", Type::Int), ("add", Type::Named("add".into()))])
    )));
    // `impl` for another type: nothing proves `C.add` exists.
    let other_impl = src.replace("impl C", "impl D");
    assert!(!resolved(&ops_for(
        &other_impl,
        structs(vec![("n", Type::Int)])
    )));
    // Unknown struct (cross-module): no signature, no proof.
    assert!(!resolved(&ops_for(src, HashMap::new())));
}

#[test]
fn vm_tuple_ops_match_tree_walker() {
    for src in [
        "t := (10, \"twenty\", 30)\nt[0]",
        "t := (10, \"twenty\", 30)\nt[1]",
        "t := (10, \"twenty\", 30)\nt[-1]",
        // NOTE: `len` needs stdlib natives (absent in unit interps) —
        // covered by the e2e fixture on both engines instead.
        "t := (10, \"twenty\", 30)\nt[0] = 99\nt[0]",
        "t := (1, 2)\nt[7]",
        "a, b := (7, 9)\na + b",
        "_, b := (7, 9)\nb",
        "a, b, c := (1, 2, 3)\na + b + c",
    ] {
        assert_same(src);
    }
}

#[test]
fn vm_compound_assign_matches_tree_walker() {
    for src in [
        "x := 100\nx += 7\nx",
        "x := 100\nx -= 7\nx",
        "x := 100\nx *= 7\nx",
        "x := 100\nx /= 7\nx",
        "x := 100\nx %= 7\nx",
        "x := 2\nx **= 10\nx",
        "x := 100\nx &= 7\nx",
        "x := 100\nx |= 7\nx",
        "x := 100\nx ^= 7\nx",
        "x := 100\nx <<= 2\nx",
        "x := 100\nx >>= 2\nx",
        "s := \"a\"\ns += \"b\"\ns",
        "struct P { x: int }\np := P{ x: 10 }\np.x += 5\np.x",
        "a := [10, 20, 30]\na[1] *= 2\na[1]",
        "a := [10, 20, 30]\na[0] += 1\na[2] -= 1\na[0] * 100 + a[2]",
        "x := 0\nx += 1\nx += 1\nx += 1\nx",
        "struct Rng { s0: int }\nfunc change(r: Rng) -> int { r.s0 += 100\nreturn r.s0 }\nrng := Rng{ s0: 10 }\nchange(rng) * 1000 + rng.s0",
    ] {
        assert_same(src);
    }
}

/// Structural proof that `x OP= y` uses the same fused slot ops as
/// `x = x OP y`: compare opcode discriminants of both compilations.
#[test]
fn vm_compound_assign_fuses_like_plain_assign() {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    fn opcodes(src: &str) -> Vec<&'static str> {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let (_res, types) = zz_checker::check_program_typed(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        let chunk = super::Compiler::compile_program_typed(
            &parsed.program,
            Arc::new(types),
            HashMap::new(),
            HashMap::new(),
            Arc::new(HashSet::new()),
        );
        chunk
            .code
            .iter()
            .map(|op| match op {
                Op::PushConst(_) => "PushConst",
                Op::IntAdd(_) => "IntAdd",
                Op::IntSub(_) => "IntSub",
                Op::IntMul(_) => "IntMul",
                Op::IntDiv(_) => "IntDiv",
                Op::IntRem(_) => "IntRem",
                Op::SlotInc { .. } => "SlotInc",
                Op::SlotAddInt { .. } => "SlotAddInt",
                Op::SlotAddIntImm { .. } => "SlotAddIntImm",
                Op::SlotBinaryInt { .. } => "SlotBinaryInt",
                Op::SlotBinaryIntImm { .. } => "SlotBinaryIntImm",
                Op::BinOp(..) => "BinOp",
                Op::LoadSlot(_) => "LoadSlot",
                Op::StoreSlot(_) => "StoreSlot",
                Op::LoadVar(..) => "LoadVar",
                Op::StoreVar(..) => "StoreVar",
                Op::DefineVar(_) => "DefineVar",
                _ => "other",
            })
            .collect()
    }

    // Identical opcode streams (spans aside) for every operator.
    for (compound, plain) in [
        ("x := 0\nx += 1", "x := 0\nx = x + 1"),
        ("x := 0\nx += 7", "x := 0\nx = x + 7"),
        ("x := 0\nx -= 7", "x := 0\nx = x - 7"),
        ("x := 0\nx *= 7", "x := 0\nx = x * 7"),
        ("x := 0\nx &= 7", "x := 0\nx = x & 7"),
        ("x := 0\nx <<= 2", "x := 0\nx = x << 2"),
    ] {
        assert_eq!(
            opcodes(compound),
            opcodes(plain),
            "opcode mismatch: {compound} vs {plain}"
        );
    }
    // The fused fast paths actually fire.
    let inc = opcodes("x := 0\nx += 1");
    assert!(inc.contains(&"SlotInc"), "SlotInc missing: {inc:?}");
    let add_imm = opcodes("x := 0\nx += 7");
    assert!(
        add_imm.contains(&"SlotAddIntImm"),
        "SlotAddIntImm missing: {add_imm:?}"
    );
}

/// Typed int comparisons (#364) compile to the unboxed fast-path ops
/// instead of generic `BinOp` — mirroring the `IntAdd` precedent.
/// Mixed-type operands keep generic dispatch.
#[test]
fn vm_typed_int_comparisons_emit_fast_ops() {
    use std::collections::{HashMap, HashSet};

    fn opcodes(src: &str) -> Vec<&'static str> {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let (_res, types) = zz_checker::check_program_typed(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        let chunk = super::Compiler::compile_program_typed(
            &parsed.program,
            Arc::new(types),
            HashMap::new(),
            HashMap::new(),
            Arc::new(HashSet::new()),
        );
        chunk
            .code
            .iter()
            .map(|op| match op {
                Op::IntLt(_) => "IntLt",
                Op::IntGt(_) => "IntGt",
                Op::IntLe(_) => "IntLe",
                Op::IntGe(_) => "IntGe",
                Op::IntEq(_) => "IntEq",
                Op::IntNe(_) => "IntNe",
                Op::BinOp(..) => "BinOp",
                _ => "other",
            })
            .collect()
    }

    let ops = opcodes(
        "a: int = 3\n\
         b: int = 4\n\
         c1 := a < b\n\
         c2 := a > b\n\
         c3 := a <= b\n\
         c4 := a >= b\n\
         c5 := a == b\n\
         c6 := a != b",
    );
    for want in ["IntLt", "IntGt", "IntLe", "IntGe", "IntEq", "IntNe"] {
        assert!(ops.contains(&want), "{want} missing: {ops:?}");
    }
    assert!(
        !ops.contains(&"BinOp"),
        "int comparisons must not use generic BinOp: {ops:?}"
    );
    // Mixed int/float operands keep generic dispatch.
    let mixed = opcodes("a: int = 3\nb: float = 1.5\nc := a < b");
    assert!(
        mixed.contains(&"BinOp"),
        "mixed-type comparison must stay generic: {mixed:?}"
    );
    assert!(
        !mixed.iter().any(|o| *o != "BinOp" && *o != "other"),
        "no int fast-path for mixed types: {mixed:?}"
    );
}

/// Structural proof of zero-cost erasure: a generic struct program
/// compiles to the same opcode *shape* as its hand-monomorphized twin
/// (names differ, so only discriminants are compared).
#[test]
fn vm_generic_struct_erases_like_monomorphic() {
    use std::collections::{HashMap, HashSet};

    fn disc(op: &Op) -> &'static str {
        match op {
            Op::PushConst(_) => "PushConst",
            Op::MakeStruct { .. } => "MakeStruct",
            Op::RegisterStruct { .. } => "RegisterStruct",
            Op::GetField(..) => "GetField",
            Op::GetFieldIdx(..) => "GetFieldIdx",
            Op::SetField(..) => "SetField",
            Op::SetFieldIdx(..) => "SetFieldIdx",
            Op::LoadSlot(_) => "LoadSlot",
            Op::StoreSlot(_) => "StoreSlot",
            Op::LoadVar(..) => "LoadVar",
            Op::StoreVar(..) => "StoreVar",
            Op::DefineVar(_) => "DefineVar",
            Op::Call { .. } => "Call",
            Op::Return => "Return",
            Op::BinOp(..) => "BinOp",
            _ => "other",
        }
    }

    fn opcodes(src: &str) -> Vec<&'static str> {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let (_res, types) = zz_checker::check_program_typed(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        let chunk = super::Compiler::compile_program_typed(
            &parsed.program,
            Arc::new(types),
            HashMap::new(),
            HashMap::new(),
            Arc::new(HashSet::new()),
        );
        chunk.code.iter().map(disc).collect()
    }

    let generic = "struct Box<T> { v: T }\nfunc main() {\n b := Box{ v: 42 }\n return b.v\n}";
    let mono = "struct BoxInt { v: int }\nfunc main() {\n b := BoxInt{ v: 42 }\n return b.v\n}";
    assert_eq!(opcodes(generic), opcodes(mono));
}

#[test]
fn vm_default_args_match_tree_walker() {
    // Omitted trailing defaults must evaluate (checker contract) on both
    // engines — same program (compiler inline fill) and across programs
    // (runtime fill from `FuncValue` defaults, which the per-program
    // compiler never saw).
    for src in [
        "func g(a: int, b: int = 5) -> int { return a + b }\ng(1)",
        "func g(a: int, b: int = 5) -> int { return a + b }\ng(1, 2)",
        "func g(a: int, b: int = 5, c: int = 7) -> int { return a + b + c }\ng(1)",
        "func g(a: int, b: int = 5, c: int = 7) -> int { return a + b + c }\ng(1, 2)",
    ] {
        assert_same(src);
    }
    assert_eq!(
        run_src("func g(a: int, b: int = 5) -> int { return a + b }\ng(1)").unwrap(),
        Value::Int(6)
    );
    // Cross-program: `g` is defined in one program, called with an omitted
    // default in another (mirrors multi-file `import m` load order).
    let dep = parse("func g(a: int, b: int = 5) -> int { return a + b }");
    assert!(dep.errors.is_empty());
    let main = parse("g(1)");
    assert!(main.errors.is_empty());
    let mut interp = Interp::new();
    interp.run(&dep.program).unwrap();
    assert_eq!(interp.run(&main.program).unwrap(), Value::Int(6));
    let mut tree = Interp::new();
    tree.run_tree_walker(&dep.program).unwrap();
    assert_eq!(tree.run_tree_walker(&main.program).unwrap(), Value::Int(6));
}

#[test]
fn vm_guarded_match_arm_miss_reloads_scrutinee() {
    // Regression: on the guarded-match path a pattern miss popped the
    // scrutinee (restore:false) but jumped straight to the next
    // `MatchArm`, skipping that arm's `LoadVar` reload — the next arm
    // tested a stale stack slot instead of the scrutinee (a trailing
    // unit arm like `.none` could never match). Every `MatchArm.next`
    // must target the next arm's `LoadVar` (or the trailing
    // `MatchError` for the last arm).
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    let src = "func f(o: Option<int>) -> int {\n    limit := 10\n    return match o {\n        .some(n) if limit > 5 => n,\n        .some(n) => 0,\n        .none => -1,\n    }\n}\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let (_res, types) = zz_checker::check_program_typed(
        &parsed.program,
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );
    let chunk = super::Compiler::compile_program_typed(
        &parsed.program,
        Arc::new(types),
        HashMap::new(),
        HashMap::new(),
        Arc::new(HashSet::new()),
    );
    // Descend into function bodies: the match lives in `f`'s chunk.
    fn chunks_in(chunk: &super::Chunk) -> Vec<&super::Chunk> {
        let mut out = vec![chunk];
        for op in &chunk.code {
            if let Op::MakeFunc { chunk: inner, .. } = op {
                out.extend(chunks_in(inner));
            }
        }
        out
    }
    let mut arm_nexts = Vec::new();
    for chunk in chunks_in(&chunk) {
        // Locate MatchArm ops and the LoadVar positions per chunk
        // (jump targets are chunk-relative).
        let mut loads = std::collections::HashSet::new();
        for (i, op) in chunk.code.iter().enumerate() {
            if matches!(op, Op::LoadVar(..)) {
                loads.insert(i);
            }
        }
        for op in &chunk.code {
            if let Op::MatchArm { next, .. } = op {
                arm_nexts.push((*next, loads.contains(next)));
            }
        }
    }
    assert!(!arm_nexts.is_empty(), "expected guarded match arms");
    // Every non-terminal arm must jump to a LoadVar reload. The last
    // arm targets the trailing `MatchError`, which is not a LoadVar.
    for (next, is_load) in &arm_nexts[..arm_nexts.len().saturating_sub(1)] {
        assert!(
            is_load,
            "MatchArm miss jumps to {next}, which is not a LoadVar reload"
        );
    }
}

#[test]
fn vm_if_let_result_does_not_alias_later_slots() {
    // Regression: `if let` left the stack-height fiction one low (the
    // result value was not counted), so the next declaration reused its
    // slot — a later match read the if-let's value instead of its own
    // scrutinee (`.some(7)` tested as `.some(42)`). Needs the typed
    // pipeline (slot promotion); the untyped path keeps everything in
    // the environment and never collides.
    use std::collections::HashMap;
    use std::sync::Arc;

    let src = "func main() -> int {\n    b := .some(42)\n    x := if let .some(v) = b { v } else { 0 }\n    o := .some(7)\n    return match o {\n        .some(n) => n,\n        _ => 0,\n    }\n}\n";
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let (checked, span_types) = zz_checker::check_program_typed(
        &parsed.program,
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );
    assert!(
        checked
            .errors
            .iter()
            .all(|e| e.severity != zz_frontend::diag::Severity::Error),
        "check errors: {:?}",
        checked.errors
    );
    let mut interp = Interp::with_natives(HashMap::new());
    let v = interp
        .run_typed(
            &parsed.program,
            Arc::new(span_types),
            HashMap::new(),
            HashMap::new(),
        )
        .expect("run");
    // `main` returns its last value... via explicit call below.
    let _ = v;
    let f = interp.funcs.get("main").cloned().expect("main registered");
    let out = interp
        .call(Value::Func(f), Vec::new(), Span::new(0, 0))
        .expect("call main");
    assert_eq!(out, Value::Int(7));
}

/// `s.f = v` with pure `v` fuses to [`Op::SetFieldTake`] (#242).
#[test]
fn vm_fused_field_store_emits_set_field_take() {
    let chunk = compile_with_push_native(
        "struct W { f: [int], g: int }\ns := W{f: [], g: 0}\ns.f = [1, 2]\n",
    );
    assert!(
        has_op(&chunk, |op| matches!(op, Op::SetFieldTake { .. })),
        "expected fused SetFieldTake, got {:?}",
        chunk.code
    );
}

/// `s.f = s.f + v` fuses to [`Op::FieldBinopTake`] (#242).
#[test]
fn vm_fused_field_binop_emits_field_binop_take() {
    let chunk =
        compile_with_push_native("struct W { s: str }\ns := W{s: \"a\"}\ns.s = s.s + \"b\"\n");
    assert!(
        has_op(&chunk, |op| matches!(op, Op::FieldBinopTake { .. })),
        "expected fused FieldBinopTake, got {:?}",
        chunk.code
    );
}

/// `s.f[i] = v` fuses to [`Op::SetFieldIndexTake`] (#242).
#[test]
fn vm_fused_field_index_store_emits_set_field_index_take() {
    let chunk = compile_with_push_native("struct W { f: [int] }\ns := W{f: [0]}\ns.f[0] = 9\n");
    assert!(
        has_op(&chunk, |op| matches!(op, Op::SetFieldIndexTake { .. })),
        "expected fused SetFieldIndexTake, got {:?}",
        chunk.code
    );
}

/// `s.f[i]` reads fuse to [`Op::GetFieldIndex`] (#242).
#[test]
fn vm_fused_field_index_read_emits_get_field_index() {
    let chunk = compile_with_push_native("struct W { f: [int] }\ns := W{f: [7]}\nx := s.f[0]\n");
    assert!(
        has_op(&chunk, |op| matches!(op, Op::GetFieldIndex { .. })),
        "expected fused GetFieldIndex, got {:?}",
        chunk.code
    );
}

/// Stores mentioning the home fall back (no fusion, generic path).
#[test]
fn vm_field_store_mentioning_home_stays_generic() {
    let chunk =
        compile_with_push_native("struct W { f: [int], g: int }\ns := W{f: [], g: 0}\ns.f = s.g\n");
    assert!(
        !has_op(&chunk, |op| matches!(
            op,
            Op::SetFieldTake { .. } | Op::FieldBinopTake { .. } | Op::SetFieldIndexTake { .. }
        )),
        "mentioning store must not fuse, got {:?}",
        chunk.code
    );
}

/// Differential: Doc-shaped workloads agree between VM and tree-walker,
/// including stores, indexed stores, concat accumulation, and indexed
/// reads through struct fields (#242).
#[test]
fn vm_tree_agree_on_doc_shapes() {
    for src in [
        "struct D { k: str, a: [str], n: [int] }\nd := D{k: \"\", a: [], n: []}\nd.k = d.k + \"I\"\nd.a = vec.push(d.a, \"s\")\nd.n = vec.push(d.n, 1)\nd.k",
        "struct D { k: str, a: [str], n: [int] }\nd := D{k: \"\", a: [], n: []}\nd.k = d.k + \"I\"\nd.a = vec.push(d.a, \"s\")\nd.a[0]",
        "struct D { t: [int] }\nd := D{t: [5, 6]}\nd.t[0] = 9\nd.t[0] + d.t[1]",
        "struct D { t: [int] }\nd := D{t: [5]}\nx := d.t[0]\nx",
        "struct D { k: str }\nfunc cat(d: D, s: str) -> D {\n    d.k = d.k + s\n    return d\n}\nd := D{k: \"a\"}\ne := cat(d, \"b\")\nd.k + \"/\" + e.k",
    ] {
        assert_same(src);
    }
}

/// Error parity: fused stores/reads report the generic errors.
#[test]
fn vm_fused_field_error_parity() {
    // Index out of bounds through a fused read.
    let (vm, tree) = (
        run_src("struct W { f: [int] }\ns := W{f: [1]}\ns.f[5]"),
        run_tree("struct W { f: [int] }\ns := W{f: [1]}\ns.f[5]"),
    );
    match (&vm, &tree) {
        (Err(a), Err(b)) => assert_eq!(a.message, b.message, "OOB messages differ"),
        _ => panic!("both must error: {vm:?} vs {tree:?}"),
    }
    // Type error through a fused binop store.
    let (vm, tree) = (
        run_src("struct W { s: str }\ns := W{s: \"a\"}\ns.s = s.s + 1\ns.s"),
        run_tree("struct W { s: str }\ns := W{s: \"a\"}\ns.s = s.s + 1\ns.s"),
    );
    match (&vm, &tree) {
        (Err(a), Err(b)) => assert_eq!(a.message, b.message, "binop messages differ"),
        _ => panic!("both must error: {vm:?} vs {tree:?}"),
    }
    // Missing field through a fused store.
    let (vm, tree) = (
        run_src("struct W { f: int }\ns := W{f: 1}\ns.nope = 2\ns.f"),
        run_tree("struct W { f: int }\ns := W{f: 1}\ns.nope = 2\ns.f"),
    );
    match (&vm, &tree) {
        (Err(a), Err(b)) => assert_eq!(a.message, b.message, "missing-field messages differ"),
        _ => panic!("both must error: {vm:?} vs {tree:?}"),
    }
}

/// `len(s.f)` fuses to [`Op::LenField`] (#242).
#[test]
fn vm_fused_len_field_emits_len_field() {
    let parsed = parse("struct W { f: [int] }\ns := W{f: [1, 2]}\nx := len(s.f)\n");
    assert!(parsed.errors.is_empty());
    let natives: Arc<std::collections::HashSet<String>> = Arc::new(
        ["vec.push".to_string(), "len".to_string()]
            .into_iter()
            .collect(),
    );
    let chunk = Compiler::compile_program_with_natives(&parsed.program, natives);
    assert!(
        has_op(&chunk, |op| matches!(op, Op::LenField { .. })),
        "expected fused LenField, got {:?}",
        chunk.code
    );
}

/// Differential: fused `len(s.f)` agrees, including strings, dicts,
/// missing fields, and unsized fields (#242).
#[test]
fn vm_tree_agree_on_len_field() {
    for src in [
        "struct W { f: [int] }\ns := W{f: [1, 2, 3]}\nlen(s.f)",
        "struct W { f: str }\ns := W{f: \"héllo\"}\nlen(s.f)",
        "struct W { f: [int] }\ns := W{f: []}\nlen(s.f)",
    ] {
        assert_same(src);
    }
    for src in [
        "struct W { f: int }\ns := W{f: 1}\nlen(s.f)",
        "struct W { f: int }\ns := W{f: 1}\nlen(s.nope)",
    ] {
        let (vm, tree) = (run_src(src), run_tree(src));
        match (&vm, &tree) {
            (Err(a), Err(b)) => assert_eq!(a.message, b.message, "len messages differ"),
            _ => panic!("both must error: {vm:?} vs {tree:?}"),
        }
    }
}

/// Short-circuit operators never fuse (`eval_binary` cannot execute
/// them; forcing the right side would diverge).
#[test]
fn vm_short_circuit_field_binop_stays_generic() {
    for src in [
        "struct W { f: bool }\ns := W{f: true}\ns.f = s.f && true\n",
        "struct W { f: bool }\ns := W{f: false}\ns.f = s.f || true\n",
    ] {
        let chunk = compile_with_push_native(src);
        assert!(
            !has_op(&chunk, |op| matches!(op, Op::FieldBinopTake { .. })),
            "short-circuit must not fuse, got {:?}",
            chunk.code
        );
        assert_same(src);
    }
    // Side-effect order: the right side runs only when reached.
    assert_same(
        "struct W { f: bool, n: int }\nfunc bump(w: W) -> bool {\n    w.n = w.n + 1\n    return true\n}\ns := W{f: false, n: 0}\ns.f = s.f && bump(s)\ns.n",
    );
}

/// `assert_same` for a recursive fixture: the tree-walker recurses on the
/// Rust stack, so the comparison runs on a worker with room for it (same
/// reason `bench_fib_vm_vs_tree` spawns a large-stack thread).
fn assert_same_deep(src: &'static str) {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || assert_same(src))
        .unwrap()
        .join()
        .unwrap();
}

/// Every call shape the direct-call fast path touches — the ones it takes
/// and the ones it declines — must resolve exactly as the tree-walker
/// does: same values, same error text.
#[test]
fn call_resolution_matches_tree_walker() {
    assert_same_deep(
        "func fib(n: int) -> int { return if n <= 1 { n } else { fib(n - 1) + fib(n - 2) } }\nfib(10)",
    );
    for src in [
        // Nested forward calls, result consumed by arithmetic.
        "func dbl(n: int) -> int { return n * 2 }\nfunc quad(n: int) -> int { return dbl(dbl(n)) }\nquad(4)",
        // Fewer arguments than parameters: the default must still be filled.
        "func greet(name: str, prefix: str = \"Hi\") -> str { return \"{prefix}, {name}\" }\ngreet(\"World\")",
        // More arguments than parameters: the same arity error, same text.
        "func one(a: int) -> int { return a }\none(1, 2)",
        // A closure with no bytecode still runs through the walker.
        "f := |x: int| x + 1\nf(1)",
        // Same, reached by name from a function body.
        "g := |x: int| x * 2\nfunc use() -> int { return g(3) }\nuse()",
        // Undefined callee: the error must still name it.
        "func caller() -> int { missing() }\ncaller()",
    ] {
        assert_same(src);
    }
}

/// Helper: a bytecode function of the given arity, defined in `env`.
fn mk_chunked_func(env: &EnvLink, params: usize) -> FuncValue {
    let chunk = Arc::new(Compiler::compile_program_with_natives(
        &parse("1").program,
        Arc::new(std::collections::HashSet::new()),
    ));
    FuncValue {
        params: (0..params)
            .map(|i| Param {
                name: Ident {
                    name: format!("p{i}"),
                    span: Span::new(0, 0),
                },
                ty: None,
                default: None,
                span: Span::new(0, 0),
            })
            .collect(),
        body: Expr::Block(Block {
            stmts: Vec::new(),
            span: Span::new(0, 0),
        }),
        env: env.clone(),
        chunk: Some(chunk),
        chunk_defaults: Vec::new(),
        // Chunked test closures keep expression-body semantics.
        tail_value: true,
    }
}

/// The fast path enters the frame — switching to the callee's definition
/// environment — for an unshadowed compiled function of matching arity,
/// without building the `Value::Func` the general path would clone.
#[test]
fn direct_call_enters_frame_for_compiled_function() {
    let mut interp = Interp::new();
    let caller_env = interp.env.clone();
    let def_env = Env::with_parent(&caller_env);
    interp.funcs.insert(
        "top.fib".to_string(),
        std::rc::Rc::new(mk_chunked_func(&def_env, 1)),
    );
    let mut vm = Vm::new();
    let entered =
        vm.try_enter_direct_call("top.fib", vec![Value::Int(1)], Span::new(0, 0), &mut interp);
    assert!(entered.is_none(), "matching arity must enter the frame");
    assert!(
        EnvLink::ptr_eq(&interp.env, &def_env),
        "entering must switch to the callee's definition environment"
    );
    assert!(!EnvLink::ptr_eq(&interp.env, &caller_env));
}

/// Whatever the fast path cannot prove equivalent comes back as the
/// original arguments with the environment untouched, so the general
/// path still fills defaults, reports arity errors, and honours
/// shadowing.
#[test]
fn direct_call_declines_and_hands_the_arguments_back() {
    let mut interp = Interp::new();
    let caller_env = interp.env.clone();
    let def_env = Env::with_parent(&caller_env);
    interp.funcs.insert(
        "top.fib".to_string(),
        std::rc::Rc::new(mk_chunked_func(&def_env, 1)),
    );
    let mut closureless = mk_chunked_func(&def_env, 0);
    closureless.chunk = None;
    interp
        .funcs
        .insert("top.closure".to_string(), std::rc::Rc::new(closureless));
    let span = Span::new(0, 0);
    let mut vm = Vm::new();

    // Arity mismatch: defaults and arity errors belong to the general path.
    let back = vm
        .try_enter_direct_call("top.fib", Vec::new(), span, &mut interp)
        .expect("declined calls return their arguments");
    assert!(back.is_empty());

    // Unknown name.
    let back = vm
        .try_enter_direct_call("top.missing", vec![Value::Int(1)], span, &mut interp)
        .expect("declined calls return their arguments");
    assert_eq!(back, vec![Value::Int(1)]);

    // Registered but without bytecode (an AST-only closure).
    let back = vm
        .try_enter_direct_call("top.closure", Vec::new(), span, &mut interp)
        .expect("declined calls return their arguments");
    assert!(back.is_empty());

    // A shadowing non-function binding wins over the function table.
    interp.env.define("top.fib", Value::Int(9));
    let back = vm
        .try_enter_direct_call("top.fib", vec![Value::Int(1)], span, &mut interp)
        .expect("declined calls return their arguments");
    assert_eq!(back, vec![Value::Int(1)]);

    // ...and no decline switched the environment.
    assert!(EnvLink::ptr_eq(&interp.env, &caller_env));
}

// ---------------------------------------------------------------------
// `for` loop codegen: folded safepoint + dead loop-result bookkeeping.
// ---------------------------------------------------------------------

/// A `for` loop's cooperative yield check is charged to `ForNext` — the
/// header target for both the back-edge and `continue` — instead of a
/// standalone `Safepoint` op: one dispatch less per iteration. The loop
/// header recorded in `ForSetup` must therefore name the `ForNext` itself.
#[test]
fn for_loop_header_is_fornext_carrying_the_safepoint() {
    let chunk =
        compile_with_push_native("total := 0\nfor i in 0..10 {\n    total = total + i\n}\ntotal");
    let header = chunk
        .code
        .iter()
        .find_map(|op| match op {
            Op::ForSetup { header, .. } => Some(*header),
            _ => None,
        })
        .expect("expected a ForSetup op");
    assert!(
        matches!(chunk.code[header], Op::ForNext { .. }),
        "loop header must be the ForNext that runs the check, got {:?}",
        chunk.code[header]
    );
}

/// v4 rotates `for` loops: the test is the loop's last op and jumps
/// forward into the body, so a live iteration is one dispatch instead of a
/// test plus an unconditional back-edge `Jump`. Guards the layout: the body
/// sits between the loop's entry `Jump` and the test, `ForSetup` names the
/// test, and the only jump to the test is that entry (no back-edge `Jump`).
#[test]
fn for_loop_back_edge_is_the_fornext_itself() {
    let chunk =
        compile_with_push_native("total := 0\nfor i in 0..10 {\n    total = total + i\n}\ntotal");
    let (test_pc, body) = chunk
        .code
        .iter()
        .enumerate()
        .find_map(|(pc, op)| match op {
            Op::ForNext { body, .. } => Some((pc, *body)),
            _ => None,
        })
        .expect("expected a ForNext op");
    assert!(
        body < test_pc,
        "rotated body must precede the test (body {body}, test {test_pc})"
    );
    assert!(
        matches!(chunk.code[body - 1], Op::Jump(t) if t == test_pc),
        "loop must enter at its test, got {:?}",
        chunk.code[body - 1]
    );
    let header = chunk
        .code
        .iter()
        .find_map(|op| match op {
            Op::ForSetup { header, .. } => Some(*header),
            _ => None,
        })
        .expect("expected a ForSetup op");
    assert_eq!(header, test_pc, "ForSetup must name the rotated test");
    let jumps_to_test: Vec<usize> = chunk
        .code
        .iter()
        .enumerate()
        .filter_map(|(pc, op)| match op {
            Op::Jump(t) if *t == test_pc => Some(pc),
            _ => None,
        })
        .collect();
    assert_eq!(
        jumps_to_test,
        vec![body - 1],
        "a rotated loop has no back-edge `Jump`; only the entry targets the test"
    );
}

/// `fornext_slots[k]` must describe the kth `ForNext` in chunk order (AOT
/// peels index it by op order). Rotation emits the test after the body, so
/// an inner loop's op precedes its outer loop's — the slots table has to
/// follow the ops, not the nesting.
#[test]
fn nested_loop_vartab_slots_follow_chunk_order() {
    let chunk = compile_with_push_native(
        "total := 0\nfor i in 0..2 {\n    for j in 0..3 {\n        total = total + j\n    }\n}\ntotal",
    );
    assert_eq!(chunk.fornext_slots.len(), 2, "two loops");
    // The inner var sits above the outer var on the stack, and the inner
    // test comes first in code order.
    assert!(
        chunk.fornext_slots[0][0] > chunk.fornext_slots[1][0],
        "vartab order must match ForNext order: {:?}",
        chunk.fornext_slots
    );
}

/// `for i in a..b` takes the `ForNext` handler's in-place fast path (the
/// value stack keeps `[range, idx, var]`, so a live iteration is a compare
/// plus two writes). It must be indistinguishable from the generic
/// truncate/pop/push path, including the exhausted edge's loop-frame pop and
/// truncation — so every case is differential against the tree-walker.
#[test]
fn range_loop_fast_path_matches_the_tree_walker() {
    // Straight line.
    assert_same("total := 0\nfor i in 0..10 {\n    total = total + i\n}\ntotal");
    // Empty range: the first test exits without running the body.
    assert_same("total := 0\nfor i in 3..3 {\n    total = total + 1\n}\ntotal");
    // Nonzero start.
    assert_same("total := 0\nfor i in 5..9 {\n    total = total + i\n}\ntotal");
    // Nested with `continue` (jumps to the test) and `break` (jumps to the
    // exit after `Break` pops the loop frame).
    assert_same(
        "total := 0\nfor i in 0..6 {\n    if i == 1 { continue }\n    for j in 0..6 {\n        if j == 4 { break }\n        total = total + i * j\n    }\n}\ntotal",
    );
    // An inner range loop that *exhausts* must pop its loop frame, otherwise
    // the outer `continue` below still sees the inner one.
    assert_same(
        "n := 0\nfor i in 0..4 {\n    for j in 0..2 {\n        n = n + j\n    }\n    if i == 1 { continue }\n    n = n + i\n}\nn",
    );
    // Statement-position loop whose body leaves a value: the exit stack must
    // still line up with the caller's `Pop`.
    assert_same("total := 0\nfor i in 0..4 {\n    total = total + i\n    total * 2\n}\ntotal");
    // A captured loop variable takes the generic `in_env` path. (Wrapped in
    // a function: at module scope a closure capturing the loop var is a
    // pre-existing gap, tracked separately.)
    assert_same(
        "func f() -> int {\n    total := 0\n    for i in 0..3 {\n        g := |x: int| { x + i }\n        total = total + g(1)\n    }\n    return total\n}\nf()",
    );
}

/// A `for` loop in non-final statement position has its value discarded by
/// the caller's `Op::Pop`, so the per-iteration `Unit` + `SetLoopResult`
/// pair is dead work and must not be emitted. The `Unit` placeholder pushed
/// before `ForSetup` is what that `Pop` removes, keeping the stack balanced.
#[test]
fn dead_for_loop_result_omits_bookkeeping() {
    let chunk =
        compile_with_push_native("total := 0\nfor i in 0..10 {\n    total = total + i\n}\ntotal");
    assert!(
        !has_op(&chunk, |op| matches!(op, Op::SetLoopResult)),
        "a discarded loop result must not emit SetLoopResult"
    );
}

/// The value of a `for` loop that *is* used (final statement of its block,
/// where no `Pop` follows) must still land in the loop's result slot.
#[test]
fn live_for_loop_result_keeps_bookkeeping() {
    let chunk = compile_with_push_native("for i in 0..10 {\n    i\n}");
    assert!(
        has_op(&chunk, |op| matches!(op, Op::SetLoopResult)),
        "a used loop result must still emit SetLoopResult"
    );
}

/// A discarded assignment statement leaves no value behind at all: the `Unit`
/// it used to push, and the caller's matching `Op::Pop`, both disappear —
/// nothing ever read that value.
#[test]
fn discarded_assignment_statement_pushes_no_unit() {
    let chunk = compile_with_push_native("total := 0\ntotal = 1\ntotal");
    assert!(
        !has_op(&chunk, |op| matches!(op, Op::Pop)),
        "a discarded assignment owes no Pop: {:#?}",
        chunk.code
    );
}

/// The same elision at a loop tail. A `for` whose own value is discarded can
/// tell its body the trailing value is dead too, so the per-iteration `Unit`
/// push and the `Op::Pop` that dropped it are both gone: the simplest
/// accumulate loop drops from 8 ops per iteration to 6.
#[test]
fn dead_loop_body_assignment_pushes_no_value() {
    let chunk =
        compile_with_push_native("total := 0\nfor i in 0..10 {\n    total = total + i\n}\ntotal");
    let (header, exit) = chunk
        .code
        .iter()
        .find_map(|op| match op {
            Op::ForSetup { header, exit, .. } => Some((*header, *exit)),
            _ => None,
        })
        .expect("expected a ForSetup op");
    for op in &chunk.code[header..exit] {
        assert!(
            !matches!(op, Op::Pop | Op::PushConst(_) | Op::SetLoopResult),
            "a dead loop body must neither push nor drop a value, got {op:?} in {:#?}",
            chunk.code
        );
    }
}

/// A type alias erases at check time, so its statement emits nothing and owns
/// no value. Reporting `Discard` made the owning container pop a value that
/// was never pushed, shifting every later slot by one — the next declaration
/// then panicked in `StoreSlot`.
#[test]
fn type_alias_statement_owns_no_value() {
    let chunk = compile_with_push_native("type A = int\nx := 41\nx");
    assert!(
        !has_op(&chunk, |op| matches!(op, Op::Pop)),
        "a type alias statement owes no Pop: {:#?}",
        chunk.code
    );
}

/// `while` used to pay a standalone header `Safepoint`. The check now rides
/// on `WhileCond` (see `Compiler::compile_while`), which entry, the
/// back-edge and `continue` all pass through — so the op has to be gone
/// from the stream, not merely unreachable.
#[test]
fn while_loop_folds_its_safepoint_into_the_test() {
    let chunk = compile_with_push_native("i := 0\nwhile i < 10 {\n    i = i + 1\n}\ni");
    assert!(
        !has_op(&chunk, |op| matches!(op, Op::Safepoint)),
        "while loops fold the safepoint into `WhileCond`: {:#?}",
        chunk.code
    );
    assert!(
        has_op(&chunk, |op| matches!(op, Op::WhileCond { .. })),
        "expected a WhileCond: {:#?}",
        chunk.code
    );
}

/// The fold must keep the loop cooperative: a CPU-bound `while` loop still
/// has to reach `Flow::Yield`, or `task.spawn` siblings would starve.
#[test]
fn while_loop_still_yields_cooperatively() {
    let chunk = Arc::new(compile_with_push_native(
        "i := 0\nwhile i < 200000 {\n    i = i + 1\n}\ni",
    ));
    // Safepoints only yield on executor threads (main-thread runs have no
    // executor to hand the slice to), so run this slice as a green task.
    let outcome = crate::value::with_executor_task(1, || {
        let mut vm = Vm::new();
        let mut interp = Interp::new();
        vm.run_chunk(&chunk, &mut interp)
    });
    match outcome {
        Ok(crate::runtime::Flow::Yield(crate::value::YieldReason::Timeslice)) => {}
        other => panic!("while loop must hit a cooperative safepoint, got {other:?}"),
    }
}

/// A statement-position `while` writes no loop result: no `SetLoopResult`
/// per iteration, no unit pushed for it, and the result placeholder is
/// dropped once on exit.
#[test]
fn vm_statement_while_emits_no_result_dance() {
    let chunk = compile_with_push_native("i := 0\nwhile i < 10 {\n    i = i + 1\n}\ni");
    assert!(
        !has_op(&chunk, |op| matches!(op, Op::SetLoopResult)),
        "a dead `while` must not store a loop result: {:#?}",
        chunk.code
    );
    let cond = chunk
        .code
        .iter()
        .position(|op| matches!(op, Op::WhileCond { .. }))
        .expect("expected a WhileCond");
    let body: Vec<&Op> = chunk.code[cond + 1..]
        .iter()
        .take_while(|op| !matches!(op, Op::Jump(_)))
        .collect();
    let pushes_unit = body.iter().any(|op| match op {
        Op::PushConst(i) => matches!(chunk.constants.get(*i as usize), Some(Value::Unit)),
        _ => false,
    });
    assert!(
        !pushes_unit
            && !body
                .iter()
                .any(|op| matches!(op, Op::Pop | Op::SetLoopResult)),
        "a dead `while` body must neither push nor drop a value, got {body:?}"
    );
}

/// Loop codegen must not change observable behavior: discarded vs used loop
/// values, nesting, `break`/`continue`, and enough iterations that any
/// per-iteration stack imbalance would blow up instead of passing quietly.
#[test]
fn for_loop_codegen_matches_tree_walker() {
    for src in [
        // Discarded result, accumulating body.
        "total := 0\nfor i in 0..10 {\n    total = total + i\n}\ntotal",
        // Used result: the loop's value is its last body value.
        "for i in 0..3 {\n    i\n}",
        // Nested loops, both discarded.
        "out := 0\nfor i in 0..3 {\n    for j in 0..3 {\n        out = out + i * j\n    }\n}\nout",
        // Discarded loop nested inside a block.
        "s := 0\nif true {\n    for i in 0..4 {\n        s = s + i\n    }\n}\ns",
        // `break` / `continue` out of a discarded loop.
        "n := 0\nfor i in 0..10 {\n    if i == 5 {\n        break\n    }\n    n = n + i\n}\nn",
        "n := 0\nfor i in 0..10 {\n    if i % 2 == 0 {\n        continue\n    }\n    n = n + i\n}\nn",
        // Array iteration; body declaring its own slot.
        "x := 0\nfor v in [1, 2, 3] {\n    x = x + v\n}\nx",
        "n := 0\nfor i in 0..3 {\n    v := i * 2\n    n = n + v\n}\nn",
        // Empty body: the block supplies the loop's result value.
        "n := 0\nfor i in 0..3 {\n}\nn",
        // 200k iterations: a per-iteration stack imbalance in the elided
        // path would exhaust memory instead of passing.
        "n := 0\nfor i in 0..200000 {\n    n = n + 1\n}\nn",
        // `if`-terminated body: the arms merge onto a single continuation, so
        // an elided value must not leave a branch jumping at a deleted op.
        "total := 0\nfor i in 0..8 {\n    if i % 2 == 0 {\n        total = total + i\n    } else {\n        total = total - i\n    }\n}\ntotal",
        // Compound assignment (rewritten to an assignment) in dead position.
        "total := 0\nfor i in 0..8 {\n    total += i\n}\ntotal",
        // Several dead assignment statements before the body's last one.
        "a := 0\nb := 0\nfor i in 0..8 {\n    a = a + i\n    b = b + 1\n    a = a + b\n}\na",
        // Dead assignment statements in a plain (non-loop) block.
        "a := 0\nif true {\n    a = a + 1\n    a = a + 2\n}\na",
    ] {
        assert_same(src);
    }
}

/// Variant methods on a typed receiver compile to the receiver namespace's
/// native (`option.unwrap_or`, `result.unwrap`, …) instead of the dynamic
/// `CallMethod` op — the same function the VM's value-based dispatch picks,
/// but visible to the AOT backends, which have no run-time method table
/// (#508). The tree-walker resolves both spellings identically.
#[test]
fn vm_variant_method_calls_resolve_statically() {
    // `main` returns its last expression, so `assert_same` compares the
    // unwrapped value, not a discarded statement.
    for src in [
        "func main() {\n    a := .some(1)\n    return a.unwrap_or(0)\n}",
        "func main() {\n    a := .some(1)\n    return a.unwrap()\n}",
        "func main() {\n    a := .some(4)\n    return a.expect(\"boom\")\n}",
        "func main() {\n    a: Result<int, str> = .ok(2)\n    return a.unwrap_or(3)\n}",
        "func main() {\n    a: Result<int, str> = .err(\"x\")\n    return a.unwrap_or(-1)\n}",
        "func main() {\n    a: Result<int, str> = .ok(6)\n    return a.expect(\"boom\")\n}",
        // Compound receiver: no slot to type, so the span map supplies it.
        "func main() {\n    return .some(7).unwrap_or(0)\n}",
        // Non-variant receivers keep their own namespaces.
        "func main() {\n    return [3, 1, 2].len()\n}",
    ] {
        assert_same(src);
    }
}

/// Structural pin for the decision: the typed compile emits the resolved
/// `CallNative`, untyped compiles keep the dynamic op, and a same-named
/// free function keeps it too — `lookup_method_recv` tries a bare function
/// first, so a static `option.unwrap_or` would silently run the stdlib
/// method instead of the user's function.
#[test]
fn vm_variant_method_op_shape() {
    use std::collections::{HashMap, HashSet};

    /// Depth-first scan: function bodies live in nested `MakeFunc` chunks.
    fn any_op(code: &[Op], pred: &dyn Fn(&Op) -> bool) -> bool {
        code.iter().any(|op| {
            pred(op)
                || match op {
                    Op::MakeFunc { chunk, .. } => any_op(&chunk.code, pred),
                    _ => false,
                }
        })
    }

    fn ops(src: &str, typed: bool) -> Vec<Op> {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        if !typed {
            return Compiler::compile_program(&parsed.program).code;
        }
        let (_res, types) = zz_checker::check_program_typed(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        let natives: HashSet<String> = [
            "option.unwrap_or",
            "option.unwrap",
            "option.expect",
            "result.unwrap_or",
            "result.unwrap",
            "result.expect",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        Compiler::compile_program_typed(
            &parsed.program,
            Arc::new(types),
            HashMap::new(),
            HashMap::new(),
            Arc::new(natives),
        )
        .code
    }

    let local_body = "func main() {\n    a := .some(1)\n    return a.unwrap_or(0)\n}";
    let typed = ops(local_body, true);
    assert!(
        any_op(&typed, &|op| matches!(
            op,
            Op::CallNative { name, .. } if name == "option.unwrap_or"
        )),
        "typed variant method should compile to the resolved native: {typed:?}"
    );
    assert!(
        !any_op(&typed, &|op| matches!(
            op,
            Op::CallMethod { name, .. } if name == "unwrap_or"
        )),
        "no dynamic method op should survive: {typed:?}"
    );

    let chained = ops("func main() {\n    return .some(7).unwrap_or(0)\n}", true);
    assert!(
        any_op(&chained, &|op| matches!(
            op,
            Op::CallNative { name, .. } if name == "option.unwrap_or"
        )),
        "compound receiver should resolve from the span map: {chained:?}"
    );

    let untyped = ops(local_body, false);
    assert!(
        !any_op(&untyped, &|op| matches!(
            op,
            Op::CallNative { name, .. } if name.starts_with("option.")
        )),
        "without a type table the call must stay dynamic: {untyped:?}"
    );
    assert!(
        any_op(&untyped, &|op| matches!(
            op,
            Op::CallMethod { name, .. } if name == "unwrap_or"
        )),
        "untyped compiles keep the dynamic op: {untyped:?}"
    );

    let shadowed = ops(
        "func unwrap_or(v: Option<int>, d: int) -> int { return d }\nfunc main() {\n    a := .some(1)\n    return a.unwrap_or(0)\n}",
        true,
    );
    assert!(
        any_op(&shadowed, &|op| matches!(
            op,
            Op::CallMethod { name, .. } if name == "unwrap_or"
        )),
        "a same-named free function must keep the dynamic dispatch: {shadowed:?}"
    );
}

/// Statement-position `if` compiles without the value dance: the condition is
/// a fused slot compare, the arm leaves nothing, and the missing-else arm
/// emits no `const unit` / `jmp` / `pop`. Inside a rotated loop that is three
/// ops per iteration (`slotlessintimm`, `jfb`, the increment) instead of ten.
#[test]
fn vm_statement_if_in_loop_emits_no_value_dance() {
    use std::collections::{HashMap, HashSet};

    let src = "func f(n: int) -> int {\n    s := 0\n    for i in 0..n {\n        if i < 5 {\n            s = s + 1\n        }\n    }\n    return s\n}\nf(3)";
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let (_res, types) = zz_checker::check_program_typed(
        &parsed.program,
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );
    let chunk = Compiler::compile_program_typed(
        &parsed.program,
        Arc::new(types),
        HashMap::new(),
        HashMap::new(),
        Arc::new(HashSet::new()),
    );
    let code = &chunk.code;
    let fchunk = code
        .iter()
        .find_map(|op| match op {
            Op::MakeFunc { name, chunk, .. } if name == "f" || name.ends_with(".f") => {
                Some(chunk.clone())
            }
            _ => None,
        })
        .expect("no compiled function `f`");
    let code = &fchunk.code;
    let (header, body) = code
        .iter()
        .enumerate()
        .find_map(|(pc, op)| match op {
            Op::ForNext { body, .. } => Some((pc, *body)),
            _ => None,
        })
        .expect("no ForNext in the compiled loop");
    let body_ops = &code[body..header];
    assert!(
        matches!(
            body_ops,
            [
                Op::SlotLessIntImm { imm: 5, .. },
                Op::JumpIfFalseBool(target, _),
                Op::SlotInc { .. },
            ] if *target == header
        ),
        "unexpected loop body: {body_ops:?}"
    );
}

/// The `if`-statement shapes that carry locals, loops or early exits must stay
/// balanced: the VM and the tree-walker have to agree on all of them.
#[test]
fn statement_if_shapes_match_the_tree_walker() {
    for src in [
        // Dead `if`, no else.
        "s := 0\nfor i in 0..6 {\n    if i < 4 {\n        s = s + i\n    }\n}\ns",
        // `if` / `else`.
        "s := 0\nfor i in 0..6 {\n    if i < 3 {\n        s = s + i\n    } else {\n        s = s - i\n    }\n}\ns",
        // `else if` chain: every arm must merge with nothing on the stack.
        "s := 0\nfor i in 0..9 {\n    if i < 3 {\n        s = s + 1\n    } else if i < 6 {\n        s = s + 10\n    } else {\n        s = s + 100\n    }\n}\ns",
        // Branch declaring its own local: exercises the block's `PopN`.
        "s := 0\nfor i in 0..4 {\n    if i < 2 {\n        v := i * 100\n        s = s + v\n    }\n}\ns",
        // Nested `if`, both dead.
        "s := 0\nfor i in 0..4 {\n    if i < 3 {\n        if i < 1 {\n            s = s + 1\n        } else {\n            s = s + 2\n        }\n    }\n}\ns",
        // A loop as the trailing statement of a dead arm: its placeholder is
        // the arm's leftover value and must be settled by the block.
        "s := 0\nif true {\n    for i in 0..4 {\n        s = s + i\n    }\n}\ns",
        // Early exit inside a dead `if`.
        "n := 0\nfor i in 0..10 {\n    if i == 5 {\n        break\n    }\n    n = n + i\n}\nn",
        // `if` in value position keeps the value-producing shape.
        "func f(n: int) -> int {\n    return if n < 3 { 1 } else { 2 }\n}\nf(1) + f(9)",
        // `if` as the returned tail of a function.
        "func f(n: int) -> int {\n    if n < 2 {\n        return 1\n    }\n    return 2\n}\nf(1) + f(5)",
        // Dead `if` at module scope.
        "a := 0\nif true {\n    a = a + 1\n    a = a + 2\n}\na",
    ] {
        assert_same(src);
    }
}

/// A named body ending in a statement-shaped `if` (both arms unit) still
/// owns its locals: the trailing value is dead, so the block cleanup must
/// drop them without assuming a result value was pushed. Regression: the
/// frame fell through with `PopN` one value too deep
/// ("block cleanup drops 3 values from a 2-deep stack").
#[test]
fn named_body_ending_in_if_statement_settles_locals() {
    assert_same(
        "func main() {\n    t := true\n    f := false\n    if t {\n        println(1)\n    }\n    if f {\n        println(999)\n    } else {\n        println(2)\n    }\n}",
    );
}

/// Module-scope loop variables captured by a closure must bind in the
/// environment, exactly like a function-local captured loop var: the
/// closure body runs in its own frame and can only read env bindings, so a
/// slot-homed `i` left it undefined at run time (#532). Same source inside
/// `func main()` always worked.
#[test]
fn vm_module_scope_loop_capture() {
    for src in [
        // Closure captures the loop var, reads it per iteration.
        "total := 0\nfor i in 0..3 {\n    f := |x: int| { x + i }\n    total = total + f(1)\n}\nprintln(total)",
        // Per-iteration rebinding: the captured value tracks the current
        // iteration, not a stale first binding.
        "vals := [7, 8, 9]\ntotal := 0\nfor i in 0..3 {\n    f := |x: int| { x + i }\n    total = total + f(vals[i])\n}\nprintln(total)",
        // Captured loop var over a non-range iterable.
        "total := 0\nfor x in [5, 6] {\n    add := |y: int| { y + x }\n    total = total + add(1)\n}\nprintln(total)",
        // Block-local captured inside a top-level loop (no loop var).
        "total := 0\nfor _ in 0..2 {\n    bump := 3\n    f := |y: int| { y + bump }\n    total = total + f(0)\n}\nprintln(total)",
        // Nested top-level loops: inner closure captures both vars.
        "total := 0\nfor i in 0..2 {\n    for j in 0..2 {\n        f := |x: int| { x + i * 10 + j }\n        total = total + f(0)\n    }\n}\nprintln(total)",
        // Control: uncaptured loop var keeps the slot fast path and the
        // same result.
        "total := 0\nfor i in 0..4 {\n    total = total + i\n}\nprintln(total)",
    ] {
        assert_same(src);
    }
}

/// The `while` shapes whose value can be proven dead (6.8) must stay
/// balanced against the tree-walker: statement position, value position
/// (the loop is the tail of a function or the program), every body-tail
/// kind, early exits, nesting and env-captured bodies.
#[test]
fn statement_while_shapes_match_the_tree_walker() {
    for src in [
        // Dead `while` in a loop body (the hot shape 6.8 elides).
        "s := 0\nfor i in 0..4 {\n    j := 0\n    while j < 3 {\n        s = s + j\n        j = j + 1\n    }\n}\ns",
        // `break` and `continue` inside a dead `while`.
        "i := 0\nwhile true {\n    i = i + 1\n    if i == 3 {\n        break\n    }\n}\ni",
        "i := 0\nn := 0\nwhile i < 6 {\n    i = i + 1\n    if i < 4 {\n        continue\n    }\n    n = n + i\n}\nn",
        // Body declaring a local: the block's `PopN` settle.
        "i := 0\ns := 0\nwhile i < 4 {\n    v := i * 2\n    s = s + v\n    i = i + 1\n}\ns",
        // Trailing slot declaration as the body's last statement: the
        // `needs_result_unit` path is skipped in the dead form.
        "i := 0\nwhile i < 3 {\n    i = i + 1\n    v := i\n}\ni",
        // Empty body.
        "i := 0\nwhile i < 3 {\n    i = i + 1\n}\ni",
        // Nested `while`, both dead.
        "i := 0\nn := 0\nwhile i < 3 {\n    j := 0\n    while j < 3 {\n        j = j + 1\n        n = n + 1\n    }\n    i = i + 1\n}\nn",
        // Dead `while` inside a dead `if` arm.
        "i := 0\nif true {\n    while i < 3 {\n        i = i + 1\n    }\n}\ni",
        // Env-captured body (the loop var is captured): keeps the
        // value-producing path, and the placeholder must still be dropped
        // exactly once.
        "total := 0\ni := 0\nwhile i < 3 {\n    f := |x: int| { x + i }\n    total = total + f(1)\n    i = i + 1\n}\ntotal",
        // Value position: the program's own tail and a unit function's tail
        // keep the value-producing shape.
        "i := 0\nwhile i < 3 {\n    i = i + 1\n}",
        "func f() {\n    i := 0\n    while i < 3 {\n        i = i + 1\n    }\n}\nf()",
        // Loop result read by the caller (`while` value used).
        "func f() {\n    i := 0\n    r := 0\n    while i < 3 {\n        i = i + 1\n        r = i\n    }\n    r\n}\nf()",
        // Early `return` out of a dead `while`.
        "func f(n: int) -> int {\n    i := 0\n    while true {\n        i = i + 1\n        if i >= n {\n            return i\n        }\n    }\n    return 0\n}\nf(4) + f(9)",
        // 5k iterations: a per-iteration stack imbalance shows up as a
        // shifted slot rather than passing quietly.
        "i := 0\ns := 0\nwhile i < 5000 {\n    s = s + i\n    i = i + 1\n}\ns",
    ] {
        assert_same(src);
    }
}

/// The `while` hot shape (see `bench/vm_gate/while_sum.zz`): one iteration
/// is exactly the test, the body and the back-edge. The cooperative check
/// rides inside `WhileCond` and the dead loop result no longer costs a
/// `PushConst unit` + `SetLoopResult` pair, so nothing may sneak back in.
#[test]
fn while_sum_hot_shape_is_test_body_and_backedge() {
    use std::collections::{HashMap, HashSet};

    let parsed =
        parse("sum := 0\ni := 0\nwhile i < 1000000 {\n    sum = sum + i\n    i = i + 1\n}\nsum");
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let (_res, types) = zz_checker::check_program_typed(
        &parsed.program,
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );
    let chunk = super::Compiler::compile_program_typed(
        &parsed.program,
        Arc::new(types),
        HashMap::new(),
        HashMap::new(),
        Arc::new(HashSet::new()),
    );
    let setup = chunk
        .code
        .iter()
        .position(|op| matches!(op, Op::WhileSetup { .. }))
        .expect("expected a WhileSetup");
    let (exit, header) = match &chunk.code[setup] {
        Op::WhileSetup { exit, header } => (*exit, *header),
        other => panic!("expected a WhileSetup, got {other:?}"),
    };
    assert_eq!(
        header,
        setup + 2,
        "the header follows the setup and its result placeholder: {:#?}",
        chunk.code
    );
    assert!(
        matches!(chunk.code.get(exit), Some(Op::Pop)),
        "a dead `while` drops its result placeholder on exit: {:#?}",
        chunk.code
    );
    let body = &chunk.code[header..exit];
    assert!(
        matches!(
            body,
            [
                Op::SlotLessIntImm { .. },
                Op::WhileCond { .. },
                Op::SlotAddInt { .. },
                Op::SlotInc { .. },
                Op::Jump(_),
            ]
        ),
        "unexpected while iteration shape: {body:?}"
    );
}

#[test]
fn for_loop_var_reassignment_drops_the_replaced_value() {
    // Dynamic (untyped) compilation can put a heap value in a `for`
    // loop-variable slot, which the `ForNext` fast path rewrites every
    // iteration. The fast path skips `drop_in_place` when the slot already
    // holds a scalar; anything else must still be dropped, so this pins the
    // checked branch (a blanket elision would trip `overwrite_scalar`'s
    // debug assertion and would leak the string in release).
    let v = run_vm_native(
        "func f() {\n    total := 0\n    for i in 0..3 {\n        i = \"heap value\"\n        total = total + 1\n    }\n    return total\n}\nf()",
    )
    .expect("run");
    assert_eq!(v, Value::Int(3));
}

#[test]
fn int_slot_writes_keep_value_semantics() {
    // The drop-eliding writes are only safe because the old value is a
    // scalar; make sure a slot that goes from heap back to int (and back)
    // keeps the value/ownership semantics of a plain assignment.
    let v = run_vm_native(
        "func f() {\n    x := \"s\"\n    x = 1\n    n := 0\n    for i in 0..3 {\n        n = n + 1\n        x = \"t{i}\"\n        x = i\n    }\n    return n * 100 + x\n}\nf()",
    )
    .expect("run");
    assert_eq!(v, Value::Int(302));
}

/// Compile `src` with `natives` registered, as `zz run` does.
fn compile_with_natives(src: &str, natives: &[&str]) -> super::Chunk {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let names: Arc<std::collections::HashSet<String>> =
        Arc::new(natives.iter().map(|n| n.to_string()).collect());
    Compiler::compile_program_with_natives(&parsed.program, names)
}

/// Depth-first search through function bodies (`MakeFunc` chunks).
fn any_op(code: &[Op], pred: &dyn Fn(&Op) -> bool) -> bool {
    code.iter().any(|op| {
        pred(op)
            || match op {
                Op::MakeFunc { chunk, .. } | Op::MakeClosure { chunk, .. } => {
                    any_op(&chunk.code, pred)
                }
                _ => false,
            }
    })
}

#[test]
fn bare_native_call_emits_direct_dispatch() {
    // A registered native must compile to one `CallNative`: the generic
    // shape builds a throwaway `Value::Native` (box + name `String`) and
    // pays the dynamic call plumbing on every invocation.
    let chunk = compile_with_natives("func main() {\n    len(\"abcd\")\n}", &["len"]);
    assert!(
        any_op(
            &chunk.code,
            &|op| matches!(op, Op::CallNative { name, argc, .. } if name == "len" && *argc == 1)
        ),
        "registered native should emit CallNative: {:?}",
        chunk.code
    );
    assert!(
        !any_op(
            &chunk.code,
            &|op| matches!(op, Op::LoadVar(name, _) if name == "len")
        ),
        "fused native call must not also load the callee: {:?}",
        chunk.code
    );
}

#[test]
fn bare_native_dispatch_calls_the_registered_entry() {
    // Fused dispatch must hit the registered native (arity gate, argument
    // order, result placement) exactly like the generic path. `tally` has
    // no VM specialisation, so the result can only come from the registry
    // entry itself (`len` is specialised inside the handler; see
    // `len_fast_path_matches_the_native_semantics`).
    let v = run_vm_native("tally(20)").expect("run");
    assert_eq!(v, Value::Int(41));
}

#[test]
fn len_fast_path_matches_the_native_semantics() {
    // The handler specialises the bare `len` call ahead of the registry;
    // both must agree on every sized variant, on non-ASCII char counting,
    // and on the failure message for unsized values.
    let cases = [
        ("len([1, 2, 3])", "3"),
        ("len((1, 2))", "2"),
        ("len(\"abc\")", "3"),
        ("len(\"héllo\")", "5"),
        ("len(1..4)", "3"),
    ];
    for (src, want) in cases {
        let v = run_vm_native(src).expect("run");
        assert_eq!(v.to_string(), want, "wrong `len` for {src}");
    }
    let d = run_vm_native("d:{str: int} = {\"a\": 1, \"b\": 2}\nlen(d)").expect("run");
    assert_eq!(d, Value::Int(2));

    let err = run_vm_native("len(5)").expect_err("unsized").message;
    assert_eq!(
        err,
        "len expects array, tuple, bytes, string, dict, or range, found `5`"
    );

    // Arity still goes through the registry gate, not the fast path.
    let err = run_vm_native("len()").expect_err("arity").message;
    assert!(
        err.contains("expected 1 arguments, found 0"),
        "unexpected arity error: {err}"
    );
}

#[test]
fn user_function_shadowing_keeps_the_generic_call() {
    // `func len` must win over the native: the direct-dispatch guard sees
    // the user signature and keeps `LoadVar` + `Call`.
    let src = "func len(x: int) -> int {\n    return x + 100\n}\nlen(1)";
    let chunk = compile_with_natives(src, &["len"]);
    assert!(
        !any_op(
            &chunk.code,
            &|op| matches!(op, Op::CallNative { name, .. } if name == "len")
        ),
        "shadowed `len` must not take native dispatch: {:?}",
        chunk.code
    );
    assert!(
        any_op(&chunk.code, &|op| matches!(op, Op::Call { .. })),
        "shadowed `len` keeps the generic call: {:?}",
        chunk.code
    );
    let v = run_vm_native(src).expect("run");
    assert_eq!(v, Value::Int(101));
}

#[test]
fn local_binding_shadowing_keeps_the_generic_call() {
    let src = "func main() {\n    len := |x| x + 100\n    len(1)\n}";
    let chunk = compile_with_natives(src, &["len"]);
    assert!(
        !any_op(
            &chunk.code,
            &|op| matches!(op, Op::CallNative { name, .. } if name == "len")
        ),
        "local `len` must not take native dispatch: {:?}",
        chunk.code
    );
    assert_same(src);
}

#[test]
fn top_level_binding_shadowing_keeps_the_generic_call() {
    // A promoted top-level declaration is a frame slot, not an env
    // binding; the guard must still refuse to fuse.
    let src = "len := |x| x + 100\nlen(1)";
    let chunk = compile_with_natives(src, &["len"]);
    assert!(
        !any_op(
            &chunk.code,
            &|op| matches!(op, Op::CallNative { name, .. } if name == "len")
        ),
        "top-level `len` must not take native dispatch: {:?}",
        chunk.code
    );
    assert_same(src);
}

#[test]
fn native_call_arity_errors_match_the_generic_path() {
    // Both shapes must reject a wrong-arity native call with the same
    // message (`CallNative` gates on the registry entry's arity).
    let fused = run_vm_native("len()").expect_err("arity").message;
    let generic = run_vm_native("f := len\nf()").expect_err("arity").message;
    assert_eq!(fused, generic, "arity error text drifted");
}
