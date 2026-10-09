mod common;

use common::{
    check_src, check_src_with_funcs, check_src_with_funcs_and_structs, errors_contain, has_errors,
};
use std::collections::HashMap;
use zz_checker::type_::Type;
use zz_checker::{check_program, FuncSig, StructSig};
use zz_frontend::span::Span;

#[test]
fn infers_int_from_literal() {
    let r = check_src("x := 1");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

#[test]
fn infers_float_from_promotion() {
    let r = check_src("x := 1 + 2.5");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Float);
}

#[test]
fn annotation_unifies() {
    let r = check_src("x: float = 1 + 2.5");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Float);
}

#[test]
fn annotation_mismatch_errors() {
    errors_contain("x: str = 1 + 2", "type mismatch");
}

#[test]
fn type_mismatch_arithmetic() {
    errors_contain("1 + \"a\"", "cannot apply `+`");
}

#[test]
fn bool_ops_require_bool() {
    errors_contain("1 && true", "expected `bool`, found `int`");
}

#[test]
fn comparison_requires_same_type() {
    errors_contain("1 == \"a\"", "type mismatch");
}

#[test]
fn undefined_variable_errors() {
    errors_contain("nope + 1", "undefined variable `nope`");
}

#[test]
fn func_signature_and_body() {
    let r = check_src("func add(a: int, b: int) -> int { return a + b }\nz := add(1, 2)");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["z"], Type::Int);
}

#[test]
fn func_return_type_inferred() {
    let r = check_src("func five() { return 5 }\nz := five()");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["z"], Type::Int);
}

#[test]
fn func_wrong_return_type_errors() {
    errors_contain("func f() -> int { return \"a\" }", "type mismatch");
}

#[test]
fn wrong_arg_count_errors() {
    errors_contain(
        "func f(a: int) -> int { a }\nf(1, 2)",
        "takes 1 argument (a: int), found 2",
    );
}

#[test]
fn wrong_arg_type_errors() {
    errors_contain("func f(a: int) -> int { a }\nf(\"x\")", "type mismatch");
}

#[test]
fn generic_func_instantiates() {
    let r = check_src("func id<T>(x: T) -> T { return x }\na := id(1)\nb := id(\"s\")");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["a"], Type::Int);
    assert_eq!(r.bindings["b"], Type::Str);
}

#[test]
fn generic_func_monomorphic_fail() {
    errors_contain(
        "func id<T>(x: T) -> T { x }\nf := id",
        "cannot use generic function `id` as a value",
    );
}

// --- generic type bounds ------------------------------------------------

#[test]
fn generic_num_bound_arith_ok() {
    let r = check_src(
        "func add<T: Num>(x: T, y: T) -> T { return x + y }\na := add(2.1, 3.0)\nb := add(1, 2)",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["a"], Type::Float);
    assert_eq!(r.bindings["b"], Type::Int);
}

#[test]
fn generic_unbound_arith_errors_with_hint() {
    errors_contain(
        "func add<T>(x: T, y: T) -> T { return x + y }",
        "needs a `Num` bound for `+`",
    );
}

#[test]
fn generic_ord_bound_comparison_ok() {
    let r = check_src(
        "func min<T: Ord>(a: T, b: T) -> T { if a < b { a } else { b } }\nm := min(3, 7)",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["m"], Type::Int);
}

#[test]
fn generic_unbound_comparison_errors_with_hint() {
    errors_contain(
        "func min<T>(a: T, b: T) -> T { if a < b { a } else { b } }",
        "needs a `Ord` bound for `<`",
    );
}

#[test]
fn generic_eq_bound_ok() {
    let r = check_src("func same<T: Eq>(a: T, b: T) -> bool { return a == b }\ns := same(1, 1)");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["s"], Type::Bool);
}

#[test]
fn generic_unbound_eq_errors_with_hint() {
    errors_contain(
        "func same<T>(a: T, b: T) -> bool { return a == b }",
        "needs a `Eq` bound for `==`",
    );
}

#[test]
fn generic_multi_bound_ok() {
    let r = check_src(
        "func minmax<T: Num + Ord>(a: T, b: T) -> T { if a < b { a } else { b } }\nm := minmax(1.5, 2.5)",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["m"], Type::Float);
}

#[test]
fn generic_call_site_bound_violation() {
    errors_contain(
        "func min<T: Ord>(a: T, b: T) -> T { if a < b { a } else { b } }\nmin(true, false)",
        "does not satisfy bound `Ord`",
    );
}

#[test]
fn generic_mixed_int_float_still_rejects() {
    errors_contain(
        "func add<T: Num>(x: T, y: T) -> T { return x + y }\nadd(2.1, 3)",
        "type mismatch",
    );
}

#[test]
fn generic_distinct_params_reject_arith() {
    errors_contain(
        "func add<T: Num, U: Num>(x: T, y: U) -> T { return x + y }",
        "distinct generic parameters",
    );
}

#[test]
fn generic_unary_neg_needs_num() {
    errors_contain(
        "func neg<T>(x: T) -> T { return -x }",
        "needs a `Num` bound for `-`",
    );
}

#[test]
fn generic_unary_neg_with_num_ok() {
    let r = check_src("func neg<T: Num>(x: T) -> T { return -x }\nn := neg(5)");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["n"], Type::Int);
}

#[test]
fn generic_identity_still_works() {
    let r = check_src("func id<T>(x: T) -> T { return x }\na := id(1)\nb := id(\"s\")");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["a"], Type::Int);
    assert_eq!(r.bindings["b"], Type::Str);
}

