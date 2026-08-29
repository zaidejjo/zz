use super::super::stdlib_natives;
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
        call("std.str.length", vec![Value::Str("héllo".into())]).unwrap(),
        Value::Int(5)
    );
}

#[test]
fn str_split_splits() {
    assert_eq!(
        call(
            "std.str.split",
            vec![Value::Str("a,b,c".into()), Value::Str(",".into())]
        )
        .unwrap(),
        Value::Array(vec![
            Value::Str("a".into()),
            Value::Str("b".into()),
            Value::Str("c".into()),
        ])
    );
}

#[test]
fn str_contains_finds_substring() {
    assert_eq!(
        call(
            "std.str.contains",
            vec![Value::Str("hello".into()), Value::Str("ell".into())]
        )
        .unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        call(
            "std.str.contains",
            vec![Value::Str("hello".into()), Value::Str("xyz".into())]
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
            vec![Value::Array(vec![Value::Int(1), Value::Int(2)])]
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
            vec![Value::Array(vec![Value::Int(1)]), Value::Int(2),]
        )
        .unwrap(),
        Value::Array(vec![Value::Int(1), Value::Int(2)])
    );
}

#[test]
fn vec_pop_removes_last() {
    assert_eq!(
        call(
            "std.vec.pop",
            vec![Value::Array(vec![Value::Int(1), Value::Int(2)])]
        )
        .unwrap(),
        Value::Array(vec![Value::Int(1)])
    );
}

#[test]
fn vec_pop_empty_errors() {
    let err = call("std.vec.pop", vec![Value::Array(vec![])]).unwrap_err();
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
        Value::Str(String::new())
    );
}
