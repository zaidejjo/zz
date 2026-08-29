use super::Interp;
use crate::runtime::{EvalError, NativeEntry};
use crate::value::Value;
use std::collections::HashMap;
use zz_frontend::parse;
use zz_frontend::span::Span;

fn eval_src(src: &str) -> Result<Value, EvalError> {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let mut interp = Interp::new();
    interp.run(&parsed.program)
}

#[test]
fn arithmetic_and_precedence() {
    assert_eq!(eval_src("1 + 2 * 3").unwrap(), Value::Int(7));
    assert_eq!(eval_src("(1 + 2) * 3").unwrap(), Value::Int(9));
    assert_eq!(eval_src("10 / 3").unwrap(), Value::Int(3));
    assert_eq!(eval_src("10 % 3").unwrap(), Value::Int(1));
    assert_eq!(eval_src("-5 + 2").unwrap(), Value::Int(-3));
}

#[test]
fn let_binding_evaluates_to_value() {
    assert_eq!(eval_src("let x = 1 + 2").unwrap(), Value::Int(3));
}

#[test]
fn let_references_previous_bindings() {
    assert_eq!(
        eval_src("let a = 10\nlet b = 20\nlet c = a + b\nc").unwrap(),
        Value::Int(30)
    );
}

#[test]
fn shadowing() {
    assert_eq!(
        eval_src("let x = 1\nlet x = x + 1\nx").unwrap(),
        Value::Int(2)
    );
}

#[test]
fn mixed_int_float_promotes() {
    assert_eq!(eval_src("1 + 2.5").unwrap(), Value::Float(3.5));
}

#[test]
fn division_by_zero_errors() {
    let err = eval_src("1 / 0").unwrap_err();
    assert_eq!(err.message, "division by zero");
}

#[test]
fn undefined_variable_errors() {
    let err = eval_src("nope + 1").unwrap_err();
    assert_eq!(err.message, "undefined variable `nope`");
}

#[test]
fn integer_overflow_errors() {
    let err = eval_src("9223372036854775807 + 1").unwrap_err();
    assert_eq!(err.message, "integer overflow in addition");
}

#[test]
fn empty_program_is_unit() {
    assert_eq!(eval_src("").unwrap(), Value::Unit);
}

#[test]
fn strings_and_concat() {
    assert_eq!(eval_src("\"a\" + \"b\"").unwrap(), Value::Str("ab".into()));
}

#[test]
fn comparisons_and_logic() {
    assert_eq!(eval_src("1 < 2").unwrap(), Value::Bool(true));
    assert_eq!(eval_src("1 == 1").unwrap(), Value::Bool(true));
    assert_eq!(eval_src("true && false").unwrap(), Value::Bool(false));
    assert_eq!(eval_src("true || false").unwrap(), Value::Bool(true));
    assert_eq!(eval_src("!true").unwrap(), Value::Bool(false));
}

#[test]
fn if_expression() {
    assert_eq!(eval_src("if true { 1 } else { 2 }").unwrap(), Value::Int(1));
    assert_eq!(
        eval_src("if false { 1 } else { 2 }").unwrap(),
        Value::Int(2)
    );
}

#[test]
fn closure_and_call() {
    assert_eq!(
        eval_src("let f = |x: int| x + 1\nf(5)").unwrap(),
        Value::Int(6)
    );
}

#[test]
fn closure_captures_env() {
    assert_eq!(
        eval_src("let a = 10\nlet f = |x: int| x + a\nf(5)").unwrap(),
        Value::Int(15)
    );
}

#[test]
fn named_func_and_recursion() {
    assert_eq!(
        eval_src("func fact(n: int) -> int { if n <= 1 { 1 } else { n * fact(n - 1) } }\nfact(5)")
            .unwrap(),
        Value::Int(120)
    );
}

#[test]
fn return_unwinds() {
    assert_eq!(
        eval_src("func f() -> int { if true { return 7 }\n 0 }\nf()").unwrap(),
        Value::Int(7)
    );
}

#[test]
fn match_option() {
    assert_eq!(
        eval_src("let v = .some(1)\nmatch v { .some(n) => n, .none => 0 }").unwrap(),
        Value::Int(1)
    );
    assert_eq!(
        eval_src("let v = .none\nmatch v { .some(n) => n, .none => 0 }").unwrap(),
        Value::Int(0)
    );
}

