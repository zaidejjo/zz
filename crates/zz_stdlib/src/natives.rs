//! Standard library native implementations, consumed by the interpreter.
//!
//! Native functions take `&mut Vec<Value>` (not a slice) because
//! `std.vec.push` must grow the argument vector.

#![allow(clippy::ptr_arg)]

use std::collections::HashMap;

use zz_runtime::{EvalError, NativeEntry, Value};

/// All standard library native functions, keyed by qualified name.
pub fn stdlib_natives() -> HashMap<String, NativeEntry> {
    let mut m = HashMap::new();

    // std.io
    m.insert(
        "std.io.printz".into(),
        NativeEntry {
            arity: 1,
            f: printz,
        },
    );
    m.insert(
        "std.io.println".into(),
        NativeEntry {
            arity: 1,
            f: println,
        },
    );
    m.insert(
        "std.io.read_line".into(),
        NativeEntry {
            arity: 0,
            f: read_line,
        },
    );

    // std.str
    m.insert(
        "std.str.length".into(),
        NativeEntry {
            arity: 1,
            f: str_length,
        },
    );
    m.insert(
        "std.str.split".into(),
        NativeEntry {
            arity: 2,
            f: str_split,
        },
    );
    m.insert(
        "std.str.contains".into(),
        NativeEntry {
            arity: 2,
            f: str_contains,
        },
    );

    // std.vec
    m.insert(
        "std.vec.len".into(),
        NativeEntry {
            arity: 1,
            f: vec_len,
        },
    );
    m.insert(
        "std.vec.push".into(),
        NativeEntry {
            arity: 2,
            f: vec_push,
        },
    );
    m.insert(
        "std.vec.pop".into(),
        NativeEntry {
            arity: 1,
            f: vec_pop,
        },
    );

    m
}

// --- helpers ---------------------------------------------------------------

fn arg<'a>(args: &'a mut Vec<Value>, i: usize, name: &str) -> Result<&'a Value, EvalError> {
    args.get(i).ok_or_else(|| {
        EvalError::new(
            format!("missing argument `{name}` for native function"),
            zz_runtime::Span::new(0, 0),
        )
    })
}

fn expect_str(args: &mut Vec<Value>, i: usize, name: &str) -> Result<String, EvalError> {
    match arg(args, i, name)? {
        Value::Str(s) => Ok(s.clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects a string, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

fn expect_array(args: &mut Vec<Value>, i: usize, name: &str) -> Result<Vec<Value>, EvalError> {
    match arg(args, i, name)? {
        Value::Array(vs) => Ok(vs.clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects an array, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

// --- std.io -----------------------------------------------------------------

fn printz(args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let v = args.first().cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument for std.io.printz",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    print!("{v}");
    Ok(Value::Unit)
}

fn println(args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let v = args.first().cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument for std.io.println",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    println!("{v}");
    Ok(Value::Unit)
}

fn read_line(_args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| {
        EvalError::new(
            format!("failed to read line: {e}"),
            zz_runtime::Span::new(0, 0),
        )
    })?;
    // Strip the trailing newline (and CR for Windows line endings).
    Ok(Value::Str(line.trim_end_matches(['\n', '\r']).to_string()))
}

// --- std.str ----------------------------------------------------------------

fn str_length(args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.length")?;
    Ok(Value::Int(s.chars().count() as i64))
}

fn str_split(args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.split")?;
    let sep = expect_str(args, 1, "std.str.split")?;
    let parts: Vec<Value> = s.split(&sep).map(|p| Value::Str(p.to_string())).collect();
    Ok(Value::Array(parts))
}

fn str_contains(args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let s = expect_str(args, 0, "std.str.contains")?;
    let sub = expect_str(args, 1, "std.str.contains")?;
    Ok(Value::Bool(s.contains(&sub)))
}

// --- std.vec ----------------------------------------------------------------

fn vec_len(args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let vs = expect_array(args, 0, "std.vec.len")?;
    Ok(Value::Int(vs.len() as i64))
}

fn vec_push(args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let mut vs = expect_array(args, 0, "std.vec.push")?;
    let x = args.get(1).cloned().ok_or_else(|| {
        EvalError::new(
            "missing argument `x` for std.vec.push",
            zz_runtime::Span::new(0, 0),
        )
    })?;
    vs.push(x);
    Ok(Value::Array(vs))
}

fn vec_pop(args: &mut Vec<Value>) -> Result<Value, EvalError> {
    let mut vs = expect_array(args, 0, "std.vec.pop")?;
    if vs.is_empty() {
        return Err(EvalError::new(
            "std.vec.pop: cannot pop from an empty array",
            zz_runtime::Span::new(0, 0),
        ));
    }
    vs.pop();
    Ok(Value::Array(vs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: Vec<Value>) -> Result<Value, EvalError> {
        let mut args = args;
        let entry = stdlib_natives()[name];
        (entry.f)(&mut args)
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
}
