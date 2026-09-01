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

#[test]
fn push_mutates_array() {
    let result = run(r#"
        arr := [1, 2, 3]
        arr.push(4)
        arr.len()
    "#)
    .unwrap();
    assert_eq!(result, Value::Int(4));
}

#[test]
fn push_preserves_elements() {
    let result = run(r#"
        arr := [1, 2, 3]
        arr.push(4)
        arr[0] + arr[1] + arr[2] + arr[3]
    "#)
    .unwrap();
    assert_eq!(result, Value::Int(10));
}

#[test]
fn push_struct_array() {
    let result = run(r#"
        struct User {
            name: str
        }
        users := []
        users.push(User { name: "Alice" })
        users.push(User { name: "Bob" })
        users.len()
    "#)
    .unwrap();
    assert_eq!(result, Value::Int(2));
}

#[test]
fn push_struct_array_for_loop() {
    let result = run(r#"
        struct User {
            name: str
        }
        users := []
        users.push(User { name: "Alice" })
        users.push(User { name: "Bob" })
        names := ""
        for u in users {
            names = names + u.name + " "
        }
        names.trim()
    "#)
    .unwrap();
    assert_eq!(result, Value::Str("Alice Bob".to_string()));
}

#[test]
fn append_mutates_array() {
    let result = run(r#"
        arr := [1, 2, 3]
        append(arr, 4)
        arr.len()
    "#)
    .unwrap();
    assert_eq!(result, Value::Int(4));
}

#[test]
fn append_preserves_elements() {
    let result = run(r#"
        arr := [1, 2, 3]
        append(arr, 4)
        arr[0] + arr[1] + arr[2] + arr[3]
    "#)
    .unwrap();
    assert_eq!(result, Value::Int(10));
}
