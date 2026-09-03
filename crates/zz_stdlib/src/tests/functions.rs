use super::run;
use zz_runtime::Value;

#[test]
fn default_param_uses_default() {
    assert_eq!(
        run(r#"
            func greet(name: str, greeting: str = "Hello") -> str {
                "{greeting}, {name}!"
            }
            greet("Alice")
        "#)
        .unwrap(),
        Value::Str("Hello, Alice!".to_string().into())
    );
}

#[test]
fn default_param_override() {
    assert_eq!(
        run(r#"
            func greet(name: str, greeting: str = "Hello") -> str {
                "{greeting}, {name}!"
            }
            greet("Bob", "Hi")
        "#)
        .unwrap(),
        Value::Str("Hi, Bob!".to_string().into())
    );
}

#[test]
fn multiple_defaults_partial_override() {
    assert_eq!(
        run(r#"
            func connect(host: str, port: int = 8080, timeout: int = 30) -> str {
                "{host}:{port} t={timeout}"
            }
            connect("db.local", port: 5432)
        "#)
        .unwrap(),
        Value::Str("db.local:5432 t=30".to_string().into())
    );
}

#[test]
fn named_args_out_of_order() {
    assert_eq!(
        run(r#"
            func f(a: int, b: int, c: int) -> int {
                a + b + c
            }
            f(c: 30, a: 1, b: 2)
        "#)
        .unwrap(),
        Value::Int(33)
    );
}

#[test]
fn named_args_with_defaults() {
    assert_eq!(
        run(r#"
            func f(x: int, y: int = 10, z: int = 20) -> int {
                x + y + z
            }
            f(z: 5, x: 1)
        "#)
        .unwrap(),
        Value::Int(16)
    );
}

#[test]
fn named_args_all_positional() {
    assert_eq!(
        run(r#"
            func add(a: int, b: int) -> int { a + b }
            add(3, 4)
        "#)
        .unwrap(),
        Value::Int(7)
    );
}
