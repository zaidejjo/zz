use std::sync::Arc;

use zz_frontend::ast::{BinOp, Block, Expr, Ident, Param};
use zz_frontend::parse;
use zz_frontend::span::Span;

use super::{Compiler, Op, Vm};
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
        "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nfunc f(r: Rect) -> int { r.p.x = 9\nr.p.x + r.w }\nf(Rect{ p: Point{ x: 1, y: 2 }, w: 3 })",
        "struct Point { x: int, y: int }\nstruct Rect { p: Point, w: int }\nfunc f(r: Rect) -> int { r.p.x = 9\nr.p.x }\nf(Rect{ p: Point{ x: 1, y: 2 }, w: 3 })",
        "struct A { b: B }\nstruct B { c: C }\nstruct C { v: int }\na := A{ b: B{ c: C{ v: 1 } } }\na.b.c.v = 42\na.b.c.v",
        "struct A { b: B }\nstruct B { c: C }\nstruct C { v: int }\nfunc f(a: A) -> int { a.b.c.v = 42\na.b.c.v }\nf(A{ b: B{ c: C{ v: 1 } } })",
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
fn vm_slot_locals_match_tree_walker() {
    for src in [
        "x := 1\n{ x := 2\nx }\nx",
        "x := 1\n{ y := 2\n{ z := 3\nx + y + z } }",
        "x := 1\n{ x := 2\n{ x := 3\nx } }",
        "x := 1\ny := 2\n{ y := 3\ny }\ny",
        "func f() -> int { a := 1\nb := 2\nc := a + b\nc }\nf()",
        "func f(n: int) -> int { m := n * 2\nm + n }\nf(5)",
        "func f(n: int) -> int { n := n + 1\nn }\nf(5)",
        "func f() -> int { x := 1\n{ x := 2\nx }\nx }\nf()",
        "match 5 { n => { m := n * 2\nm } }",
        "match 5 { 1 => 10, n => { m := n * 2\nm } }",
        "x := .some(5)\nif let .some(n) = x { m := n + 1\nm } else { 0 }",
        "x := .none\nif let .some(n) = x { m := n + 1\nm } else { 0 }",
        "x := .some(5)\nif let .some(n) = x { m := n + 1\nm }",
        "struct Point { x: int }\nfunc f(p: Point) -> int { p.x }\nf(Point{ x: 7 })",
        "struct Point { x: int }\nfunc dist(p: Point) -> int { p.x }\np := Point{ x: 9 }\np.dist()",
        "struct Point { x: int }\nstruct Holder { p: Point }\nfunc dist(p: Point) -> int { p.x }\nh := Holder{ p: Point{ x: 9 } }\nh.p.dist()",
        "struct Point { x: int }\nfunc f(p: Point) -> int { p.x = 5\np.x }\nf(Point{ x: 1 })",
        "func outer() { x := 10\n|x| x + 1 }\ng := outer()\ng(5)",
        "func outer() { x := 10\ny := 20\n|x| x + y }\ng := outer()\ng(5)",
        "func outer() { x := 10\n{ y := x + 1\ny } }\ng := outer()\ng(5)",
        "func counter() { n := 0\n|inc| { n = n + inc\nn } }\nc := counter()\nc(1)\nc(2)",
        "x := 0\nfor x in 0..3 { x }\nx",
        "x := 5\nfor i in 0..3 { x := i\nx }\nx",
        "sum := 0\nfor i in 0..3 { j := i * 2\nsum = sum + j }\nsum",
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
        "func dbl(n: int) -> int { n * 2 }\ndbl(21)",
        "func fib(n: int) -> int { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\nfib(5)",
        "func f() -> int { return 5 }\nf()",
        "func f() -> int { if true { return 5 }\n3 }\nf()",
        "func add(a: int, b: int) -> int { a + b }\nadd(2, 3)",
        "x := 1\nx = 5\nx",
        "x := 1\nx = x + 1\nx",
        "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x",
        "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }\ndist(Point{ x: 3, y: 4 })",
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
        "func even(n: int) -> bool { if n == 0 { true } else { odd(n - 1) } }\nfunc odd(n: int) -> bool { if n == 0 { false } else { even(n - 1) } }\neven(4)",
        "func apply(f, x) { f(x) }\napply(|n| n + 1, 41)",
        "func outer() { func inner(n: int) -> int { n * 3 }\ninner }\ng := outer()\ng(7)",
        "x := 1\n{ y := 2\nx + y }",
        "func f() -> int { { return 7 }\n0 }\nf()",
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
        "func f() -> int { for i in 0..3 { if i == 1 { return 42 } }\n0 }\nf()",
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
        "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }\ndist(Point{ x: 3, y: 4 })",
        "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\nfunc sum(p: Point) -> int { p.x + p.y }\nsum(p)",
        "struct Point { x: int, y: int }\nfunc mk() -> Point { Point{ x: 7, y: 8 } }\nmk().x",
        "f := |x| x * 2\nf(5)",
        "f := |x, y| x + y\nf(2, 3)",
        "n := 10\nf := |x| x + n\nf(5)",
        "f := |x| |y| x + y\nf(2)(3)",
        "f := |x| { y := x * 2\ny }\nf(5)",
        "f := |n| if n < 2 { n } else { f(n - 1) + f(n - 2) }\nf(5)",
        "func apply(f, x) { f(x) }\napply(|n| n + 1, 41)",
        "func outer() { func inner(n: int) -> int { n * 3 }\ninner }\ng := outer()\ng(7)",
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
        "func inc(n: int) -> int { n + 1 }\nfunc dbl(n: int) -> int { n * 2 }\n5\n  |> inc\n  |> dbl",
        "func inc(n: int) -> int { n + 1 }\nfunc dbl(n: int) -> int { n * 2 }\nval := 10\nval\n  |> inc\n  |> dbl\n  |> inc",
        "func inc(n: int) -> int { n + 1 }\nresult := {\n  5\n    |> inc\n    |> inc\n}\nresult",
    ] {
        assert_same(src);
    }
}

