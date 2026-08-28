use super::run;
use zz_runtime::Value;

#[test]
fn option_unwrap_some() {
    assert_eq!(run(".some(42).unwrap()").unwrap(), Value::Int(42));
}

#[test]
fn option_unwrap_or_some() {
    assert_eq!(run(".some(42).unwrap_or(0)").unwrap(), Value::Int(42));
}

#[test]
fn option_unwrap_or_none() {
    assert_eq!(run(".none.unwrap_or(99)").unwrap(), Value::Int(99));
}

#[test]
fn option_expect_some() {
    assert_eq!(
        run(".some(42).expect(\"should exist\")").unwrap(),
        Value::Int(42)
    );
}

#[test]
fn option_expect_none_errors() {
    let err = run(".none.expect(\"missing!\")").unwrap_err();
    assert!(err.contains("missing!"), "{}", err);
}

#[test]
fn result_unwrap_ok() {
    assert_eq!(run(".ok(42).unwrap()").unwrap(), Value::Int(42));
}

#[test]
fn result_unwrap_or_ok() {
    assert_eq!(run(".ok(42).unwrap_or(0)").unwrap(), Value::Int(42));
}

#[test]
fn result_unwrap_or_err() {
    assert_eq!(run(".err(\"boom\").unwrap_or(99)").unwrap(), Value::Int(99));
}

#[test]
fn result_expect_ok() {
    assert_eq!(
        run(".ok(42).expect(\"should work\")").unwrap(),
        Value::Int(42)
    );
}

#[test]
fn result_expect_err_errors() {
    let err = run(".err(\"bad\").expect(\"nope\")").unwrap_err();
    assert!(err.contains("nope"), "{}", err);
    assert!(err.contains("bad"), "{}", err);
}

#[test]
fn option_unwrap_via_int_parse() {
    assert_eq!(run("x := int(\"42\")\nx.unwrap()").unwrap(), Value::Int(42));
}

#[test]
fn option_unwrap_or_via_int_parse() {
    assert_eq!(
        run("x := int(\"abc\")\nx.unwrap_or(-1)").unwrap(),
        Value::Int(-1)
    );
}

#[test]
fn result_unwrap_via_fs() {
    let v = run("import std.fs\nfs.read_file(\"/tmp/zz_no_such_file_zz\").unwrap_or(\"default\")")
        .unwrap();
    assert_eq!(v, Value::Str("default".to_string()));
}