#[test]
fn match_result() {
    assert_eq!(
        eval_src("let v = .ok(1)\nmatch v { .ok(n) => n, .err(_) => 0 }").unwrap(),
        Value::Int(1)
    );
    assert_eq!(
        eval_src("let v = .err(\"x\")\nmatch v { .ok(n) => n, .err(_) => 0 }").unwrap(),
        Value::Int(0)
    );
}

#[test]
fn if_let() {
    assert_eq!(
        eval_src("let v = .some(3)\nif let .some(n) = v { n } else { 0 }").unwrap(),
        Value::Int(3)
    );
    assert_eq!(
        eval_src("let v = .none\nif let .some(n) = v { n } else { 0 }").unwrap(),
        Value::Int(0)
    );
}

#[test]
fn try_unwraps_option() {
    assert_eq!(
        eval_src("func f() -> Option<int> { let x = .some(1)?; .some(x) }\nf()").unwrap(),
        Value::Option(Some(Box::new(Value::Int(1))))
    );
}

#[test]
fn try_propagates_none() {
    assert_eq!(
        eval_src("func f() -> Option<int> { x := .none?; .some(x) }\nf()").unwrap(),
        Value::Option(None)
    );
}

#[test]
fn try_propagates_err() {
    assert_eq!(
        eval_src("func f() -> Result<int, str> { x := .err(\"boom\")?; .ok(x) }\nf()").unwrap(),
        Value::Result(Err(Box::new(Value::Str("boom".into()))))
    );
}

#[test]
fn variant_constructors() {
    assert_eq!(
        eval_src(".ok(1)").unwrap(),
        Value::Result(Ok(Box::new(Value::Int(1))))
    );
    assert_eq!(eval_src(".none").unwrap(), Value::Option(None));
}

#[test]
fn array_literal() {
    assert_eq!(
        eval_src("scores := [10, 20, 30]\nscores").unwrap(),
        Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
    );
}

#[test]
fn array_explicit_decl() {
    assert_eq!(
        eval_src("[int] scores = [1, 2]\nscores").unwrap(),
        Value::Array(vec![Value::Int(1), Value::Int(2)])
    );
}

#[test]
fn dict_literal() {
    assert_eq!(
        eval_src("ages := {\"Zaid\": 20}\nages").unwrap(),
        Value::Dict(vec![(Value::Str("Zaid".into()), Value::Int(20))])
    );
}

#[test]
fn dict_explicit_decl() {
    assert_eq!(
        eval_src("{str: int} ages = {\"a\": 1}\nages").unwrap(),
        Value::Dict(vec![(Value::Str("a".into()), Value::Int(1))])
    );
}

#[test]
fn dict_union_value_type() {
    assert_eq!(
        eval_src("{str: str | int} user = {\"name\": \"Zaid\", \"age\": 20}\nuser").unwrap(),
        Value::Dict(vec![
            (Value::Str("name".into()), Value::Str("Zaid".into())),
            (Value::Str("age".into()), Value::Int(20)),
        ])
    );
}

#[test]
fn import_is_noop() {
    assert_eq!(eval_src("import std.io\nx := 1\nx").unwrap(), Value::Int(1));
}

#[test]
fn native_function_dispatches() {
    #[allow(clippy::ptr_arg)]
    fn double(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
        let n = match args.first() {
            Some(Value::Int(n)) => *n,
            _ => return Err(EvalError::new("expected int", Span::new(0, 0))),
        };
        Ok(Value::Int(n * 2))
    }
    let mut natives = HashMap::new();
    natives.insert(
        "test.double".into(),
        NativeEntry {
            arity: 1,
            f: double,
        },
    );
    let mut interp = Interp::with_natives(natives);
    let parsed = parse("test.double(21)\n");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let v = interp.run(&parsed.program).unwrap();
    assert_eq!(v, Value::Int(42));
}