#[test]
fn vm_deep_recursion_no_rust_stack_overflow() {
    assert_eq!(
        run_src(
            "func fib(n: int) -> int { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\nfib(20)"
        )
        .unwrap(),
        Value::Int(6765)
    );
    assert_eq!(
        run_src(
            "func count(n: int) -> int { if n == 0 { 0 } else { count(n - 1) + 1 } }\ncount(100000)"
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
        "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }\np := Point{ x: 3, y: 4 }\np.dist()",
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
    };
    interp.funcs.insert("shapes.dist".into(), fv);
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
        "func fib(n: int) -> int { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\nfib(20)";
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
        "func rotl(x: int, k: int) -> int { (x << k) | (x >> (64 - k)) }\nrotl(305419896, 4)",
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

/// `b = f(b, x)` inside a function body takes the single `b` load
/// ([`Op::TakeSlot`]) instead of cloning it into the call.
#[test]
fn vm_thread_call_emits_take_slot() {
    let chunk = compile_with_push_native(
        "func f(x: [int], y: int) -> [int] { x }\nfunc g() -> [int] {\nb := [1]\nb = f(b, 2)\nb\n}\n",
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
        "func f(x: [int]) -> [int] { x }\nf([1, 2])[1]",
        "func f(x: [int]) -> [int] { return x }\nf([1, 2])[0]",
        "func f(n: int) -> int { m := n * 2\nm }\nf(20)",
        "struct W { f: [int] }\nfunc g(w: W) -> W { w }\ng(W{f: [7]}).f[0]",
        "x := 10\nf := |u: int| x + u\nf(0) + x",
        "x := [1, 2]\nf := |u: int| x[u]\nf(1) + x[0]",
        "func f(x: int) -> int { g := |u: int| x + u\ng(0) + x }\nf(3)",
        "func f(x: int) -> int { g := |u: int| x + u\ng(1) + g(2) + x }\nf(3)",
        "f := |u: int| u + 1\nf(1) + f(2)",
        "func f(n: int) -> int { defer println(\"dd\")\nn }\nf(41)",
        "outer := 99\nfunc f() -> int { outer }\nf() + outer",
    ] {
        assert_same(src);
    }
    assert_eq!(
        run_src("func f(x: [int]) -> [int] { x }\nf([1, 2, 3])[2]").unwrap(),
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
    Ok(Value::Array(Box::new(vs)))
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
    let src = "struct W { f: [int] }\nimpl W {\n func push(w: W, x: int) -> W {\n W{f: vec.push(w.f, x)}\n }\n}\nfunc go(s: W) -> int {\nt := s\nt = t.push(2)\nlen(t.f)\n}\ngo(W{f: [1]})\n";
    assert_eq!(run_vm_native(src).unwrap(), Value::Int(2));
    let src2 = "struct W { f: [int] }\nimpl W {\n func push(w: W, x: int) -> W {\n W{f: vec.push(w.f, x)}\n }\n}\nt := W{f: [1]}\nt = t.push(2)\nlen(t.f)\n";
    assert_eq!(run_vm_native(src2).unwrap(), Value::Int(2));
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
        "struct Rng { s0: int }\nfunc change(r: Rng) -> int { r.s0 += 100\nr.s0 }\nrng := Rng{ s0: 10 }\nchange(rng) * 1000 + rng.s0",
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

    let generic = "struct Box<T> { v: T }\nfunc main() {\n b := Box{ v: 42 }\n b.v\n}";
    let mono = "struct BoxInt { v: int }\nfunc main() {\n b := BoxInt{ v: 42 }\n b.v\n}";
    assert_eq!(opcodes(generic), opcodes(mono));
}

#[test]
fn vm_default_args_match_tree_walker() {
    // Omitted trailing defaults must evaluate (checker contract) on both
    // engines — same program (compiler inline fill) and across programs
    // (runtime fill from `FuncValue` defaults, which the per-program
    // compiler never saw).
    for src in [
        "func g(a: int, b: int = 5) -> int { a + b }\ng(1)",
        "func g(a: int, b: int = 5) -> int { a + b }\ng(1, 2)",
        "func g(a: int, b: int = 5, c: int = 7) -> int { a + b + c }\ng(1)",
        "func g(a: int, b: int = 5, c: int = 7) -> int { a + b + c }\ng(1, 2)",
    ] {
        assert_same(src);
    }
    assert_eq!(
        run_src("func g(a: int, b: int = 5) -> int { a + b }\ng(1)").unwrap(),
        Value::Int(6)
    );
    // Cross-program: `g` is defined in one program, called with an omitted
    // default in another (mirrors multi-file `import m` load order).
    let dep = parse("func g(a: int, b: int = 5) -> int { a + b }");
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

    let src = "func f(o: Option<int>) -> int {\n    limit := 10\n    match o {\n        .some(n) if limit > 5 => n,\n        .some(n) => 0,\n        .none => -1,\n    }\n}\n";
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
