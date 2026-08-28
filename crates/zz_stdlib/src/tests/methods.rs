use super::run;
use zz_runtime::Value;

#[test]
fn literal_str_method_trim() {
    assert_eq!(
        run("\" hello \".trim()").unwrap(),
        Value::Str("hello".to_string())
    );
}

#[test]
fn literal_str_method_to_upper() {
    assert_eq!(
        run("\"hello\".to_upper()").unwrap(),
        Value::Str("HELLO".to_string())
    );
}

#[test]
fn literal_str_method_to_lower() {
    assert_eq!(
        run("\"HELLO\".to_lower()").unwrap(),
        Value::Str("hello".to_string())
    );
}

#[test]
fn literal_str_method_chaining() {
    assert_eq!(
        run("\" hello \".trim().to_upper()").unwrap(),
        Value::Str("HELLO".to_string())
    );
}

#[test]
fn literal_str_method_triple_chain() {
    assert_eq!(
        run("\"  world  \".trim().to_upper().to_lower()").unwrap(),
        Value::Str("world".to_string())
    );
}

#[test]
fn literal_array_method_reverse() {
    assert_eq!(
        run("[1, 2, 3].reverse()").unwrap(),
        Value::Array(vec![Value::Int(3), Value::Int(2), Value::Int(1),])
    );
}

#[test]
fn literal_array_method_sort() {
    assert_eq!(
        run("[3, 1, 2].sort()").unwrap(),
        Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3),])
    );
}