#[test]
fn native_wrong_arity_errors() {
    fn noop(_interp: &mut Interp, _: &mut Vec<Value>) -> Result<Value, EvalError> {
        Ok(Value::Unit)
    }
    let mut natives = HashMap::new();
    natives.insert("test.noop".into(), NativeEntry { arity: 1, f: noop });
    let mut interp = Interp::with_natives(natives);
    let parsed = parse("test.noop()\n");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let err = interp.run(&parsed.program).unwrap_err();
    assert!(
        err.message.contains("expected 1 arguments"),
        "{}",
        err.message
    );
}

#[test]
fn unknown_path_errors() {
    let err = eval_src("foo.bar.baz(1)").unwrap_err();
    assert!(
        err.message.contains("undefined variable `foo.bar`"),
        "{}",
        err.message
    );
}

#[test]
fn struct_init_and_field_access() {
    let v = eval_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x").unwrap();
    assert_eq!(v, Value::Int(1));
}

#[test]
fn struct_displays_with_name() {
    let v = eval_src("struct Point { x: int, y: int }\nPoint{ x: 1, y: 2 }").unwrap();
    assert_eq!(v.to_string(), "Point{x: 1, y: 2}");
}

#[test]
fn struct_field_mutation() {
    let v = eval_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x = 10\np.x")
        .unwrap();
    assert_eq!(v, Value::Int(10));
}

#[test]
fn struct_field_mutation_visible_in_object() {
    let v =
        eval_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x = 10\np").unwrap();
    assert_eq!(
        v,
        Value::Object {
            name: "Point".into(),
            fields: vec![("x".into(), Value::Int(10)), ("y".into(), Value::Int(2)),],
        }
    );
}

#[test]
fn struct_nested_field_access() {
    let v = eval_src(
            "struct Point { x: int, y: int }\nstruct Nested { p: Point, z: int }\nn := Nested{ p: Point{ x: 1, y: 2 }, z: 3 }\nn.p.y",
        )
        .unwrap();
    assert_eq!(v, Value::Int(2));
}

#[test]
fn struct_passed_to_func() {
    let v = eval_src(
            "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }\ndist(Point{ x: 3, y: 4 })",
        )
        .unwrap();
    assert_eq!(v, Value::Int(7));
}

#[test]
fn struct_missing_field_errors() {
    let err = eval_src("struct Point { x: int, y: int }\nPoint{ x: 1 }").unwrap_err();
    assert!(err.message.contains("missing field `y`"), "{}", err.message);
}

#[test]
fn struct_unknown_field_errors() {
    let err =
        eval_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.z").unwrap_err();
    assert!(err.message.contains("has no field `z`"), "{}", err.message);
}

#[test]
fn struct_field_on_non_struct_errors() {
    let err = eval_src("x := 5\nx.y").unwrap_err();
    assert!(
        err.message.contains("cannot access field `y`"),
        "{}",
        err.message
    );
}

#[test]
fn struct_unknown_type_errors() {
    let err = eval_src("Nope{ x: 1 }").unwrap_err();
    assert!(
        err.message.contains("unknown struct `Nope`"),
        "{}",
        err.message
    );
}

#[test]
fn for_over_range_sums() {
    let v = eval_src("sum := 0\nfor i in 0..5 { sum = sum + i }\nsum").unwrap();
    assert_eq!(v, Value::Int(10));
}

#[test]
fn for_over_array_sums() {
    let v = eval_src("total := 0\nfor n in [10, 20, 30] { total = total + n }\ntotal").unwrap();
    assert_eq!(v, Value::Int(60));
}

#[test]
fn for_break_stops_loop() {
    let v =
        eval_src("found := 0\nfor i in 0..10 { if i == 3 { found = i; break } }\nfound").unwrap();
    assert_eq!(v, Value::Int(3));
}

#[test]
fn for_continue_skips_iteration() {
    let v =
        eval_src("count := 0\nfor i in 0..5 { if i == 2 { continue }; count = count + 1 }\ncount")
            .unwrap();
    assert_eq!(v, Value::Int(4));
}

#[test]
fn for_loop_var_does_not_leak() {
    let err = eval_src("for i in 0..5 { i }\ni").unwrap_err();
    assert!(
        err.message.contains("undefined variable `i`"),
        "{}",
        err.message
    );
}

