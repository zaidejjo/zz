use super::run;
use zz_runtime::Value;

#[test]
fn list_comp_basic() {
    assert_eq!(
        run("[x * 2 for x in [1, 2, 3, 4]]").unwrap(),
        Value::Array(Box::new(vec![
            Value::Int(2),
            Value::Int(4),
            Value::Int(6),
            Value::Int(8)
        ]))
    );
}

#[test]
fn list_comp_filter() {
    assert_eq!(
        run("[x for x in [1, 2, 3, 4] if x > 2]").unwrap(),
        Value::Array(Box::new(vec![Value::Int(3), Value::Int(4)]))
    );
}

#[test]
fn list_comp_range() {
    assert_eq!(
        run("[x * x for x in 0..5]").unwrap(),
        Value::Array(Box::new(vec![
            Value::Int(0),
            Value::Int(1),
            Value::Int(4),
            Value::Int(9),
            Value::Int(16)
        ]))
    );
}

#[test]
fn list_comp_range_filter() {
    assert_eq!(
        run("[x * x for x in 0..10 if x % 2 == 0]").unwrap(),
        Value::Array(Box::new(vec![
            Value::Int(0),
            Value::Int(4),
            Value::Int(16),
            Value::Int(36),
            Value::Int(64)
        ]))
    );
}

#[test]
fn list_comp_multiple_sequential() {
    let src = r#"
        a := [i * 2 for i in [1, 2, 3]]
        b := [i + 10 for i in [4, 5, 6]]
        c := [i for i in 0..5 if i % 2 == 0]
        a
    "#;
    assert_eq!(
        run(src).unwrap(),
        Value::Array(Box::new(vec![Value::Int(2), Value::Int(4), Value::Int(6)]))
    );
}

#[test]
fn list_comp_six_sequential() {
    let src = r#"
        a := [i * 1 for i in [1, 2, 3]]
        b := [i * 2 for i in [4, 5, 6]]
        c := [i * 3 for i in [7, 8, 9]]
        d := [i for i in 0..10 if i > 5]
        e := [i ** 2 for i in 0..5]
        f := [x + 10 for x in [1, 2, 3, 4, 5]]
        a
    "#;
    assert_eq!(
        run(src).unwrap(),
        Value::Array(Box::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]))
    );
}
