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
        "expected 1 to 1 arguments, found 2",
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
    assert_eq!(r.bindings["p"], Type::Struct("Point".into()));
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
    assert_eq!(r.bindings["p"], Type::Struct("Point".into()));
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
        "type mismatch",
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
    let r = check_src("import std.io\nx := 1");
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
            generics: vec!["T".to_string()],
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
            generics: Vec::new(),
            params: vec![
                ("p".to_string(), Type::Struct("Point".to_string())),
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
            generics: Vec::new(),
            params: vec![("p".to_string(), Type::Struct("shapes.Point".to_string()))],
            has_default: vec![],
            ret: Type::Int,
        },
    );
    let mut structs = HashMap::new();
    structs.insert(
        "shapes.Point".to_string(),
        StructSig {
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
            generics: vec!["T".to_string()],
            params: vec![("v".to_string(), t.clone())],
            has_default: vec![],
            ret: Type::Str,
        },
    );
    funcs.insert(
        "int".to_string(),
        FuncSig {
            generics: vec!["T".to_string()],
            params: vec![("v".to_string(), t.clone())],
            has_default: vec![],
            ret: Type::Option(Box::new(Type::Int)),
        },
    );
    funcs.insert(
        "float".to_string(),
        FuncSig {
            generics: vec!["T".to_string()],
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
            generics: vec![],
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
    let r = check_program(&parsed.program, HashMap::new(), funcs, HashMap::new());
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
fn fixit_structure_is_populated() {
    use zz_frontend::diag::FixIt;
    let fixit = FixIt::safe(Span::new(0, 5), "_x", "rename to");
    assert_eq!(fixit.replacement, "_x");
    assert_eq!(fixit.message, "rename to");
}
