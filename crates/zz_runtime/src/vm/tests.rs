use std::rc::Rc;

use zz_frontend::ast::{BinOp, Block, Expr, Ident, Param};
use zz_frontend::parse;
use zz_frontend::span::Span;

use super::{Compiler, Op};
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
    interp
        .structs
        .insert("shapes.Point".into(), vec!["x".into(), "y".into()]);
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
        env: Rc::clone(&interp.env),
        chunk: Some(Rc::new(chunk)),
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
