use super::run;
use zz_runtime::Value;

#[test]
fn conv_str() {
    assert_eq!(run("str(42)").unwrap(), Value::Str("42".to_string().into()));
    assert_eq!(
        run("str(3.5)").unwrap(),
        Value::Str("3.5".to_string().into())
    );
    assert_eq!(
        run("str(true)").unwrap(),
        Value::Str("true".to_string().into())
    );
    assert_eq!(
        run("str([1, 2])").unwrap(),
        Value::Str("[1, 2]".to_string().into())
    );
    assert_eq!(
        run("str({\"a\": 1})").unwrap(),
        Value::Str("{a: 1}".to_string().into())
    );
}

#[test]
fn conv_int() {
    assert_eq!(
        run("int(\"42\")").unwrap(),
        Value::Option(Some(Box::new(Value::Int(42))))
    );
    assert_eq!(
        run("int(\" 7 \")").unwrap(),
        Value::Option(Some(Box::new(Value::Int(7))))
    );
    assert_eq!(run("int(\"abc\")").unwrap(), Value::Option(None));
    assert_eq!(
        run("int(3.7)").unwrap(),
        Value::Option(Some(Box::new(Value::Int(3))))
    );
    assert_eq!(
        run("int(5)").unwrap(),
        Value::Option(Some(Box::new(Value::Int(5))))
    );
    assert_eq!(run("int(true)").unwrap(), Value::Option(None));
}

#[test]
fn conv_float() {
    assert_eq!(run("float(\"2.5\")").unwrap(), Value::Float(2.5));
    // Invalid parse returns NaN (not Option::None).
    match run("float(\"x\")").unwrap() {
        Value::Float(f) => assert!(f.is_nan()),
        other => panic!("expected float NaN, got {:?}", other),
    }
    assert_eq!(run("float(3)").unwrap(), Value::Float(3.0));
    assert_eq!(run("float(1.5)").unwrap(), Value::Float(1.5));
}
