use super::run;
use zz_runtime::Value;

#[test]
fn typeof_builtin() {
    assert_eq!(run("typeof(1)").unwrap(), Value::Str("int".to_string()));
    assert_eq!(run("typeof(1.5)").unwrap(), Value::Str("float".to_string()));
    assert_eq!(run("typeof(\"x\")").unwrap(), Value::Str("str".to_string()));
    assert_eq!(run("typeof(true)").unwrap(), Value::Str("bool".to_string()));
    assert_eq!(
        run("typeof([1, 2])").unwrap(),
        Value::Str("array".to_string())
    );
    assert_eq!(
        run("typeof({\"a\": 1})").unwrap(),
        Value::Str("dict".to_string())
    );
    assert_eq!(
        run("typeof(.some(1))").unwrap(),
        Value::Str("option".to_string())
    );
    assert_eq!(
        run("typeof(.ok(1))").unwrap(),
        Value::Str("result".to_string())
    );
    assert_eq!(
        run("typeof(0..5)").unwrap(),
        Value::Str("range".to_string())
    );
}

#[test]
fn typeof_struct_reports_name() {
    let v = run("struct Point { x: int }\np := Point{ x: 1 }\ntypeof(p)").unwrap();
    assert_eq!(v, Value::Str("Point".to_string()));
}
