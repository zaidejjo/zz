//! Standard library type signatures, consumed by the checker.

use std::collections::HashMap;

use zz_checker::{FuncSig, Type};

/// Build a non-generic signature.
fn sig(params: Vec<(&str, Type)>, ret: Type) -> FuncSig {
    FuncSig {
        generics: Vec::new(),
        params: params
            .into_iter()
            .map(|(n, t)| (n.to_string(), t))
            .collect(),
        ret,
    }
}

/// Build a signature generic over `T`.
fn sig_t(params: Vec<(&str, Type)>, ret: Type) -> FuncSig {
    FuncSig {
        generics: vec!["T".to_string()],
        params: params
            .into_iter()
            .map(|(n, t)| (n.to_string(), t))
            .collect(),
        ret,
    }
}

/// All standard library function signatures, keyed by qualified name
/// (e.g. `std.io.println`).
pub fn stdlib_funcs() -> HashMap<String, FuncSig> {
    let mut m = HashMap::new();

    // std.io — print functions accept any value (displayed).
    let t = Type::Named("T".to_string());
    m.insert(
        "std.io.printz".into(),
        sig_t(vec![("v", t.clone())], Type::Unit),
    );
    m.insert(
        "std.io.println".into(),
        sig_t(vec![("v", t.clone())], Type::Unit),
    );
    m.insert("std.io.read_line".into(), sig(vec![], Type::Str));

    // std.str
    m.insert(
        "std.str.length".into(),
        sig(vec![("s", Type::Str)], Type::Int),
    );
    m.insert(
        "std.str.split".into(),
        sig(
            vec![("s", Type::Str), ("sep", Type::Str)],
            Type::Array(Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.str.contains".into(),
        sig(vec![("s", Type::Str), ("sub", Type::Str)], Type::Bool),
    );

    // std.vec — generic over element type T.
    let t = Type::Named("T".to_string());
    m.insert(
        "std.vec.len".into(),
        sig_t(vec![("v", Type::Array(Box::new(t.clone())))], Type::Int),
    );
    m.insert(
        "std.vec.push".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone()))), ("x", t.clone())],
            Type::Array(Box::new(t.clone())),
        ),
    );
    m.insert(
        "std.vec.pop".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone())))],
            Type::Array(Box::new(t)),
        ),
    );

    // std.json
    let json_t = Type::Json;
    let t = Type::Named("T".to_string());
    m.insert(
        "std.json.parse".into(),
        sig(vec![("s", Type::Str)], json_t.clone()),
    );
    m.insert(
        "std.json.stringify".into(),
        sig_t(vec![("v", t.clone())], Type::Str),
    );
    m.insert(
        "std.json.get".into(),
        sig(
            vec![("j", json_t.clone()), ("key", Type::Str)],
            json_t.clone(),
        ),
    );
    m.insert(
        "std.json.as_str".into(),
        sig(vec![("j", json_t.clone())], Type::Str),
    );
    m.insert(
        "std.json.as_int".into(),
        sig(vec![("j", json_t.clone())], Type::Int),
    );
    m.insert(
        "std.json.as_float".into(),
        sig(vec![("j", json_t.clone())], Type::Float),
    );
    m.insert(
        "std.json.as_bool".into(),
        sig(vec![("j", json_t)], Type::Bool),
    );

    // std.http
    let server_t = Type::HttpServer;
    let handler_t = Type::Func(vec![Type::Str], Box::new(Type::Str));
    m.insert("std.http.server".into(), sig(vec![], server_t.clone()));
    m.insert(
        "std.http.get".into(),
        sig(
            vec![
                ("server", server_t.clone()),
                ("path", Type::Str),
                ("handler", handler_t.clone()),
            ],
            server_t.clone(),
        ),
    );
    m.insert(
        "std.http.post".into(),
        sig(
            vec![
                ("server", server_t.clone()),
                ("path", Type::Str),
                ("handler", handler_t.clone()),
            ],
            server_t.clone(),
        ),
    );
    m.insert(
        "std.http.handle".into(),
        sig(
            vec![
                ("server", server_t.clone()),
                ("method", Type::Str),
                ("path", Type::Str),
                ("body", Type::Str),
            ],
            Type::Str,
        ),
    );
    m.insert(
        "std.http.listen".into(),
        sig(vec![("server", server_t), ("port", Type::Int)], Type::Unit),
    );

    // std.fs
    m.insert(
        "std.fs.read_file".into(),
        sig(
            vec![("path", Type::Str)],
            Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.fs.write_file".into(),
        sig(
            vec![("path", Type::Str), ("contents", Type::Str)],
            Type::Result(Box::new(Type::Unit), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.fs.exists".into(),
        sig(vec![("path", Type::Str)], Type::Bool),
    );

    // std.env
    m.insert(
        "std.env.get_var".into(),
        sig(vec![("name", Type::Str)], Type::Option(Box::new(Type::Str))),
    );
    m.insert(
        "std.env.args".into(),
        sig(vec![], Type::Array(Box::new(Type::Str))),
    );

    // Built-in: `typeof(v)` — accepts any value, returns its type name.
    let t = Type::Named("T".to_string());
    m.insert("typeof".into(), sig_t(vec![("v", t.clone())], Type::Str));

    // Built-in conversions.
    // `str(v)` — stringify any value (total).
    m.insert("str".into(), sig_t(vec![("v", t.clone())], Type::Str));
    // `int(v)` — parse a string, truncate a float, or pass through an int.
    // Invalid parses yield `.none`.
    m.insert(
        "int".into(),
        sig_t(vec![("v", t.clone())], Type::Option(Box::new(Type::Int))),
    );
    // `float(v)` — parse a string or widen an int. Invalid parses yield
    // `.none`.
    m.insert(
        "float".into(),
        sig_t(vec![("v", t.clone())], Type::Option(Box::new(Type::Float))),
    );

    // std.math
    m.insert(
        "std.math.abs".into(),
        sig_t(vec![("v", t.clone())], t.clone()),
    );
    m.insert(
        "std.math.floor".into(),
        sig(vec![("v", Type::Float)], Type::Int),
    );
    m.insert(
        "std.math.ceil".into(),
        sig(vec![("v", Type::Float)], Type::Int),
    );
    m.insert(
        "std.math.sqrt".into(),
        sig_t(vec![("v", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.pow".into(),
        sig_t(vec![("base", t.clone()), ("exp", t.clone())], Type::Float),
    );
    m.insert("std.math.random".into(), sig(vec![], Type::Float));

    // std.time
    m.insert("std.time.now_ms".into(), sig(vec![], Type::Int));
    m.insert(
        "std.time.sleep_ms".into(),
        sig(vec![("ms", Type::Int)], Type::Unit),
    );

    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_all_modules() {
        let funcs = stdlib_funcs();
        assert!(funcs.contains_key("std.io.println"));
        assert!(funcs.contains_key("std.str.length"));
        assert!(funcs.contains_key("std.vec.push"));
        assert!(funcs.contains_key("std.json.parse"));
        assert!(funcs.contains_key("std.json.stringify"));
        assert!(funcs.contains_key("std.http.server"));
        assert!(funcs.contains_key("std.http.handle"));
        assert!(funcs.contains_key("std.http.listen"));
        assert!(funcs.contains_key("std.fs.read_file"));
        assert!(funcs.contains_key("std.env.get_var"));
        assert!(funcs.contains_key("std.math.abs"));
        assert!(funcs.contains_key("std.math.random"));
        assert!(funcs.contains_key("std.time.now_ms"));
        assert!(funcs.contains_key("typeof"));
        assert!(funcs.contains_key("str"));
        assert!(funcs.contains_key("int"));
        assert!(funcs.contains_key("float"));
        assert_eq!(funcs.len(), 38);
    }

    #[test]
    fn vec_funcs_are_generic() {
        let funcs = stdlib_funcs();
        assert_eq!(funcs["std.vec.push"].generics, vec!["T"]);
        assert_eq!(funcs["std.vec.push"].params[1].1, Type::Named("T".into()));
    }
}
