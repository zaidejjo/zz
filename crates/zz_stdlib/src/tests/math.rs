use super::run;
use zz_runtime::Value;

#[test]
fn math_abs() {
    assert_eq!(run("import std.math\nmath.abs(-5)").unwrap(), Value::Int(5));
    assert_eq!(
        run("import std.math\nmath.abs(-5.5)").unwrap(),
        Value::Float(5.5)
    );
    assert_eq!(run("import std.math\nmath.abs(3)").unwrap(), Value::Int(3));
}

#[test]
fn math_floor_ceil() {
    assert_eq!(
        run("import std.math\nmath.floor(3.7)").unwrap(),
        Value::Int(3)
    );
    assert_eq!(
        run("import std.math\nmath.floor(-3.2)").unwrap(),
        Value::Int(-4)
    );
    assert_eq!(
        run("import std.math\nmath.ceil(3.2)").unwrap(),
        Value::Int(4)
    );
    assert_eq!(
        run("import std.math\nmath.ceil(-3.7)").unwrap(),
        Value::Int(-3)
    );
}

#[test]
fn math_sqrt_pow() {
    assert_eq!(
        run("import std.math\nmath.sqrt(9)").unwrap(),
        Value::Float(3.0)
    );
    assert_eq!(
        run("import std.math\nmath.sqrt(2.25)").unwrap(),
        Value::Float(1.5)
    );
    assert_eq!(
        run("import std.math\nmath.pow(2, 10)").unwrap(),
        Value::Float(1024.0)
    );
    assert_eq!(
        run("import std.math\nmath.pow(2.0, 3.0)").unwrap(),
        Value::Float(8.0)
    );
}

#[test]
fn math_random_in_range() {
    let v = run("import std.math\nmath.random()").unwrap();
    match v {
        Value::Float(f) => assert!((0.0..1.0).contains(&f), "random out of range: {f}"),
        other => panic!("expected float, got {other}"),
    }
}