#[test]
fn for_over_non_iterable_errors() {
    let err = eval_src("for i in 5 { i }").unwrap_err();
    assert!(err.message.contains("cannot iterate"), "{}", err.message);
}

#[test]
fn break_outside_loop_errors() {
    let err = eval_src("break").unwrap_err();
    assert!(
        err.message.contains("`break` outside of a loop"),
        "{}",
        err.message
    );
}

#[test]
fn continue_outside_loop_errors() {
    let err = eval_src("continue").unwrap_err();
    assert!(
        err.message.contains("`continue` outside of a loop"),
        "{}",
        err.message
    );
}

#[test]
fn while_loop_with_break() {
    let v = eval_src("x := 0\nwhile x < 10 { x = x + 1; if x == 3 { break } }\nx").unwrap();
    assert_eq!(v, Value::Int(3));
}

#[test]
fn range_value_displays() {
    let v = eval_src("0..5").unwrap();
    assert_eq!(v.to_string(), "0..5");
}

#[test]
fn assignment_to_undefined_errors() {
    let err = eval_src("nope = 5").unwrap_err();
    assert!(
        err.message.contains("undefined variable `nope`"),
        "{}",
        err.message
    );
}

#[test]
fn closure_mutation_propagates() {
    let v = eval_src("x := 0\nf := |n: int| { x = x + n }\nf(5)\nf(3)\nx").unwrap();
    assert_eq!(v, Value::Int(8));
}

#[test]
fn array_index() {
    let v = eval_src("scores := [10, 20, 30]\nscores[1]").unwrap();
    assert_eq!(v, Value::Int(20));
}

#[test]
fn array_negative_index() {
    let v = eval_src("scores := [10, 20, 30]\nscores[-1]").unwrap();
    assert_eq!(v, Value::Int(30));
}

#[test]
fn array_index_out_of_bounds_errors() {
    let err = eval_src("scores := [1, 2]\nscores[5]").unwrap_err();
    assert!(
        err.message.contains("index 5 out of bounds for length 2"),
        "{}",
        err.message
    );
}

#[test]
fn array_negative_index_out_of_bounds_errors() {
    let err = eval_src("scores := [1, 2]\nscores[-3]").unwrap_err();
    assert!(
        err.message.contains("index -3 out of bounds for length 2"),
        "{}",
        err.message
    );
}

#[test]
fn dict_index() {
    let v = eval_src("ages := {\"a\": 1, \"b\": 2}\nages[\"b\"]").unwrap();
    assert_eq!(v, Value::Int(2));
}

#[test]
fn dict_missing_key_errors() {
    let err = eval_src("ages := {\"a\": 1}\nages[\"zz\"]").unwrap_err();
    assert!(
        err.message.contains("key `zz` not found in dict"),
        "{}",
        err.message
    );
}

#[test]
fn str_index() {
    let v = eval_src("\"hello\"[1]").unwrap();
    assert_eq!(v, Value::Str("e".to_string()));
}

#[test]
fn index_non_indexable_errors() {
    let err = eval_src("x := 5\nx[0]").unwrap_err();
    assert!(
        err.message.contains("cannot index a value of type `int`"),
        "{}",
        err.message
    );
}

#[test]
fn array_slice() {
    let v = eval_src("scores := [10, 20, 30, 40]\nscores[1:3]").unwrap();
    assert_eq!(v, Value::Array(vec![Value::Int(20), Value::Int(30)]));
}

#[test]
fn slice_open_bounds() {
    assert_eq!(
        eval_src("scores := [10, 20, 30]\nscores[:2]").unwrap(),
        Value::Array(vec![Value::Int(10), Value::Int(20)])
    );
    assert_eq!(
        eval_src("scores := [10, 20, 30]\nscores[1:]").unwrap(),
        Value::Array(vec![Value::Int(20), Value::Int(30)])
    );
    assert_eq!(
        eval_src("scores := [10, 20, 30]\nscores[:]").unwrap(),
        Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
    );
}

#[test]
fn slice_negative_bounds() {
    let v = eval_src("\"hello\"[-2:]").unwrap();
    assert_eq!(v, Value::Str("lo".to_string()));
}

