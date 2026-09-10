use super::super::{stdlib_funcs, stdlib_natives};
use zz_runtime::{EvalError, Interp, Span, Value};

fn call(name: &str, args: Vec<Value>) -> Result<Value, EvalError> {
    let mut interp = Interp::new();
    let mut args = args;
    let entry = stdlib_natives()[name];
    (entry.f)(&mut interp, &mut args, Span::new(0, 0))
}

#[test]
fn str_length_counts_chars() {
    assert_eq!(
        call(
            "std.str.length",
            vec![Value::Str("héllo".to_string().into())]
        )
        .unwrap(),
        Value::Int(5)
    );
}

#[test]
fn str_split_splits() {
    assert_eq!(
        call(
            "std.str.split",
            vec![
                Value::Str("a,b,c".to_string().into()),
                Value::Str(",".to_string().into())
            ]
        )
        .unwrap(),
        Value::Array(Box::new(vec![
            Value::Str("a".to_string().into()),
            Value::Str("b".to_string().into()),
            Value::Str("c".to_string().into()),
        ]))
    );
}

#[test]
fn str_contains_finds_substring() {
    assert_eq!(
        call(
            "std.str.contains",
            vec![
                Value::Str("hello".to_string().into()),
                Value::Str("ell".to_string().into())
            ]
        )
        .unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        call(
            "std.str.contains",
            vec![
                Value::Str("hello".to_string().into()),
                Value::Str("xyz".to_string().into())
            ]
        )
        .unwrap(),
        Value::Bool(false)
    );
}

#[test]
fn vec_len_counts() {
    assert_eq!(
        call(
            "std.vec.len",
            vec![Value::Array(Box::new(vec![Value::Int(1), Value::Int(2)]))]
        )
        .unwrap(),
        Value::Int(2)
    );
}

#[test]
fn vec_push_appends() {
    assert_eq!(
        call(
            "std.vec.push",
            vec![Value::Array(Box::new(vec![Value::Int(1)])), Value::Int(2),]
        )
        .unwrap(),
        Value::Array(Box::new(vec![Value::Int(1), Value::Int(2)]))
    );
}

#[test]
fn vec_pop_removes_last() {
    assert_eq!(
        call(
            "std.vec.pop",
            vec![Value::Array(Box::new(vec![Value::Int(1), Value::Int(2)]))]
        )
        .unwrap(),
        Value::Array(Box::new(vec![Value::Int(1)]))
    );
}

#[test]
fn vec_pop_empty_errors() {
    let err = call("std.vec.pop", vec![Value::Array(Box::default())]).unwrap_err();
    assert!(err.message.contains("empty array"), "{}", err.message);
}

#[test]
fn wrong_type_errors() {
    let err = call("std.str.length", vec![Value::Int(5)]).unwrap_err();
    assert!(err.message.contains("expects a string"), "{}", err.message);
}

#[test]
fn read_line_from_dev_null_is_empty() {
    // In the test harness stdin is /dev/null, so read_line yields "".
    assert_eq!(
        call("std.io.read_line", vec![]).unwrap(),
        Value::Str(String::new().into())
    );
}

#[test]
fn every_funcs_key_has_a_native() {
    // Drift census: the checker registry (`stdlib_funcs`) and the interpreter
    // registry (`stdlib_natives`) must stay in lockstep. Every signature the
    // checker knows must resolve to a runtime implementation.
    //
    // Exception: pure-ZZ stdlib functions (str.repeat, str.count, math.sum,
    // math.product, math.count, vec.fold) are implemented in .zz files and
    // compiled at startup — they have no Rust native entry.
    let pure_zz_funcs = [
        // str helpers
        "std.str.repeat",
        "std.str.count",
        "std.str.is_empty",
        "std.str.reverse",
        "std.str.pad_left",
        "std.str.pad_right",
        "str.repeat",
        "str.count",
        "str.is_empty",
        "str.reverse",
        "str.pad_left",
        "str.pad_right",
        // math helpers
        "std.math.sum",
        "std.math.product",
        "std.math.count",
        "std.math.min",
        "std.math.max",
        "std.math.is_even",
        "std.math.is_odd",
        "std.math.min_arr",
        "std.math.max_arr",
        "std.math.sum_f",
        "std.math.product_f",
        "std.math.mean_f",
        "std.math.median_f",
        "math.sum",
        "math.product",
        "math.count",
        "math.min",
        "math.max",
        "math.is_even",
        "math.is_odd",
        "math.min_arr",
        "math.max_arr",
        "math.sum_f",
        "math.product_f",
        "math.mean_f",
        "math.median_f",
        // vec helpers
        "std.vec.fold",
        "std.vec.sum",
        "std.vec.product",
        "std.vec.min_val",
        "std.vec.max_val",
        "std.vec.sum_f",
        "std.vec.product_f",
        "std.vec.concat",
        "std.vec.flatten",
        "std.vec.index_of",
        "std.vec.last_index_of",
        "vec.fold",
        "vec.sum",
        "vec.product",
        "vec.min_val",
        "vec.max_val",
        "vec.sum_f",
        "vec.product_f",
        "vec.concat",
        "vec.flatten",
        "vec.index_of",
        "vec.last_index_of",
    ];
    let funcs = stdlib_funcs();
    let natives = stdlib_natives();
    let missing: Vec<String> = funcs
        .keys()
        .filter(|k| !natives.contains_key(*k) && !pure_zz_funcs.contains(&k.as_str()))
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "stdlib_funcs keys without a stdlib_natives impl: {missing:?}"
    );
    let untyped: Vec<String> = natives
        .keys()
        .filter(|k| !funcs.contains_key(*k))
        .cloned()
        .collect();
    assert!(
        untyped.is_empty(),
        "stdlib_natives keys without a stdlib_funcs signature: {untyped:?}"
    );
}