#[test]
fn recursion_works() {
    let r = check_src("func fact(n: int) -> int { if n <= 1 { 1 } else { n * fact(n - 1) } }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

// --- structs -----------------------------------------------------------

#[test]
fn struct_def_and_init() {
    let r = check_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["p"], Type::Struct("Point".into(), vec![]));
    assert_eq!(r.structs["Point"].fields.len(), 2);
}

#[test]
fn struct_field_access() {
    let r = check_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\nz := p.x");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["z"], Type::Int);
}

#[test]
fn struct_field_mutation() {
    let r = check_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x = 10");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn struct_field_mutation_type_mismatch_errors() {
    errors_contain(
        "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x = \"a\"",
        "type mismatch",
    );
}

#[test]
fn struct_unknown_field_errors() {
    errors_contain(
        "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.z",
        "has no field `z`",
    );
}

#[test]
fn struct_embedding_promotes_field() {
    let r = check_src(
        "struct Base { id: int }\nstruct User { Base, age: int }\nu := User{ Base: Base{ id: 1 }, age: 2 }\nz := u.id",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["z"], Type::Int);
}

#[test]
fn struct_embedding_explicit_form_matches_shorthand() {
    // `Base: Base` is the explicit spelling of the embedded field `Base`.
    let r = check_src(
        "struct Base { id: int }\nstruct User { Base: Base, age: int }\nu := User{ Base: Base{ id: 1 }, age: 2 }\nz := u.id",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["z"], Type::Int);
}

#[test]
fn struct_embedding_shorthand_init() {
    let r = check_src(
        "struct Base { id: int }\nstruct User { Base, age: int }\nu := User{ Base{ id: 1 }, age: 2 }\nz := u.id",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["z"], Type::Int);
}

#[test]
fn struct_embedding_transitive_field() {
    let r = check_src(
        "struct Base { id: int }\nstruct Mid { Base, tag: int }\nstruct Outer { Mid, top: int }\no := Outer{ Mid: Mid{ Base: Base{ id: 1 }, tag: 2 }, top: 3 }\nz := o.id",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["z"], Type::Int);
}

#[test]
fn struct_embedding_direct_field_shadows_promoted() {
    let r = check_src(
        "struct Base { id: int }\nstruct User { Base, id: int }\nu := User{ Base: Base{ id: 1 }, id: 2 }\nz := u.id",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["z"], Type::Int);
}

#[test]
fn struct_embedding_promoted_method_call() {
    let r = check_src(
        "struct Base { id: int }\nimpl Base { func get(self) -> int { self.id } }\nstruct User { Base, age: int }\nu := User{ Base: Base{ id: 1 }, age: 2 }\nz := u.get()",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["z"], Type::Int);
}

#[test]
fn struct_embedding_direct_method_shadows_promoted() {
    let r = check_src(
        "struct Base { id: int }\nimpl Base { func who(self) -> str { \"base\" } }\nstruct User { Base, age: int }\nimpl User { func who(self) -> str { \"user\" } }\nu := User{ Base: Base{ id: 1 }, age: 2 }\nz := u.who()",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["z"], Type::Str);
}

#[test]
fn struct_embedding_unknown_field_still_errors() {
    errors_contain(
        "struct Base { id: int }\nstruct User { Base, age: int }\nu := User{ Base: Base{ id: 1 }, age: 2 }\nu.nope",
        "has no field `nope`",
    );
}

#[test]
fn struct_embedding_promoted_mutation() {
    let r = check_src(
        "struct Base { id: int }\nstruct User { Base, age: int }\nu := User{ Base: Base{ id: 1 }, age: 2 }\nu.id = 9",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn struct_embedding_suggests_promoted_field() {
    // Typo of a promoted field suggests the promoted name.
    let r = check_src(
        "struct Base { id: int }\nstruct User { Base, age: int }\nu := User{ Base: Base{ id: 1 }, age: 2 }\nu.ix",
    );
    let notes: Vec<String> = r.errors.iter().flat_map(|e| e.notes.clone()).collect();
    assert!(
        notes.iter().any(|n| n.contains("did you mean field `id`")),
        "expected promoted-field suggestion, got notes: {notes:?}, errors: {:?}",
        r.errors,
    );
}

#[test]
fn struct_embedding_flat_init() {
    // Flattened init: promoted fields nest into the embedded struct.
    let r = check_src(
        "struct Base { id: int, name: str }\nstruct User { Base, age: int }\nu := User{ id: 1, name: \"Zaid\", age: 19 }",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["u"], Type::Struct("User".into(), vec![]));
}

#[test]
fn struct_embedding_flat_init_type_mismatch_errors() {
    errors_contain(
        "struct Base { id: int }\nstruct User { Base, age: int }\nu := User{ id: \"x\", age: 2 }",
        "type mismatch",
    );
}

#[test]
fn struct_embedding_flat_init_missing_leaf_errors() {
    errors_contain(
        "struct Base { id: int, name: str }\nstruct User { Base, age: int }\nu := User{ id: 1, age: 2 }",
        "missing field `Base.name` in struct literal `User`",
    );
}

#[test]
fn struct_embedding_init_conflict_errors() {
    // Explicit embedded value + flattened leaves of the same subtree.
    errors_contain(
        "struct Base { id: int }\nstruct User { Base, age: int }\nu := User{ Base: Base{ id: 1 }, id: 2, age: 3 }",
        "conflicts with embedded value `Base`",
    );
}

#[test]
fn struct_unknown_field_in_init_errors() {
    errors_contain(
        "struct Point { x: int, y: int }\np := Point{ x: 1, z: 2 }",
        "has no field `z`",
    );
}

#[test]
fn struct_unknown_type_errors() {
    errors_contain("p := Nope{ x: 1 }", "unknown struct `Nope`");
}

#[test]
fn struct_in_func_signature() {
    let r = check_src(
        "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }\nz := dist(Point{ x: 1, y: 2 })",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["z"], Type::Int);
}

#[test]
fn struct_wrong_arg_type_errors() {
    errors_contain(
        "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x }\ndist(5)",
        "type mismatch",
    );
}

#[test]
fn struct_field_on_non_struct_errors() {
    errors_contain("x := 5\nx.y", "cannot access field `y`");
}

#[test]
fn struct_duplicate_definition_errors() {
    errors_contain(
        "struct A { x: int }\nstruct A { y: int }",
        "duplicate definition of struct `A`",
    );
}

#[test]
fn struct_type_annotation_resolves() {
    let r = check_src("struct Point { x: int, y: int }\np: Point = Point{ x: 1, y: 2 }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["p"], Type::Struct("Point".into(), vec![]));
}

// --- for loops ---------------------------------------------------------

#[test]
fn for_over_range() {
    let r = check_src("sum := 0\nfor i in 0..5 { sum = sum + i }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn for_over_array() {
    let r = check_src("total := 0\nfor n in [10, 20, 30] { total = total + n }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn for_loop_var_typed() {
    let r = check_src("for i in 0..5 { i }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn for_over_non_iterable_errors() {
    errors_contain("for i in 5 { i }", "cannot iterate a value of type `int`");
}

#[test]
fn for_loop_var_scope_does_not_leak() {
    errors_contain("for i in 0..5 { i }\ni", "undefined variable `i`");
}

#[test]
fn break_outside_loop_errors() {
    errors_contain("break", "`break` outside of a loop");
}

#[test]
fn continue_outside_loop_errors() {
    errors_contain("continue", "`continue` outside of a loop");
}

#[test]
fn break_inside_loop_ok() {
    let r = check_src("for i in 0..5 { if i == 2 { break } }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn break_inside_while_ok() {
    let r = check_src("x := 0\nwhile x < 5 { x = x + 1; break }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn range_bounds_must_be_int() {
    errors_contain("for i in 0.5..2.5 { i }", "range bounds must be `int`");
}

#[test]
fn assignment_to_undefined_errors() {
    errors_contain("nope = 5", "undefined variable `nope`");
}

#[test]
fn closure_inference() {
    let r = check_src("f := |x: int| x + 1");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(
        r.bindings["f"],
        Type::Func(vec![Type::Int], Box::new(Type::Int))
    );
}

#[test]
fn closure_ambiguous_errors() {
    errors_contain("f := |x| x", "cannot infer the type of `f`");
}

#[test]
fn calling_closure() {
    let r = check_src("f := |x: int| x + 1\ny := f(5)");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["y"], Type::Int);
}

#[test]
fn zero_arg_closure_no_space() {
    // #261: `|| body` parses as a zero-arg closure (no space needed).
    let r = check_src("f := || 42\ny := f()");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["y"], Type::Int);
}

#[test]
fn or_op_spacing_variants_stay_boolean_or() {
    // #261: every spacing of `||` after an operand stays boolean-or.
    for src in ["a||b", "a ||b", "a|| b", "a || b"] {
        let prog = format!("a := true\nb := false\nc := {src}");
        let r = check_src(&prog);
        assert!(!has_errors(&r), "{src}: {:?}", r.errors);
        assert_eq!(r.bindings["c"], Type::Bool, "{src}");
    }
}

#[test]
fn unused_tuple_binding_span_points_at_binding() {
    // #293: an unused destructured binding points at its own site, not 1:1.
    let src = "func pair() -> (int, int) {\n    return (1, 2)\n}\nfunc main() {\n    m, n := pair()\n    println(n)\n}\n";
    let r = check_src(src);
    let warn = r
        .errors
        .iter()
        .find(|d| d.message.contains("unused variable `m`"))
        .expect("expected unused-`m` warning");
    let span = warn.span.expect("warning must carry a span");
    assert_eq!(&src[span.to_range()], "m", "span {span:?} must cover `m`");
}

#[test]
fn unused_for_loop_var_span_points_at_binding() {
    // #293 (same class): unused loop variables point at their own site.
    let src = "func main() {\n    total := 0\n    for i, x in [(1, 10), (2, 20)] {\n        total = total + x\n    }\n}\n";
    let r = check_src(src);
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    let warn = r
        .errors
        .iter()
        .find(|d| d.message.contains("unused variable `i`"))
        .expect("expected unused-`i` warning");
    let span = warn.span.expect("warning must carry a span");
    assert_eq!(&src[span.to_range()], "i", "span {span:?} must cover `i`");
}

#[test]
fn match_option() {
    let r = check_src("v := .some(1)\nx := match v { .some(n) => n, .none => 0 }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

#[test]
fn match_result() {
    let r = check_src("v: Result<int, str> = .ok(1)\nx := match v { .ok(n) => n, .err(_) => 0 }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

#[test]
fn match_nonexhaustive_errors() {
    errors_contain("v := .some(1)\nmatch v { .some(n) => n }", "non-exhaustive");
}

#[test]
fn match_on_int_requires_wildcard() {
    errors_contain("match 5 { 1 => 1 }", "requires a `_` wildcard arm");
}

#[test]
fn match_arm_type_mismatch_errors() {
    errors_contain(
        "v := .some(1)\nmatch v { .some(n) => n, .none => \"x\" }",
        "type mismatch",
    );
}

#[test]
fn match_or_pattern_literals() {
    let r = check_src("x := match 2 { 1 | 2 => 10, _ => 0 }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

#[test]
fn match_or_pattern_in_variant_arg() {
    let r = check_src(
        "v := .some(\"yes\")\nx := match v { .some(\"done\" | \"yes\" | \"true\") => 1, .some(_) => 0, .none => 0 }",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

#[test]
fn match_or_pattern_bool_exhaustive() {
    let r = check_src("b := true\nx := match b { true | false => 1 }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn match_or_pattern_binding_mismatch_errors() {
    errors_contain(
        "v := .some(1)\nmatch v { .some(x) | .none => x, _ => 0 }",
        "same names",
    );
}

#[test]
fn match_break_arm_body() {
    let r = check_src("i := 0\nwhile true { i = i + 1\nmatch i { 3 => break, _ => 0 } }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn match_break_outside_loop_errors() {
    errors_contain("match 1 { 1 => break, _ => 0 }", "outside of a loop");
}

#[test]
fn if_let_binds() {
    let r = check_src("v := .some(5)\nx := if let .some(n) = v { n } else { 0 }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

#[test]
fn try_propagates_result() {
    let r = check_src(
        "func div(a: int, b: int) -> Result<int, str> { if b == 0 { .err(\"z\") } else { .ok(a / b) } }\nfunc f(a: int, b: int) -> Result<int, str> { q := div(a, b)?; .ok(q) }",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn try_on_option() {
    let r = check_src("func f() -> Option<int> { x := .some(1)?; .some(x) }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn try_outside_function_errors() {
    errors_contain(".ok(1)?", "only be used inside a function");
}

#[test]
fn try_on_plain_int_errors() {
    errors_contain(
        "func f() -> Result<int, str> { x := 5?; .ok(x) }",
        "cannot use `?` on a value of type `int`",
    );
}

#[test]
fn try_error_type_mismatch() {
    errors_contain(
        "func a() -> Result<int, str> { .ok(1) }\nfunc b() -> Result<int, int> { x := a()?; .ok(x) }",
        "no conversion path",
    );
}

#[test]
fn variant_type_inference() {
    let r = check_src("a := .ok(1)\nb := .none\nc := .err(\"boom\")");
    assert!(!has_errors(&r), "expected no errors, got {:?}", r.errors);
    errors_contain("f := |x| x", "cannot infer the type of `f`");
}

#[test]
fn return_outside_function_errors() {
    errors_contain("return 5", "`return` outside of a function");
}

#[test]
fn if_else_type_unify() {
    let r = check_src("x := if true { 1 } else { 2 }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

#[test]
fn if_else_mismatch_errors() {
    errors_contain("x := if true { 1 } else { \"a\" }", "type mismatch");
}

#[test]
fn if_condition_must_be_bool() {
    errors_contain("if 1 { 1 } else { 2 }", "expected `bool`");
}

#[test]
fn while_condition_must_be_bool() {
    errors_contain("while 1 { f() }", "expected `bool`");
}

#[test]
fn str_concat() {
    let r = check_src("s := \"a\" + \"b\"");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["s"], Type::Str);
}

#[test]
fn str_plus_int_errors() {
    errors_contain("s := \"a\" + 1", "cannot apply `+`");
}

#[test]
fn shadowing_allowed() {
    let r = check_src("x := 1\nx := x + 1");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

#[test]
fn duplicate_func_errors() {
    errors_contain("func f() { 1 }\nfunc f() { 2 }", "duplicate definition");
}

#[test]
fn array_literal_infers() {
    let r = check_src("scores := [10, 20, 30]");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["scores"], Type::Array(Box::new(Type::Int)));
}

#[test]
fn array_explicit_decl() {
    let r = check_src("scores: [int] = [10, 20, 30]");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["scores"], Type::Array(Box::new(Type::Int)));
}

#[test]
fn array_mixed_types_form_union() {
    let r = check_src("v := [1, \"a\"]");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(
        r.bindings["v"],
        Type::Array(Box::new(Type::Union(vec![Type::Int, Type::Str])))
    );
}

#[test]
fn array_annotation_unifies_with_union() {
    let r = check_src("v: [int] = [1, 2]");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["v"], Type::Array(Box::new(Type::Int)));
}

#[test]
fn array_type_mismatch_errors() {
    errors_contain("v: [int] = [\"a\"]", "type mismatch");
}

#[test]
fn array_union_member_accepted() {
    let r = check_src("v: [int] = [1, \"a\"]");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn empty_array_deferred_inference() {
    // Empty array without context: inference is deferred, no error.
    let r = check_src("v := []");
    assert!(
        !has_errors(&r),
        "empty array should not error (deferred inference): {:?}",
        r.errors
    );
}

#[test]
fn dict_literal_infers() {
    let r = check_src("ages := {\"Zaid\": 20}");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(
        r.bindings["ages"],
        Type::Dict(Box::new(Type::Str), Box::new(Type::Int))
    );
}

#[test]
fn dict_explicit_decl() {
    let r = check_src("ages: {str: int} = {\"a\": 1}");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(
        r.bindings["ages"],
        Type::Dict(Box::new(Type::Str), Box::new(Type::Int))
    );
}

#[test]
fn dict_union_value_type() {
    let r = check_src("user: {str: str | int} = {\"name\": \"Zaid\", \"age\": 20}");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(
        r.bindings["user"],
        Type::Dict(
            Box::new(Type::Str),
            Box::new(Type::Union(vec![Type::Str, Type::Int]))
        )
    );
}

#[test]
fn dict_key_mismatch_errors() {
    errors_contain("m: {str: int} = {1: 2}", "type mismatch");
}

#[test]
fn empty_dict_deferred_inference() {
    // Empty dict without context: inference is deferred, no error.
    let r = check_src("m := {}");
    assert!(
        !has_errors(&r),
        "empty dict should not error (deferred inference): {:?}",
        r.errors
    );
}

#[test]
fn import_is_noop() {
    let r = check_src("x := 1");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

#[test]
fn union_annotation_accepts_member() {
    let r = check_src("v: str | int = 5");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["v"], Type::Int);
}

#[test]
fn union_mismatch_errors() {
    errors_contain("v: str | int = true", "type mismatch");
}

// --- indexing & slicing -------------------------------------------------

#[test]
fn array_index_type() {
    let r = check_src("scores := [10, 20]\nx := scores[0]");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

#[test]
fn dict_index_type() {
    let r = check_src("ages := {\"a\": 1}\nx := ages[\"a\"]");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

#[test]
fn str_index_type() {
    let r = check_src("x := \"hello\"[1]");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Str);
}

#[test]
fn array_slice_type() {
    let r = check_src("scores := [10, 20, 30]\nx := scores[1:3]");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Array(Box::new(Type::Int)));
}

#[test]
fn str_slice_type() {
    let r = check_src("x := \"hello\"[1:3]");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Str);
}

#[test]
fn index_non_indexable_errors() {
    errors_contain("x := 5\nx[0]", "cannot index a value of type `int`");
}

#[test]
fn index_non_int_errors() {
    errors_contain("scores := [1, 2]\nscores[\"a\"]", "index must be `int`");
}

#[test]
fn slice_non_sliceable_errors() {
    errors_contain("x := 5\nx[1:2]", "cannot slice a value of type `int`");
}

#[test]
fn index_assign_type_checked() {
    let r = check_src("scores := [1, 2]\nscores[0] = 5");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn index_assign_wrong_type_errors() {
    errors_contain("scores := [1, 2]\nscores[0] = \"x\"", "type mismatch");
}

#[test]
fn str_index_assign_errors() {
    errors_contain(
        "s := \"abc\"\ns[0] = \"x\"",
        "cannot assign to an index of a string",
    );
}

#[test]
fn dict_index_assign_ok() {
    let r = check_src("ages := {\"a\": 1}\nages[\"b\"] = 2");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

// --- pipeline -----------------------------------------------------------

#[test]
fn pipe_type_checks() {
    let r = check_src("func dbl(a: int, b: int) -> int { a * b }\nx := 5 |> dbl(3)");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

#[test]
fn pipe_bare_name_type_checks() {
    let r = check_src("func inc(n: int) -> int { n + 1 }\nx := 5 |> inc");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

#[test]
fn pipe_type_mismatch_errors() {
    errors_contain(
        "func dbl(a: int, b: int) -> int { a * b }\nx := \"s\" |> dbl(3)",
        "type mismatch",
    );
}

#[test]
fn pipe_chain_type_checks() {
    let r = check_src(
        "func inc(n: int) -> int { n + 1 }\nfunc dbl(n: int) -> int { n * 2 }\nx := 5 |> inc |> dbl",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

// --- typeof -------------------------------------------------------------

#[test]
fn typeof_any_value() {
    let mut funcs = HashMap::new();
    funcs.insert(
        "typeof".to_string(),
        FuncSig {
            is_extern: false,
            extern_c_symbol: None,
            generics: vec!["T".to_string()],
            bounds: Vec::new(),
            params: vec![("v".to_string(), Type::Named("T".to_string()))],
            has_default: vec![],
            ret: Type::Str,
        },
    );
    for src in [
        "x := typeof(1)",
        "x := typeof(\"s\")",
        "x := typeof([1, 2])",
        "x := typeof({\"a\": 1})",
        "x := typeof(.some(1))",
    ] {
        let r = check_src_with_funcs(src, funcs.clone());
        assert!(!has_errors(&r), "errors for `{src}`: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Str, "for `{src}`");
    }
}

// --- method calls -------------------------------------------------------

fn method_funcs() -> HashMap<String, FuncSig> {
    let mut funcs = HashMap::new();
    funcs.insert(
        "dist".to_string(),
        FuncSig {
            is_extern: false,
            extern_c_symbol: None,
            generics: Vec::new(),
            bounds: Vec::new(),
            params: vec![
                ("p".to_string(), Type::Struct("Point".to_string(), vec![])),
                ("scale".to_string(), Type::Int),
            ],
            has_default: vec![],
            ret: Type::Int,
        },
    );
    funcs
}

#[test]
fn method_call_type_checks() {
    let r = check_src_with_funcs(
        "struct Point { x: int }\np := Point{ x: 3 }\nz := p.dist(2)",
        method_funcs(),
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["z"], Type::Int);
}

#[test]
fn method_call_receiver_mismatch_errors() {
    let r = check_src_with_funcs(
        "struct Point { x: int }\nstruct Line { a: int }\nl := Line{ a: 1 }\nz := l.dist(2)",
        method_funcs(),
    );
    assert!(
        r.errors.iter().any(|e| e.message.contains("type mismatch")),
        "errors: {:?}",
        r.errors
    );
}

#[test]
fn method_call_arg_mismatch_errors() {
    let r = check_src_with_funcs(
        "struct Point { x: int }\np := Point{ x: 3 }\nz := p.dist(\"s\")",
        method_funcs(),
    );
    assert!(
        r.errors.iter().any(|e| e.message.contains("type mismatch")),
        "errors: {:?}",
        r.errors
    );
}

#[test]
fn method_call_unknown_method_errors() {
    let r = check_src_with_funcs(
        "struct Point { x: int }\np := Point{ x: 3 }\nz := p.nope()",
        method_funcs(),
    );
    assert!(
        r.errors
            .iter()
            .any(|e| e.message.contains("no field `nope`")),
        "errors: {:?}",
        r.errors
    );
}

#[test]
fn method_call_namespaced_by_struct_type() {
    let mut funcs = HashMap::new();
    funcs.insert(
        "shapes.dist".to_string(),
        FuncSig {
            is_extern: false,
            extern_c_symbol: None,
            generics: Vec::new(),
            bounds: Vec::new(),
            params: vec![(
                "p".to_string(),
                Type::Struct("shapes.Point".to_string(), vec![]),
            )],
            has_default: vec![],
            ret: Type::Int,
        },
    );
    let mut structs = HashMap::new();
    structs.insert(
        "shapes.Point".to_string(),
        StructSig {
            generics: Vec::new(),
            fields: vec![("x".to_string(), Type::Int)],
        },
    );
    let r = check_src_with_funcs_and_structs(
        "p := shapes.Point{ x: 3 }\nz := p.dist()",
        funcs,
        structs,
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["z"], Type::Int);
}

// --- conversions --------------------------------------------------------

fn conv_funcs() -> HashMap<String, FuncSig> {
    let t = Type::Named("T".to_string());
    let mut funcs = HashMap::new();
    funcs.insert(
        "str".to_string(),
        FuncSig {
            is_extern: false,
            extern_c_symbol: None,
            generics: vec!["T".to_string()],
            bounds: Vec::new(),
            params: vec![("v".to_string(), t.clone())],
            has_default: vec![],
            ret: Type::Str,
        },
    );
    funcs.insert(
        "int".to_string(),
        FuncSig {
            is_extern: false,
            extern_c_symbol: None,
            generics: vec!["T".to_string()],
            bounds: Vec::new(),
            params: vec![("v".to_string(), t.clone())],
            has_default: vec![],
            ret: Type::Option(Box::new(Type::Int)),
        },
    );
    funcs.insert(
        "float".to_string(),
        FuncSig {
            is_extern: false,
            extern_c_symbol: None,
            generics: vec!["T".to_string()],
            bounds: Vec::new(),
            params: vec![("v".to_string(), t.clone())],
            has_default: vec![],
            ret: Type::Option(Box::new(Type::Float)),
        },
    );
    funcs
}

#[test]
fn conversion_sigs() {
    let r = check_src_with_funcs("a := str(1)\nb := int(\"42\")\nc := float(3)", conv_funcs());
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["a"], Type::Str);
    assert_eq!(r.bindings["b"], Type::Option(Box::new(Type::Int)));
    assert_eq!(r.bindings["c"], Type::Option(Box::new(Type::Float)));
}

#[test]
fn conversion_any_value() {
    for src in ["a := str([1, 2])", "a := int(3.7)", "a := float(\"2.5\")"] {
        let r = check_src_with_funcs(src, conv_funcs());
        assert!(!has_errors(&r), "errors for `{src}`: {:?}", r.errors);
    }
}

// --- smart diagnostics tests -------------------------------------------

#[test]
fn unused_variable_warning() {
    let r = check_src("x := 1");
    let msgs: Vec<_> = r.errors.iter().map(|e| e.message.as_str()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("unused variable")),
        "expected unused variable warning, got: {msgs:?}"
    );
}

#[test]
fn underscore_prefixed_no_warning() {
    let r = check_src("_x := 1");
    assert!(
        r.errors
            .iter()
            .all(|e| !e.message.contains("unused variable")),
        "underscore-prefixed should not warn: {:?}",
        r.errors
    );
}

#[test]
fn used_variable_no_warning() {
    let r = check_src("x := 1\ny := x + 1");
    let warns: Vec<String> = r
        .errors
        .iter()
        .filter(|e| e.severity == zz_frontend::diag::Severity::Warning)
        .map(|e| e.message.clone())
        .collect();
    assert!(
        !warns.iter().any(|m| m.contains("unused variable `x`")),
        "x should not be unused: {warns:?}"
    );
}

#[test]
fn typo_suggestion_variable() {
    let mut funcs = HashMap::new();
    funcs.insert(
        "println".to_string(),
        FuncSig {
            is_extern: false,
            extern_c_symbol: None,
            generics: vec![],
            bounds: Vec::new(),
            params: vec![("msg".to_string(), Type::Str)],
            has_default: vec![false],
            ret: Type::Unit,
        },
    );
    let parsed = zz_frontend::parse("prntlnn");
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let r = check_program(
        &parsed.program,
        HashMap::new(),
        funcs,
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );
    let notes: Vec<String> = r.errors.iter().flat_map(|e| e.notes.clone()).collect();
    let msgs: Vec<_> = r.errors.iter().map(|e| e.message.as_str()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("undefined")),
        "expected undefined variable error, got: {msgs:?}"
    );
    assert!(
        notes.iter().any(|n| n.contains("did you mean")),
        "expected typo suggestion, got: {notes:?}"
    );
}

#[test]
fn typo_suggestion_struct_field() {
    let r = check_src_with_funcs_and_structs(
        "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\nq := p.xz",
        HashMap::new(),
        {
            let mut s = HashMap::new();
            s.insert(
                "Point".to_string(),
                StructSig {
                    generics: Vec::new(),
                    fields: vec![("x".to_string(), Type::Int), ("y".to_string(), Type::Int)],
                },
            );
            s
        },
    );
    let notes: Vec<String> = r.errors.iter().flat_map(|e| e.notes.clone()).collect();
    assert!(
        notes.iter().any(|n| n.contains("did you mean")),
        "expected field suggestion, got: {notes:?}"
    );
}

fn print_test_funcs() -> HashMap<String, FuncSig> {
    let mut funcs = HashMap::new();
    funcs.insert(
        "println".to_string(),
        FuncSig {
            is_extern: false,
            extern_c_symbol: None,
            generics: vec![],
            bounds: Vec::new(),
            params: vec![("v".to_string(), Type::Str)],
            has_default: vec![false],
            ret: Type::Unit,
        },
    );
    funcs.insert(
        "env.os".to_string(),
        FuncSig {
            is_extern: false,
            extern_c_symbol: None,
            generics: vec![],
            bounds: Vec::new(),
            params: vec![],
            has_default: vec![],
            ret: Type::Str,
        },
    );
    funcs.insert(
        "env.temp_dir".to_string(),
        FuncSig {
            is_extern: false,
            extern_c_symbol: None,
            generics: vec![],
            bounds: Vec::new(),
            params: vec![],
            has_default: vec![],
            ret: Type::Str,
        },
    );
    funcs
}

#[test]
fn print_bare_function_is_call_hint() {
    let parsed = zz_frontend::parse("println(env.os)");
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let r = check_program(
        &parsed.program,
        HashMap::new(),
        print_test_funcs(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );
    let msgs: Vec<_> = r.errors.iter().map(|e| e.message.as_str()).collect();
    assert!(
        msgs.iter()
            .any(|m| m.contains("cannot print function `env.os`")),
        "expected call hint, got: {msgs:?}"
    );
    let notes: Vec<String> = r.errors.iter().flat_map(|e| e.notes.clone()).collect();
    assert!(
        notes.iter().any(|n| n.contains("env.os()`")),
        "expected () hint, got: {notes:?}"
    );
}

#[test]
fn typo_suggestion_dotted_path() {
    let parsed = zz_frontend::parse("println(env.tmp_dir())");
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let r = check_program(
        &parsed.program,
        HashMap::new(),
        print_test_funcs(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );
    let notes: Vec<String> = r.errors.iter().flat_map(|e| e.notes.clone()).collect();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("did you mean `env.temp_dir`")),
        "expected dotted suggestion, got: {notes:?}"
    );
}

#[test]
fn unclosed_paren_in_parser() {
    let parsed = zz_frontend::parse("func add(a: int, b: int) -> int {\n    a +\n");
    assert!(
        parsed.errors.iter().any(|e| e.message.contains("unclosed")),
        "expected unclosed delimiter error, got: {:?}",
        parsed.errors
    );
}

#[test]
fn mismatched_delimiter_in_parser() {
    let parsed = zz_frontend::parse("(1 + 2]");
    let msgs: Vec<_> = parsed.errors.iter().map(|e| e.message.as_str()).collect();
    assert!(
        msgs.iter()
            .any(|m| m.contains("unexpected") || m.contains("unclosed")),
        "expected mismatched delimiter error, got: {msgs:?}"
    );
}

#[test]
fn struct_init_marks_import_used() {
    // Regression: `ml.Circle{...}` must count as a use of `ml`, so a
    // namespaced struct init does not trigger a spurious unused-import warning.
    let r = check_src_with_funcs_and_structs(
        "import math_lib as ml\nc := ml.Circle{rad: 10}\nc",
        HashMap::new(),
        {
            let mut s = HashMap::new();
            s.insert(
                "ml.Circle".to_string(),
                StructSig {
                    generics: Vec::new(),
                    fields: vec![("rad".to_string(), Type::Int)],
                },
            );
            s
        },
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    let warns: Vec<String> = r
        .errors
        .iter()
        .filter(|e| e.severity == zz_frontend::diag::Severity::Warning)
        .map(|e| e.message.clone())
        .collect();
    assert!(
        !warns.iter().any(|m| m.contains("unused import")),
        "unused import should not be warned: {warns:?}"
    );
}

#[test]
fn pub_before_impl_is_parse_error() {
    // `pub impl` is rejected with a hint to remove `pub` — impl methods are
    // always public.
    let parsed = zz_frontend::parse("pub impl Point {\n    func dist(self) -> int { self.x }\n}");
    let msgs: Vec<String> = parsed
        .errors
        .iter()
        .map(|e| format!("{}\n{}", e.message, e.notes.join("\n")))
        .collect();
    assert!(
        msgs.iter()
            .any(|m| m.contains("cannot use `pub` on `impl`")),
        "expected `pub` on `impl` error, got: {msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.contains("remove the `pub` keyword")),
        "expected hint to remove `pub`, got: {msgs:?}"
    );
}

#[test]
fn pub_func_inside_impl_is_allowed() {
    // `pub func` inside an `impl` block is the supported way to export a
    // method cross-module.
    let parsed = zz_frontend::parse("impl Point {\n    pub func dist(self) -> int { self.x }\n}");
    assert!(
        parsed.errors.is_empty(),
        "expected pub func inside impl to parse, got: {:?}",
        parsed.errors
    );
}

#[test]
fn impl_method_without_pub_checkable() {
    // Plain `impl` (no `pub` keyword) with struct methods must type-check.
    let r = check_src(
        "struct Point { x: int, y: int }\n\
         impl Point {\n\
         \x20   func dist(self) -> int { self.x + self.y }\n\
         }\n\
         p := Point{ x: 1, y: 2 }\n\
         z := p.dist()",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["z"], Type::Int);
}

#[test]
fn impl_method_export_requires_pub() {
    // Only `pub` methods are exported to the cross-module seed. Non-pub
    // methods stay visible within the module but absent from `pub_funcs`.
    let r = check_src(
        "struct Point { x: int, y: int }\n\
         impl Point {\n\
         \x20   func priv_m(self) -> int { self.x }\n\
         \x20   pub func pub_m(self) -> int { self.y }\n\
         }\n\
         p := Point{ x: 1, y: 2 }\n\
         a := p.priv_m()\n\
         b := p.pub_m()",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert!(
        r.funcs.contains_key("Point.priv_m"),
        "private method must be registered for in-module calls"
    );
    assert!(
        !r.pub_funcs.contains_key("Point.priv_m"),
        "private method must not be exported"
    );
    assert!(
        r.pub_funcs.contains_key("Point.pub_m"),
        "pub method must be exported"
    );
}

#[test]
fn fixit_structure_is_populated() {
    use zz_frontend::diag::FixIt;
    let fixit = FixIt::safe(Span::new(0, 5), "_x", "rename to");
    assert_eq!(fixit.replacement, "_x");
    assert_eq!(fixit.message, "rename to");
}

// --- const (immutable variables) -------------------------------------------

#[test]
fn const_decl_type_checks() {
    let r = check_src("const x = 10");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

#[test]
fn const_explicit_type_checks() {
    let r = check_src("const x: float = 1.5");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Float);
}

#[test]
fn cannot_reassign_const() {
    errors_contain(
        "const x = 10\nx = 20",
        "cannot assign to immutable variable `x`",
    );
}

#[test]
fn cannot_reassign_const_explicit() {
    errors_contain(
        "const x: int = 10\nx = 20",
        "cannot assign to immutable variable `x`",
    );
}

#[test]
fn const_error_has_hint() {
    let r = check_src("const x = 10\nx = 20");
    let diag = r
        .errors
        .iter()
        .find(|e| e.message.contains("cannot assign to immutable variable"))
        .unwrap();
    assert!(
        diag.notes
            .iter()
            .any(|n| n.contains("remove `const` to make `x` mutable")),
        "missing hint, got: {:?}",
        diag.notes,
    );
}

#[test]
fn const_error_has_secondary_label() {
    let r = check_src("const x = 10\nx = 20");
    let diag = r
        .errors
        .iter()
        .find(|e| e.message.contains("cannot assign to immutable variable"))
        .unwrap();
    let sec = diag
        .secondary
        .as_ref()
        .expect("const error must carry a secondary label");
    assert_eq!(sec.message, "variable defined as immutable here");
    // The secondary span points at the `x` in `const x = 10` (byte 6).
    assert_eq!(sec.span.start, 6);
}

#[test]
fn can_reassign_mutable() {
    let r = check_src("x := 10\nx = 20");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn const_in_function_scope() {
    errors_contain(
        "func f() -> int { const y = 1\ny = 2\nreturn y }",
        "cannot assign to immutable variable `y`",
    );
}

#[test]
fn mutable_shadow_of_const_is_reassignable() {
    // The inner `x := 5` shadows the outer const; reassigning the shadow is
    // legal even though an outer `x` is immutable.
    let r = check_src(
        "const x = 10\n\
         func f() -> int {\n\
         \x20   x := 5\n\
         \x20   x = 6\n\
         \x20   return x\n\
         }",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn const_shadowed_by_const_still_immutable() {
    errors_contain(
        "const x = 10\n\
         func f() -> int {\n\
         \x20   const x = 5\n\
         \x20   x = 6\n\
         \x20   return x\n\
         }",
        "cannot assign to immutable variable `x`",
    );
}

#[test]
fn const_can_be_read() {
    let r = check_src("const x = 10\ny := x + 1");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["y"], Type::Int);
}

#[test]
fn const_closure_capture() {
    let r = check_src("const x = 10\nf := | | x\nz := f()");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

fn opaque_test_funcs() -> HashMap<String, FuncSig> {
    let mut funcs = HashMap::new();
    funcs.insert(
        "regex.compile".to_string(),
        FuncSig {
            is_extern: false,
            extern_c_symbol: None,
            generics: Vec::new(),
            bounds: Vec::new(),
            params: vec![("pat".to_string(), Type::Str)],
            has_default: vec![],
            ret: Type::Opaque("regex".to_string()),
        },
    );
    funcs.insert(
        "regex.is_match".to_string(),
        FuncSig {
            is_extern: false,
            extern_c_symbol: None,
            generics: Vec::new(),
            bounds: Vec::new(),
            params: vec![
                ("re".to_string(), Type::Opaque("regex".to_string())),
                ("s".to_string(), Type::Str),
            ],
            has_default: vec![],
            ret: Type::Bool,
        },
    );
    funcs
}

#[test]
fn opaque_handle_method_dispatch() {
    let r = check_src_with_funcs(
        "r := regex.compile(\"[a-z]+\")\nok := r.is_match(\"abc\")",
        opaque_test_funcs(),
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["r"], Type::Opaque("regex".to_string()));
    assert_eq!(r.bindings["ok"], Type::Bool);
}

#[test]
fn opaque_handle_tag_mismatch_errors() {
    // A `uuid` handle passed where a `regex` handle is expected.
    let mut funcs = opaque_test_funcs();
    funcs.insert(
        "uuid.v4".to_string(),
        FuncSig {
            is_extern: false,
            extern_c_symbol: None,
            generics: Vec::new(),
            bounds: Vec::new(),
            params: vec![],
            has_default: vec![],
            ret: Type::Opaque("uuid".to_string()),
        },
    );
    let r = check_src_with_funcs("u := uuid.v4()\nok := regex.is_match(u, \"abc\")", funcs);
    assert!(has_errors(&r), "expected tag mismatch, got {:?}", r.errors);
}

#[test]
fn nested_func_rejected() {
    // Nested named func declarations should produce a clean error, not a panic.
    errors_contain(
        "func outer() {\n  func inner() {  }\n  inner()\n}",
        "nested function `inner` is not supported",
    );
}

#[test]
fn http_request_fields_typecheck() {
    // Typed request: method/path/body are str, headers/query/params dicts.
    let r = check_src(
        "func gb(req: http.request) -> str { return req.body }\nfunc gm(req: http.request) -> str { return req.method }\nfunc gq(req: http.request) -> {str: str} { return req.query }",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.funcs["gb"].ret, Type::Str);
    assert_eq!(r.funcs["gm"].ret, Type::Str);
    assert_eq!(
        r.funcs["gq"].ret,
        Type::Dict(Box::new(Type::Str), Box::new(Type::Str))
    );
}

#[test]
fn http_request_unknown_field_errors() {
    errors_contain(
        "func h(req: http.request) {\n  x := req.nope\n}",
        "http.request has no field `nope`",
    );
}

#[test]
fn opaque_handle_types_resolve_in_annotations() {
    // Regression: opaque handle types (chan, task.join, http.server,
    // tcp.stream, tcp.listener, http.response, http.request) were valid Types but had
    // no name resolution — `func w(c: chan)` failed with "unknown type".
    // Same gap class as json/db, fixed alongside.
    for (ann, want) in [
        ("chan", Type::Chan),
        ("task.join", Type::TaskJoin),
        ("http.server", Type::HttpServer),
        ("tcp.stream", Type::TcpStream),
        ("tcp.listener", Type::TcpListener),
        ("http.response", Type::Response),
        ("http.request", Type::HttpRequest),
    ] {
        let src = format!("func w(c: {ann}) {{ }}\n");
        let r = check_src(&src);
        assert!(!has_errors(&r), "{ann}: errors: {:?}", r.errors);
        let sig = r
            .funcs
            .get("w")
            .unwrap_or_else(|| panic!("{ann}: func w missing"));
        assert_eq!(sig.params.len(), 1, "{ann}: arity");
        assert_eq!(sig.params[0].1, want, "{ann}: param type");
    }
}

// --- `return` divergence (`Type::Never`) -----------------------------------
// A `return` with a value diverges: it types as `Never`, which vanishes
// from if/match joins without constraining sibling arms — while the
// function's fall-through type is still verified against its signature.
// These tests pin both sides: wrong-typed value paths are rejected even
// when a sibling arm returns, and valid early-return shapes keep working.

#[test]
fn return_diverges_if_fallthrough_mismatch_rejected() {
    // n1: the `else` value path must match the return type even though
    // the `then` arm diverges via `return`.
    errors_contain(
        "func f(c: bool) -> int {\n    if c {\n        return 1\n    } else {\n        \"hi\"\n    }\n}",
        "type mismatch",
    );
}

#[test]
fn return_diverges_missing_else_rejected() {
    // n2: no `else` means the fall-through path yields unit, which a
    // non-unit function must reject (also closes the pre-existing hole
    // where any `return` anywhere skipped the body check entirely).
    errors_contain(
        "func f(c: bool) -> int {\n    if c {\n        return 1\n    }\n}",
        "type mismatch",
    );
}

#[test]
fn return_diverges_match_fallthrough_mismatch_rejected() {
    // n7: match version of n1 — the value arm must match the return type
    // even though the other arm diverges.
    errors_contain(
        "func f(x: int) -> int {\n    match x {\n        0 => {\n            return 10\n        },\n        _ => {\n            \"negative\"\n        },\n    }\n}",
        "type mismatch",
    );
}

#[test]
fn return_diverges_annotated_let_rejected() {
    // n8: even an explicit annotation on the join must be enforced —
    // `Never` must not absorb the declared type.
    errors_contain(
        "func f(c: bool) -> int {\n    x: int = if c { return 1 } else { \"hi\" }\n    x\n}",
        "type mismatch",
    );
}

#[test]
fn return_value_still_checked() {
    // The returned value itself is validated against the signature.
    errors_contain(
        "func f(c: bool) -> str {\n    if c {\n        return 1\n    } else {\n        \"hi\"\n    }\n}",
        "type mismatch",
    );
}

#[test]
fn return_in_match_arm_value_still_checked() {
    // n4: errors inside the returned expression are still reported.
    errors_contain(
        "struct E { msg: str }\nfunc f_inner(x: int) -> Result<int, E> {\n    .ok(x)\n}\nfunc f(x: int) -> Result<int, E> {\n    r := f_inner(x)\n    match r {\n        .ok(v) => {\n            return .ok(\"not an int\")\n        },\n        .err(e) => {\n            return .err(e)\n        },\n    }\n}",
        "type mismatch",
    );
}

#[test]
fn all_paths_diverge_accepted() {
    // n6: every path returns — the body types as `Never`, which checks
    // against any signature.
    let r = check_src(
        "func f(c: bool) -> int {\n    if c {\n        return 1\n    } else {\n        return 2\n    }\n}",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn divergent_arms_do_not_constrain_siblings() {
    // Sibling value arms still constrain each other (n9 shape): the
    // divergent `then` arm must not mask the "mid"/3 mismatch.
    errors_contain(
        "func f(c: bool, d: bool) -> int {\n    if c {\n        return 1\n    } else if d {\n        \"mid\"\n    } else {\n        3\n    }\n}",
        "type mismatch",
    );
}

#[test]
fn match_return_in_if_accepted() {
    // Original bug 2 shape: match with returns nested in `if`, falling
    // through to a tail value of the right type.
    let r = check_src(
        "struct E { msg: str }\nfunc inner(x: int) -> Result<int, E> {\n    if x < 0 {\n        return .err(E{msg: \"neg\"})\n    }\n    .ok(x)\n}\nfunc outer(x: int) -> Result<int, E> {\n    if x > 100 {\n        r := inner(x)\n        match r {\n            .ok(v) => {\n                return .ok(v + 1)\n            },\n            .err(e) => {\n                return .err(e)\n            },\n        }\n    }\n    .ok(x)\n}",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn trailing_if_in_else_accepted() {
    // Original bug 4 shape: trailing guard-`if` with returns as an else
    // tail, falling through to a tail value of the right type.
    let r = check_src(
        "struct E { msg: str }\nfunc f(k: str, n: int) -> Result<int, E> {\n    cur := 0\n    if k == \"table\" {\n        cur = 1\n    } else {\n        if k != \"array\" {\n            return .err(E{msg: \"bad\"})\n        }\n        cur = n\n        if cur > 10 {\n            return .err(E{msg: \"big\"})\n        }\n    }\n    .ok(cur)\n}",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn unused_warnings_emit_in_source_order() {
    // Unused-variable warnings must be source-ordered, not HashMap
    // order: the dual-engine parity harness diffs stderr across two
    // separate `zz` processes (different hash seeds). Names are chosen
    // so alphabetical order disagrees with source order.
    let r = check_src("func f() {\n    zzz := 1\n    aaa := 2\n}");
    let warns: Vec<&str> = r
        .errors
        .iter()
        .filter(|e| e.severity == zz_frontend::diag::Severity::Warning)
        .map(|e| e.message.as_str())
        .collect();
    assert_eq!(warns.len(), 2, "warnings: {:?}", r.errors);
    assert!(
        warns[0].contains("zzz"),
        "first warning should name `zzz`: {warns:?}"
    );
    assert!(
        warns[1].contains("aaa"),
        "second warning should name `aaa`: {warns:?}"
    );
}

#[test]
fn bitwise_ops_require_int_operands() {
    for src in [
        "x := 6 & 3",
        "x := 6 | 3",
        "x := 6 ^ 3",
        "x := 1 << 4",
        "x := 16 >> 2",
        "x := ~6",
    ] {
        let r = check_src(src);
        assert!(!has_errors(&r), "{src} errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int, "{src}");
    }
}

#[test]
fn bitwise_float_operand_errors() {
    errors_contain("x := 1.5 & 2", "bitwise `&` requires `int` operands");
    errors_contain("x := 1 | 2.5", "bitwise `|` requires `int` operands");
    errors_contain("x := 1.0 ^ 2.0", "bitwise `^` requires `int` operands");
    errors_contain("x := 1.5 << 2", "bitwise `<<` requires `int` operands");
}

#[test]
fn bitwise_bool_operand_errors() {
    errors_contain("x := true & false", "bitwise `&` requires `int` operands");
    errors_contain("x := true | false", "bitwise `|` requires `int` operands");
    errors_contain("x := ~true", "bitwise `~` requires an `int` operand");
}

#[test]
fn bitwise_result_flows_into_int_context() {
    let r = check_src("x := (6 & 3) + (1 << 4)\ny: int = x");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["x"], Type::Int);
}

#[test]
fn tuple_index_reads_element_types() {
    let r = check_src("t := (1, \"two\", 3.5)\na := t[0]\nb := t[1]\nc := t[2]");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["a"], Type::Int);
    assert_eq!(r.bindings["b"], Type::Str);
    assert_eq!(r.bindings["c"], Type::Float);
}

#[test]
fn tuple_negative_index_counts_from_end() {
    let r = check_src("t := (1, \"two\")\na := t[-1]");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["a"], Type::Str);
}

#[test]
fn tuple_index_out_of_bounds_errors() {
    errors_contain("t := (1, 2)\nx := t[5]", "out of bounds");
    errors_contain("t := (1, 2)\nx := t[-3]", "out of bounds");
}

#[test]
fn tuple_dynamic_index_errors_with_hint() {
    errors_contain(
        "t := (1, 2)\ni := 0\nx := t[i]",
        "must be an integer literal",
    );
}

#[test]
fn tuple_index_write_checks_element_type() {
    let r = check_src("t := (1, \"two\")\nt[0] = 99");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    errors_contain("t := (1, \"two\")\nt[0] = \"s\"", "type mismatch");
}

#[test]
fn bare_destructure_binds_names() {
    let r = check_src("a, b := (1, \"two\")");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["a"], Type::Int);
    assert_eq!(r.bindings["b"], Type::Str);
}

#[test]
fn compound_assign_valid_combinations() {
    for src in [
        "x := 1\nx += 2",
        "x := 1\nx -= 2",
        "x := 1\nx *= 2",
        "x := 8\nx /= 2",
        "x := 8\nx %= 3",
        "x := 2\nx **= 10",
        "x := 1.5\nx += 2.5",
        "x := 6\nx &= 3",
        "x := 6\nx |= 3",
        "x := 1\nx <<= 4",
        "s := \"a\"\ns += \"b\"",
    ] {
        let r = check_src(src);
        assert!(!has_errors(&r), "{src} errors: {:?}", r.errors);
    }
}

#[test]
fn compound_assign_matches_binary_op_rules() {
    // Whatever `x = x OP y` rejects, `x OP= y` rejects identically.
    errors_contain("x := 1\nx += 1.5", "type mismatch");
    errors_contain("x := 1\nx &= 1.5", "requires `int` operands");
    errors_contain("x := true\nx |= false", "requires `int` operands");
    errors_contain("x := \"a\"\nx -= \"b\"", "cannot apply");
    errors_contain("nope += 1", "undefined variable");
}

#[test]
fn compound_assign_rejects_const() {
    errors_contain("const x = 1\nx += 2", "immutable variable");
}

#[test]
fn compound_assign_field_and_index() {
    let r = check_src("struct P { x: int }\np := P{ x: 1 }\np.x += 2");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    let r = check_src("a := [1, 2]\na[0] *= 3");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    errors_contain("a := [1]\na[0] += \"s\"", "cannot apply");
}

#[test]
fn generic_struct_infers_from_literal() {
    let r = check_src("struct Box<T> { v: T }\nb := Box{ v: 42 }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["b"], Type::Struct("Box".into(), vec![Type::Int]));
    let r = check_src("struct Box<T> { v: T }\ns := Box{ v: \"hi\" }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["s"], Type::Struct("Box".into(), vec![Type::Str]));
}

#[test]
fn generic_struct_annotation_checked() {
    let r = check_src("struct Box<T> { v: T }\nx: Box<int> = Box{ v: 1 }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    errors_contain(
        "struct Box<T> { v: T }\ns: Box<str> = Box{ v: 1 }",
        "type mismatch",
    );
}

#[test]
fn generic_struct_field_access_substitutes() {
    let r = check_src("struct Box<T> { v: T }\nb := Box{ v: 42 }\nx: int = b.v");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    errors_contain(
        "struct Box<T> { v: T }\nb := Box{ v: 42 }\nx: str = b.v",
        "type mismatch",
    );
}

#[test]
fn generic_struct_multi_param() {
    let r = check_src("struct Pair<A, B> { a: A, b: B }\np := Pair{ a: 1, b: \"s\" }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(
        r.bindings["p"],
        Type::Struct("Pair".into(), vec![Type::Int, Type::Str])
    );
}

#[test]
fn generic_struct_nested() {
    let r = check_src("struct Box<T> { v: T }\nn := Box{ v: Box{ v: 1 } }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(
        r.bindings["n"],
        Type::Struct(
            "Box".into(),
            vec![Type::Struct("Box".into(), vec![Type::Int])]
        )
    );
}

#[test]
fn generic_struct_methods() {
    let r = check_src(
        "struct Box<T> { v: T }\nimpl Box<T> { func get(self) -> T { self.v } }\nb := Box{ v: 42 }\nx: int = b.get()",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    errors_contain(
        "struct Box<T> { v: T }\nimpl Box<T> { func get(self) -> T { self.v } }\nb := Box{ v: 42 }\nx: str = b.get()",
        "type mismatch",
    );
}

#[test]
fn generic_struct_arity_errors() {
    errors_contain(
        "struct P { x: int }\np: P<int> = P{ x: 1 }",
        "takes 0 type arguments",
    );
    errors_contain(
        "struct Box<T> { v: T }\nb: Box = Box{ v: 1 }",
        "takes 1 type argument",
    );
    errors_contain(
        "struct Pair<A, B> { a: A, b: B }\np: Pair<int> = Pair{ a: 1, b: 2 }",
        "takes 2 type arguments",
    );
}

#[test]
fn generic_struct_impl_mismatch() {
    errors_contain(
        "struct Box<T> { v: T }\nimpl Box { func get(self) -> int { self.v } }",
        "expected `impl",
    );
    errors_contain(
        "struct Box<T> { v: T }\nstruct Box<T> { w: T }",
        "duplicate definition of struct",
    );
    errors_contain("struct Box<T, T> { v: T }", "duplicate type parameter");
}

#[test]
fn plain_struct_unaffected_by_generics() {
    let r = check_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }");
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    assert_eq!(r.bindings["p"], Type::Struct("Point".into(), vec![]));
}

#[test]
fn cross_module_inferred_ret_resolves_on_export() {
    // Regression: `pub func ping()` with no return annotation exported its
    // return type as a bare `Var(0)`. The importing module's fresh unifier
    // then reused id 0 for an unrelated local (e.g. an empty `[]` element
    // var), unifying `unit` with `str` and rejecting a valid program.
    // Export now deep-resolves (ret becomes `unit`) and the importer
    // offsets fresh ids above seeded ones.
    let dep = check_src("pub func ping() {}");
    assert!(!has_errors(&dep), "errors: {:?}", dep.errors);
    let ping = dep.pub_funcs.get("ping").expect("pub ping").clone();
    assert_eq!(ping.ret, Type::Unit, "exported ret must resolve to unit");
    let mut seed = HashMap::new();
    seed.insert("n.ping".to_string(), ping);
    let main = "func main() -> [str] {\n    stop := false\n    if stop {\n        n.ping()\n    }\n    []\n}";
    let r = check_src_with_funcs(main, seed);
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn cross_module_nonunit_block_still_rejected() {
    // A single-branch `if` whose body yields a non-unit value is still an
    // error — including for cross-module calls. Only the spurious
    // var-collision failure above was fixed.
    let dep = check_src("pub func zstr() -> str { \"s\" }");
    assert!(!has_errors(&dep), "errors: {:?}", dep.errors);
    let zstr = dep.pub_funcs.get("zstr").expect("pub zstr").clone();
    let mut seed = HashMap::new();
    seed.insert("o.zstr".to_string(), zstr);
    let main = "func main() -> [str] {\n    out: [str] = []\n    stop := false\n    if stop {\n        o.zstr()\n    }\n    out\n}";
    let r = check_src_with_funcs(main, seed);
    let errs: Vec<String> = r.errors.iter().map(|e| e.message.clone()).collect();
    assert!(
        errs.iter().any(|e| e.contains("type mismatch")),
        "expected a type-mismatch error, got: {errs:?}"
    );
}

fn db_exec_seed() -> HashMap<String, FuncSig> {
    let mut funcs = HashMap::new();
    let exec = FuncSig {
        generics: vec![],
        bounds: vec![],
        params: vec![("db".to_string(), Type::Db), ("sql".to_string(), Type::Str)],
        has_default: vec![false, false],
        ret: Type::Int,
        is_extern: false,
        extern_c_symbol: None,
    };
    funcs.insert("sqlz.exec".to_string(), exec.clone());
    funcs.insert("db.exec".to_string(), exec);
    funcs.insert(
        "sqlz.open".to_string(),
        FuncSig {
            generics: vec![],
            bounds: vec![],
            params: vec![("path".to_string(), Type::Str)],
            has_default: vec![false],
            ret: Type::Db,
            is_extern: false,
            extern_c_symbol: None,
        },
    );
    funcs.insert(
        "println".to_string(),
        FuncSig {
            generics: vec![],
            bounds: vec![],
            params: vec![("v".to_string(), Type::Str)],
            has_default: vec![false],
            ret: Type::Unit,
            is_extern: false,
            extern_c_symbol: None,
        },
    );
    funcs
}

#[test]
fn local_db_method_call_reads_as_method() {
    // `db.exec(sql)` with a *local* `db` is a method call even though
    // `db.exec` also names a seeded free function: the free-function
    // reading (full arity) is already impossible, and the method reading
    // fits with a matching receiver. A top-level `db` keeps working,
    // as does the explicit static form.
    let seed = db_exec_seed();
    let local = "func main.main() {\n    db := sqlz.open(\":memory:\")\n    n := db.exec(\"CREATE TABLE t(v INTEGER)\")\n    println(\"exec={n}\")\n}";
    let r = check_src_with_funcs(local, seed.clone());
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
    let explicit = "func main.main() {\n    db := sqlz.open(\":memory:\")\n    n := db.exec(db, \"SELECT 1\")\n    println(\"exec={n}\")\n}";
    let r = check_src_with_funcs(explicit, seed);
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn enum_construction_and_match() {
    let r = check_src_with_funcs(
        "enum Token { Eof, IntLit(int) }\nfunc f(t: Token) -> str {\n    match t {\n        .Eof => \"eof\",\n        .IntLit(_v) => \"int\",\n    }\n}\nfunc main() {\n    t := Token.IntLit(1)\n    println(f(t))\n    println(f(Token.Eof))\n}\n",
        print_test_funcs(),
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn enum_nonexhaustive_reports() {
    errors_contain(
        "enum Token { Eof, IntLit(int) }\nfunc f(t: Token) -> str {\n    match t {\n        .Eof => \"eof\",\n    }\n}\n",
        "non-exhaustive match",
    );
}

#[test]
fn enum_unknown_variant_reports() {
    errors_contain(
        "enum Token { Eof }\nfunc main() {\n    t := Token.Num\n    println(t)\n}\n",
        "unknown variant `Num`",
    );
}

#[test]
fn enum_payload_arity_reports() {
    errors_contain(
        "enum Token { Eof, IntLit(int) }\nfunc main() {\n    t := Token.Eof(1)\n    println(t)\n}\n",
        "takes no arguments",
    );
    errors_contain(
        "enum Token { Eof, IntLit(int) }\nfunc main() {\n    t: Token = Token.IntLit\n    println(t)\n}\n",
        "holds a value",
    );
}

#[test]
fn enum_payload_type_mismatch_reports() {
    errors_contain(
        "enum Token { IntLit(int) }\nfunc main() {\n    t := Token.IntLit(\"s\")\n    println(t)\n}\n",
        "mismatch",
    );
}

#[test]
fn enum_duplicate_reports() {
    errors_contain(
        "enum Token { Eof }\nenum Token { Eof }\nfunc main() {\n    println(\"x\")\n}\n",
        "duplicate definition of enum",
    );
}

#[test]
fn enum_impl_method_resolves() {
    let r = check_src_with_funcs(
        "enum Token { Eof, IntLit(int) }\nimpl Token {\n    func is_eof(self) -> bool {\n        match self {\n            .Eof => true,\n            _ => false,\n        }\n    }\n}\nfunc main() {\n    _b := Token.Eof.is_eof()\n}\n",
        print_test_funcs(),
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn enum_generic_inference() {
    let r = check_src_with_funcs(
        "enum Box<T> { V(T), E }\nfunc main() {\n    a := Box.V(42)\n    b: Box<int> = Box.E\n    _c := (a == Box.V(42))\n    println(\"ok\")\n}\n",
        print_test_funcs(),
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn enum_generic_pattern_binds_payload() {
    let r = check_src(
        "enum Box<T> { V(T), E }\nfunc f<T>(b: Box<T>) -> T {\n    match b {\n        .V(v) => v,\n        .E => f(Box.E),\n    }\n}\n",
    );
    assert!(!has_errors(&r), "errors: {:?}", r.errors);
}

#[test]
fn enum_generic_mismatch_reports() {
    errors_contain(
        "enum Box<T> { V(T) }\nfunc main() {\n    a := Box.V(1)\n    s := Box.V(\"hi\")\n    _c := (a == s)\n}\n",
        "mismatch",
    );
}

#[test]
fn enum_generic_arity_reports() {
    errors_contain(
        "enum Box<T> { V(T) }\nfunc main() {\n    b: Box<int, str> = Box.V(1)\n}\n",
        "takes 1 type argument",
    );
}

#[test]
fn option_matched_with_ok_suggests_some() {
    let r = check_src(
        "func main() {\n    x := .some(1)\n    match x {\n        .ok(v) => v\n        .err(e) => e\n    }\n}",
    );
    let notes: Vec<String> = r.errors.iter().flat_map(|e| e.notes.clone()).collect();
    assert!(
        notes.iter().any(|n| n.contains("use `.some(x)`")),
        "expected `.some` hint, got: {notes:?}"
    );
}

#[test]
fn result_matched_with_some_suggests_ok() {
    let r = check_src(
        "func f() -> Result<int, str> { .ok(1) }\nfunc main() {\n    r := f()\n    match r {\n        .some(v) => v\n        .none => 0\n    }\n}",
    );
    let notes: Vec<String> = r.errors.iter().flat_map(|e| e.notes.clone()).collect();
    assert!(
        notes.iter().any(|n| n.contains("use `.ok(x)`")),
        "expected `.ok` hint, got: {notes:?}"
    );
}

#[test]
fn unwrapped_result_at_return_suggests_fixes() {
    let r = check_src(
        "func f() -> Result<int, str> { .ok(1) }\nfunc g() -> int {\n    t := f()\n    t\n}",
    );
    let msgs: Vec<String> = r.errors.iter().map(|e| e.message.clone()).collect();
    assert!(
        msgs.iter()
            .any(|m| m.contains("unwrapped `Result<int, str>`")),
        "expected unwrap error, got: {msgs:?}"
    );
    let notes: Vec<String> = r.errors.iter().flat_map(|e| e.notes.clone()).collect();
    assert!(
        notes.iter().any(|n| n.contains('?')),
        "expected `?` suggestion, got: {notes:?}"
    );
}

#[test]
fn match_mismatch_binds_no_cascade() {
    // Root mismatch errors only: payload names bind as Error so their uses
    // stay silent (#246).
    let r = check_src(
        "func div(a: int, b: int) -> int { a / b }\nfunc main() {\n    r := div(1, 1)\n    match r {\n        .ok(v) => v\n        .err(e) => e\n    }\n}",
    );
    let errs: Vec<String> = r
        .errors
        .iter()
        .filter(|e| e.severity == zz_frontend::diag::Severity::Error)
        .map(|e| e.message.clone())
        .collect();
    assert_eq!(
        errs.len(),
        2,
        "expected only the two mismatch errors, got: {errs:?}"
    );
}

#[test]
fn enum_variant_typo_suggests() {
    let r = check_src(
        "enum Color { Red, Green, Blue }\nfunc main() {\n    c := Color.Red\n    match c {\n        .Gren(v) => v\n        .Red => 1\n        .Blue => 2\n    }\n}",
    );
    let notes: Vec<String> = r.errors.iter().flat_map(|e| e.notes.clone()).collect();
    assert!(
        notes.iter().any(|n| n.contains(".Green")),
        "expected `.Green` suggestion, got: {notes:?}",
    );
}

#[test]
fn literal_zero_divisor_is_check_error() {
    let r = check_src_with_funcs(
        "func main() {\n    x := 1 / 0\n    println(x)\n}",
        print_test_funcs(),
    );
    let msgs: Vec<String> = r.errors.iter().map(|e| e.message.clone()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("division by zero")),
        "expected div-zero error, got: {msgs:?}"
    );
}

#[test]
fn literal_zero_remainder_is_check_error() {
    let r = check_src_with_funcs("func main() {\n    println(5 % 0)\n}", print_test_funcs());
    let msgs: Vec<String> = r.errors.iter().map(|e| e.message.clone()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("remainder by zero")),
        "expected rem-zero error, got: {msgs:?}"
    );
}

#[test]
fn float_division_by_zero_stays_legal() {
    // No println: the point is float division itself stays legal.
    let r = check_src("func main() {\n    _x := 1.0 / 0.0\n}");
    assert!(
        !r.errors.iter().any(|e| e.message.contains("zero")),
        "float div-zero must stay legal, got: {:?}",
        r.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn unreachable_after_return_warns() {
    let r = check_src_with_funcs(
        "func main() {\n    return\n    println(\"hi\")\n}",
        print_test_funcs(),
    );
    assert!(
        r.errors
            .iter()
            .any(|e| e.message.contains("unreachable code")),
        "expected unreachable warning, got: {:?}",
        r.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    assert!(
        !has_errors(&r),
        "unreachable must be a warning, not an error"
    );
}
