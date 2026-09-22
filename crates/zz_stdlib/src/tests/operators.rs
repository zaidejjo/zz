use super::run;
use zz_runtime::Value;

#[test]
fn pow_int_basic() {
    assert_eq!(run("2 ** 3").unwrap(), Value::Int(8));
}

#[test]
fn pow_int_zero_exp() {
    assert_eq!(run("5 ** 0").unwrap(), Value::Int(1));
}

#[test]
fn pow_int_large() {
    assert_eq!(run("2 ** 10").unwrap(), Value::Int(1024));
}

#[test]
fn pow_float() {
    let v = run("2.0 ** 0.5").unwrap();
    match v {
        Value::Float(f) => assert!((f - 1.4142135623730951).abs() < 1e-10),
        other => panic!("expected float, got {other}"),
    }
}

#[test]
fn pow_right_associative() {
    assert_eq!(run("2 ** 2 ** 3").unwrap(), Value::Int(256));
}

#[test]
fn elvis_some_unwraps() {
    assert_eq!(run("x := .some(42)\nx ?? 0").unwrap(), Value::Int(42));
}

#[test]
fn elvis_none_fallback() {
    assert_eq!(
        run("x: Option<int> = .none\nx ?? 0").unwrap(),
        Value::Int(0)
    );
}

#[test]
fn elvis_non_option_passes_through() {
    assert_eq!(run("42 ?? 0").unwrap(), Value::Int(42));
}

#[test]
fn elvis_chain() {
    assert_eq!(run("42 ?? 0 ?? -1").unwrap(), Value::Int(42));
}

#[test]
fn elvis_none_to_none_chain() {
    assert_eq!(
        run(r#"
            a: Option<int> = .some(42)
            a ?? 0 ?? -1
        "#)
        .unwrap(),
        Value::Int(42)
    );
}

#[test]
fn elvis_with_format_spec_hex() {
    let v = run(r#"
        n := 255
        "{n:x}"
    "#)
    .unwrap();
    assert_eq!(v, Value::Str("ff".to_string()));
}

#[test]
fn elvis_with_format_spec_float() {
    let v = run(r#"
        pi := 3.14159
        "{pi:.2f}"
    "#)
    .unwrap();
    assert_eq!(v, Value::Str("3.14".to_string()));
}
