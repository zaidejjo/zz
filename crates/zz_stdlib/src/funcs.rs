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
        assert_eq!(funcs.len(), 9);
    }

    #[test]
    fn vec_funcs_are_generic() {
        let funcs = stdlib_funcs();
        assert_eq!(funcs["std.vec.push"].generics, vec!["T"]);
        assert_eq!(funcs["std.vec.push"].params[1].1, Type::Named("T".into()));
    }
}