#[test]
fn slice_clamps_bounds() {
    let v = eval_src("scores := [10, 20, 30]\nscores[1:99]").unwrap();
    assert_eq!(v, Value::Array(vec![Value::Int(20), Value::Int(30)]));
}

#[test]
fn str_slice() {
    let v = eval_src("\"hello\"[1:3]").unwrap();
    assert_eq!(v, Value::Str("el".to_string()));
}

#[test]
fn slice_non_sliceable_errors() {
    let err = eval_src("x := 5\nx[1:2]").unwrap_err();
    assert!(
        err.message.contains("cannot slice a value of type `int`"),
        "{}",
        err.message
    );
}

#[test]
fn array_index_assign() {
    let v = eval_src("scores := [10, 20, 30]\nscores[0] = 99\nscores[0]").unwrap();
    assert_eq!(v, Value::Int(99));
}

#[test]
fn array_index_assign_negative() {
    let v = eval_src("scores := [10, 20, 30]\nscores[-1] = 99\nscores[2]").unwrap();
    assert_eq!(v, Value::Int(99));
}

#[test]
fn dict_index_assign_existing() {
    let v = eval_src("ages := {\"a\": 1}\nages[\"a\"] = 5\nages[\"a\"]").unwrap();
    assert_eq!(v, Value::Int(5));
}

#[test]
fn dict_index_assign_new_key() {
    let v = eval_src("ages := {\"a\": 1}\nages[\"b\"] = 2\nages[\"b\"]").unwrap();
    assert_eq!(v, Value::Int(2));
}

#[test]
fn str_index_assign_errors() {
    let err = eval_src("s := \"abc\"\ns[0] = \"x\"").unwrap_err();
    assert!(
        err.message
            .contains("cannot assign to an index of a string"),
        "{}",
        err.message
    );
}

#[test]
fn index_assign_through_field() {
    let v = eval_src(
        "struct Box { items: [int] }\nb := Box{ items: [1, 2, 3] }\nb.items[1] = 99\nb.items[1]",
    )
    .unwrap();
    assert_eq!(v, Value::Int(99));
}

#[test]
fn method_call() {
    let v = eval_src(
            "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }\np := Point { x: 3, y: 4 }\np.dist()",
        )
        .unwrap();
    assert_eq!(v, Value::Int(7));
}

#[test]
fn method_call_with_args() {
    let v = eval_src(
            "struct Point { x: int }\nfunc scale(p: Point, f: int) -> int { p.x * f }\np := Point { x: 3 }\np.scale(4)",
        )
        .unwrap();
    assert_eq!(v, Value::Int(12));
}

#[test]
fn method_call_undefined_errors() {
    let err = eval_src("struct Point { x: int }\np := Point { x: 1 }\np.nope()").unwrap_err();
    assert!(
        err.message.contains("undefined method `nope`"),
        "{}",
        err.message
    );
}

#[test]
fn method_call_on_field() {
    let v = eval_src(
            "struct Point { x: int }\nstruct Holder { p: Point }\nfunc dist(p: Point) -> int { p.x }\nh := Holder{ p: Point { x: 9 } }\nh.p.dist()",
        )
        .unwrap();
    assert_eq!(v, Value::Int(9));
}

#[test]
fn struct_init_with_space_runtime() {
    let v =
        eval_src("struct Point { x: int, y: int }\np := Point { x: 1, y: 2 }\np.x + p.y").unwrap();
    assert_eq!(v, Value::Int(3));
}

#[test]
fn pipe_inserts_lhs_as_first_arg() {
    let v = eval_src("func dbl(a: int, b: int) -> int { a * b }\n5 |> dbl(3)").unwrap();
    assert_eq!(v, Value::Int(15));
}

#[test]
fn pipe_bare_name() {
    let v = eval_src("func inc(n: int) -> int { n + 1 }\n5 |> inc").unwrap();
    assert_eq!(v, Value::Int(6));
}

#[test]
fn pipe_chain() {
    let v = eval_src(
        "func inc(n: int) -> int { n + 1 }\nfunc dbl(n: int) -> int { n * 2 }\n5 |> inc |> dbl",
    )
    .unwrap();
    assert_eq!(v, Value::Int(12));
}
