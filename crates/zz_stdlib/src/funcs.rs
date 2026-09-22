//! Standard library type signatures, consumed by the checker.

use std::collections::HashMap;

use zz_checker::{FuncSig, Type};

/// Build a non-generic signature.
fn sig(params: Vec<(&str, Type)>, ret: Type) -> FuncSig {
    FuncSig {
        generics: Vec::new(),
        bounds: Vec::new(),
        params: params
            .into_iter()
            .map(|(n, t)| (n.to_string(), t))
            .collect(),
        has_default: vec![],
        ret,
        is_extern: false,
        extern_c_symbol: None,
    }
}

/// Build a signature generic over `T`.
fn sig_t(params: Vec<(&str, Type)>, ret: Type) -> FuncSig {
    FuncSig {
        generics: vec!["T".to_string()],
        bounds: Vec::new(),
        params: params
            .into_iter()
            .map(|(n, t)| (n.to_string(), t))
            .collect(),
        has_default: vec![],
        ret,
        is_extern: false,
        extern_c_symbol: None,
    }
}

/// Build a signature generic over `T, U`.
fn sig_tu(params: Vec<(&str, Type)>, ret: Type) -> FuncSig {
    FuncSig {
        generics: vec!["T".to_string(), "U".to_string()],
        bounds: Vec::new(),
        params: params
            .into_iter()
            .map(|(n, t)| (n.to_string(), t))
            .collect(),
        has_default: vec![],
        ret,
        is_extern: false,
        extern_c_symbol: None,
    }
}

/// All standard library function signatures, keyed by qualified name
/// (e.g. `std.str.length`). Console I/O lives here as bare builtins
/// (`print`, `println`, `input`) — there is no `std.io` module.
pub fn stdlib_funcs() -> HashMap<String, FuncSig> {
    let mut m = HashMap::new();

    // Builtin console I/O — no import required.
    let t = Type::Named("T".to_string());
    m.insert("print".into(), sig_t(vec![("v", t.clone())], Type::Unit));
    m.insert("println".into(), sig_t(vec![("v", t.clone())], Type::Unit));
    m.insert(
        "input".into(),
        sig_t(vec![("prompt", Type::Str)], Type::Str),
    );

    // Range and iterator builtins
    let range_t = Type::Range(Box::new(Type::Int));
    // range(stop) | range(start, stop) | range(start, stop, step)
    // Checker handles variable arg count; signature declares max 3 args.
    m.insert(
        "range".into(),
        sig(
            vec![
                ("start", Type::Int),
                ("stop", Type::Int),
                ("step", Type::Int),
            ],
            range_t.clone(),
        ),
    );
    m.insert("len".into(), sig_t(vec![("v", t.clone())], Type::Int));
    // Union of array-of-T and range-of-T so T stays as element type.
    let iterable_t = Type::Union(vec![
        Type::Array(Box::new(t.clone())),
        Type::Range(Box::new(t.clone())),
    ]);
    m.insert("map".into(), {
        let u = Type::Named("U".to_string());
        sig_tu(
            vec![
                ("arr", iterable_t.clone()),
                ("f", Type::Func(vec![t.clone()], Box::new(u.clone()))),
            ],
            Type::Array(Box::new(u.clone())),
        )
    });
    m.insert(
        "filter".into(),
        sig_t(
            vec![
                ("arr", iterable_t.clone()),
                ("f", Type::Func(vec![t.clone()], Box::new(Type::Bool))),
            ],
            Type::Array(Box::new(t.clone())),
        ),
    );
    m.insert(
        "enumerate".into(),
        sig_t(
            vec![("arr", iterable_t.clone())],
            Type::Array(Box::new(Type::Tuple(vec![Type::Int, t.clone()]))),
        ),
    );
    m.insert("zip".into(), {
        let t2 = Type::Named("U".to_string());
        let iterable_t2 = Type::Union(vec![
            Type::Array(Box::new(t2.clone())),
            Type::Range(Box::new(t2.clone())),
        ]);
        sig_tu(
            vec![("a", iterable_t.clone()), ("b", iterable_t2)],
            Type::Array(Box::new(Type::Tuple(vec![t.clone(), t2.clone()]))),
        )
    });

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

    // str.* methods (for method dispatch: "hello".trim())
    m.insert("str.length".into(), sig(vec![("s", Type::Str)], Type::Int));
    m.insert("str.trim".into(), sig(vec![("s", Type::Str)], Type::Str));
    m.insert(
        "str.to_upper".into(),
        sig(vec![("s", Type::Str)], Type::Str),
    );
    m.insert(
        "str.to_lower".into(),
        sig(vec![("s", Type::Str)], Type::Str),
    );
    m.insert(
        "str.split".into(),
        sig(
            vec![("s", Type::Str), ("sep", Type::Str)],
            Type::Array(Box::new(Type::Str)),
        ),
    );
    m.insert(
        "str.contains".into(),
        sig(vec![("s", Type::Str), ("sub", Type::Str)], Type::Bool),
    );
    m.insert(
        "str.replace".into(),
        sig(
            vec![("s", Type::Str), ("old", Type::Str), ("new", Type::Str)],
            Type::Str,
        ),
    );
    m.insert(
        "str.starts_with".into(),
        sig(vec![("s", Type::Str), ("prefix", Type::Str)], Type::Bool),
    );
    m.insert(
        "str.ends_with".into(),
        sig(vec![("s", Type::Str), ("suffix", Type::Str)], Type::Bool),
    );
    m.insert(
        "str.join".into(),
        sig(
            vec![
                ("items", Type::Array(Box::new(Type::Str))),
                ("sep", Type::Str),
            ],
            Type::Str,
        ),
    );
    m.insert(
        "str.trim_start".into(),
        sig(vec![("s", Type::Str)], Type::Str),
    );
    m.insert(
        "str.trim_end".into(),
        sig(vec![("s", Type::Str)], Type::Str),
    );

    // Pure-ZZ stdlib: string helpers (compiled from zz/str/mod.zz)
    m.insert(
        "std.str.repeat".into(),
        sig(vec![("s", Type::Str), ("n", Type::Int)], Type::Str),
    );
    m.insert(
        "std.str.count".into(),
        sig(vec![("s", Type::Str), ("sub", Type::Str)], Type::Int),
    );
    m.insert(
        "std.str.is_empty".into(),
        sig(vec![("s", Type::Str)], Type::Bool),
    );
    m.insert(
        "std.str.reverse".into(),
        sig(vec![("s", Type::Str)], Type::Str),
    );
    m.insert(
        "std.str.pad_left".into(),
        sig(
            vec![("s", Type::Str), ("len", Type::Int), ("pad", Type::Str)],
            Type::Str,
        ),
    );
    // Asymmetric crypto + JWT. Keypairs cross as `[secret, public]`;
    // signing is fallible (malformed keys), verification is total.
    let str_arr = || Type::Array(Box::new(Type::Str));
    let result_str2 = || Type::Result(Box::new(Type::Str), Box::new(Type::Str));
    m.insert("std.crypto.ed25519_keypair".into(), sig(vec![], str_arr()));
    m.insert(
        "std.crypto.ed25519_sign".into(),
        sig(vec![("sk", Type::Str), ("msg", Type::Str)], result_str2()),
    );
    m.insert(
        "std.crypto.ed25519_verify".into(),
        sig(
            vec![("pk", Type::Str), ("msg", Type::Str), ("sig", Type::Str)],
            Type::Bool,
        ),
    );
    m.insert(
        "std.crypto.rsa_keypair".into(),
        sig(
            vec![],
            Type::Result(Box::new(str_arr()), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.crypto.rsa_sign".into(),
        sig(vec![("sk", Type::Str), ("msg", Type::Str)], result_str2()),
    );
    m.insert(
        "std.crypto.rsa_verify".into(),
        sig(
            vec![("pk", Type::Str), ("msg", Type::Str), ("sig", Type::Str)],
            Type::Bool,
        ),
    );
    m.insert(
        "std.crypto.jwt_encode".into(),
        sig(
            vec![("payload", Type::Str), ("secret", Type::Str)],
            Type::Str,
        ),
    );
    m.insert(
        "std.crypto.jwt_decode".into(),
        sig(
            vec![("token", Type::Str), ("secret", Type::Str)],
            result_str2(),
        ),
    );
    m.insert(
        "std.crypto.jwt_encode_ed".into(),
        sig(
            vec![("payload", Type::Str), ("sk", Type::Str)],
            result_str2(),
        ),
    );
    m.insert(
        "std.crypto.jwt_decode_ed".into(),
        sig(vec![("token", Type::Str), ("pk", Type::Str)], result_str2()),
    );
    m.insert("crypto.ed25519_keypair".into(), sig(vec![], str_arr()));
    m.insert(
        "crypto.ed25519_sign".into(),
        sig(vec![("sk", Type::Str), ("msg", Type::Str)], result_str2()),
    );
    m.insert(
        "crypto.ed25519_verify".into(),
        sig(
            vec![("pk", Type::Str), ("msg", Type::Str), ("sig", Type::Str)],
            Type::Bool,
        ),
    );
    m.insert(
        "crypto.rsa_keypair".into(),
        sig(
            vec![],
            Type::Result(Box::new(str_arr()), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "crypto.rsa_sign".into(),
        sig(vec![("sk", Type::Str), ("msg", Type::Str)], result_str2()),
    );
    m.insert(
        "crypto.rsa_verify".into(),
        sig(
            vec![("pk", Type::Str), ("msg", Type::Str), ("sig", Type::Str)],
            Type::Bool,
        ),
    );
    m.insert(
        "crypto.jwt_encode".into(),
        sig(
            vec![("payload", Type::Str), ("secret", Type::Str)],
            Type::Str,
        ),
    );
    m.insert(
        "crypto.jwt_decode".into(),
        sig(
            vec![("token", Type::Str), ("secret", Type::Str)],
            result_str2(),
        ),
    );
    m.insert(
        "crypto.jwt_decode_ed".into(),
        sig(vec![("token", Type::Str), ("pk", Type::Str)], result_str2()),
    );
    // Process result triples `[status, stdout, stderr]` (heterogeneous).
    let process_t = Type::Opaque("process".to_string());
    let str_arr2 = || Type::Array(Box::new(Type::Str));
    let triple_t = || Type::Array(Box::new(Type::Union(vec![Type::Int, Type::Str, Type::Str])));
    let result_triple = || Type::Result(Box::new(triple_t()), Box::new(Type::Str));
    m.insert(
        "std.process.run".into(),
        sig(
            vec![("cmd", Type::Str), ("argv", str_arr2())],
            result_triple(),
        ),
    );
    m.insert(
        "std.process.run_with_env".into(),
        sig(
            vec![
                ("cmd", Type::Str),
                ("argv", str_arr2()),
                ("env", str_arr2()),
            ],
            result_triple(),
        ),
    );
    m.insert(
        "std.process.spawn".into(),
        sig(
            vec![("cmd", Type::Str), ("argv", str_arr2())],
            Type::Result(Box::new(process_t.clone()), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.process.wait".into(),
        sig(vec![("child", process_t.clone())], result_triple()),
    );
    m.insert(
        "std.process.exit".into(),
        sig(vec![("code", Type::Int)], Type::Unit),
    );
    m.insert("std.process.pid".into(), sig(vec![], Type::Int));
    m.insert(
        "process.run".into(),
        sig(
            vec![("cmd", Type::Str), ("argv", str_arr2())],
            result_triple(),
        ),
    );
    m.insert(
        "process.run_with_env".into(),
        sig(
            vec![
                ("cmd", Type::Str),
                ("argv", str_arr2()),
                ("env", str_arr2()),
            ],
            result_triple(),
        ),
    );
    m.insert(
        "process.spawn".into(),
        sig(
            vec![("cmd", Type::Str), ("argv", str_arr2())],
            Type::Result(Box::new(process_t.clone()), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "process.wait".into(),
        sig(vec![("child", process_t.clone())], result_triple()),
    );
    m.insert(
        "process.exit".into(),
        sig(vec![("code", Type::Int)], Type::Unit),
    );
    m.insert("process.pid".into(), sig(vec![], Type::Int));

    // std.uuid — v4/v7 generation, parse (normalizing), validation.
    // Both spellings (like json).
    let result_str3 = || Type::Result(Box::new(Type::Str), Box::new(Type::Str));
    m.insert("std.uuid.v4".into(), sig(vec![], Type::Str));
    m.insert("std.uuid.v7".into(), sig(vec![], Type::Str));
    m.insert(
        "std.uuid.parse".into(),
        sig(vec![("s", Type::Str)], result_str3()),
    );
    m.insert(
        "std.uuid.is_valid".into(),
        sig(vec![("s", Type::Str)], Type::Bool),
    );
    m.insert("uuid.v4".into(), sig(vec![], Type::Str));
    m.insert("uuid.v7".into(), sig(vec![], Type::Str));
    m.insert(
        "uuid.parse".into(),
        sig(vec![("s", Type::Str)], result_str3()),
    );
    m.insert(
        "uuid.is_valid".into(),
        sig(vec![("s", Type::Str)], Type::Bool),
    );

    // std.log — levels, sinks, spans. Span handles are Opaque("span"),
    // dispatching `span.end` by tag. Both spellings (like json).
    let span_t = Type::Opaque("span".to_string());
    m.insert(
        "std.log.set_level".into(),
        sig(vec![("name", Type::Str)], Type::Bool),
    );
    m.insert("std.log.get_level".into(), sig(vec![], Type::Str));
    m.insert(
        "std.log.set_format".into(),
        sig(vec![("name", Type::Str)], Type::Bool),
    );
    m.insert(
        "std.log.to_file".into(),
        sig(vec![("path", Type::Str)], Type::Bool),
    );
    m.insert("std.log.to_stderr".into(), sig(vec![], Type::Unit));
    m.insert(
        "std.log.trace".into(),
        sig(vec![("msg", Type::Str)], Type::Unit),
    );
    m.insert(
        "std.log.debug".into(),
        sig(vec![("msg", Type::Str)], Type::Unit),
    );
    m.insert(
        "std.log.info".into(),
        sig(vec![("msg", Type::Str)], Type::Unit),
    );
    m.insert(
        "std.log.warn".into(),
        sig(vec![("msg", Type::Str)], Type::Unit),
    );
    m.insert(
        "std.log.error".into(),
        sig(vec![("msg", Type::Str)], Type::Unit),
    );
    m.insert(
        "std.log.span_begin".into(),
        sig(vec![("name", Type::Str)], span_t.clone()),
    );
    m.insert(
        "std.span.end".into(),
        sig(vec![("sp", span_t.clone())], Type::Int),
    );
    m.insert(
        "log.set_level".into(),
        sig(vec![("name", Type::Str)], Type::Bool),
    );
    m.insert("log.get_level".into(), sig(vec![], Type::Str));
    m.insert(
        "log.set_format".into(),
        sig(vec![("name", Type::Str)], Type::Bool),
    );
    m.insert(
        "log.to_file".into(),
        sig(vec![("path", Type::Str)], Type::Bool),
    );
    m.insert("log.to_stderr".into(), sig(vec![], Type::Unit));
    m.insert(
        "log.trace".into(),
        sig(vec![("msg", Type::Str)], Type::Unit),
    );
    m.insert(
        "log.debug".into(),
        sig(vec![("msg", Type::Str)], Type::Unit),
    );
    m.insert("log.info".into(), sig(vec![("msg", Type::Str)], Type::Unit));
    m.insert("log.warn".into(), sig(vec![("msg", Type::Str)], Type::Unit));
    m.insert(
        "log.error".into(),
        sig(vec![("msg", Type::Str)], Type::Unit),
    );
    m.insert(
        "log.span_begin".into(),
        sig(vec![("name", Type::Str)], span_t.clone()),
    );
    m.insert(
        "span.end".into(),
        sig(vec![("sp", span_t.clone())], Type::Int),
    );

    // std.sys — system information. All queries infallible (empty
    // string / fallback values, never errors). Both spellings.
    m.insert("std.sys.os".into(), sig(vec![], Type::Str));
    m.insert("std.sys.arch".into(), sig(vec![], Type::Str));
    m.insert("std.sys.cpu_count".into(), sig(vec![], Type::Int));
    m.insert("std.sys.hostname".into(), sig(vec![], Type::Str));
    m.insert("std.sys.total_mem".into(), sig(vec![], Type::Int));
    m.insert("std.sys.avail_mem".into(), sig(vec![], Type::Int));
    m.insert("sys.os".into(), sig(vec![], Type::Str));
    m.insert("sys.arch".into(), sig(vec![], Type::Str));
    m.insert("sys.cpu_count".into(), sig(vec![], Type::Int));
    m.insert("sys.hostname".into(), sig(vec![], Type::Str));
    m.insert("sys.total_mem".into(), sig(vec![], Type::Int));
    m.insert("sys.avail_mem".into(), sig(vec![], Type::Int));

    // std.args — raw argv + flag parser. Parser handles are Opaque("args"),
    // dispatching `args.*` methods by tag. Both spellings (like json).
    let args_t = Type::Opaque("args".to_string());
    m.insert(
        "std.args.get_raw".into(),
        sig(vec![], Type::Array(Box::new(Type::Str))),
    );
    m.insert("std.args.parser".into(), sig(vec![], args_t.clone()));
    m.insert(
        "std.args.str_flag".into(),
        sig(
            vec![
                ("p", args_t.clone()),
                ("name", Type::Str),
                ("default", Type::Str),
            ],
            Type::Unit,
        ),
    );
    m.insert(
        "std.args.int_flag".into(),
        sig(
            vec![
                ("p", args_t.clone()),
                ("name", Type::Str),
                ("default", Type::Int),
            ],
            Type::Unit,
        ),
    );
    m.insert(
        "std.args.bool_flag".into(),
        sig(vec![("p", args_t.clone()), ("name", Type::Str)], Type::Unit),
    );
    m.insert(
        "std.args.parse".into(),
        sig(
            vec![
                ("p", args_t.clone()),
                ("argv", Type::Array(Box::new(Type::Str))),
            ],
            Type::Bool,
        ),
    );
    m.insert(
        "std.args.get_str".into(),
        sig(vec![("p", args_t.clone()), ("name", Type::Str)], Type::Str),
    );
    m.insert(
        "std.args.get_int".into(),
        sig(vec![("p", args_t.clone()), ("name", Type::Str)], Type::Int),
    );
    m.insert(
        "std.args.get_bool".into(),
        sig(vec![("p", args_t.clone()), ("name", Type::Str)], Type::Bool),
    );
    m.insert(
        "std.args.positional".into(),
        sig(
            vec![("p", args_t.clone()), ("i", Type::Int)],
            Type::Option(Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.args.subcommand".into(),
        sig(vec![("p", args_t.clone())], Type::Str),
    );
    m.insert(
        "std.args.help".into(),
        sig(vec![("p", args_t.clone()), ("prog", Type::Str)], Type::Str),
    );
    m.insert(
        "std.args.error".into(),
        sig(vec![("p", args_t.clone())], Type::Str),
    );
    m.insert(
        "std.args.was_help".into(),
        sig(vec![("p", args_t.clone())], Type::Bool),
    );
    m.insert(
        "args.get_raw".into(),
        sig(vec![], Type::Array(Box::new(Type::Str))),
    );
    m.insert("args.parser".into(), sig(vec![], args_t.clone()));
    m.insert(
        "args.str_flag".into(),
        sig(
            vec![
                ("p", args_t.clone()),
                ("name", Type::Str),
                ("default", Type::Str),
            ],
            Type::Unit,
        ),
    );
    m.insert(
        "args.int_flag".into(),
        sig(
            vec![
                ("p", args_t.clone()),
                ("name", Type::Str),
                ("default", Type::Int),
            ],
            Type::Unit,
        ),
    );
    m.insert(
        "args.bool_flag".into(),
        sig(vec![("p", args_t.clone()), ("name", Type::Str)], Type::Unit),
    );
    m.insert(
        "args.parse".into(),
        sig(
            vec![
                ("p", args_t.clone()),
                ("argv", Type::Array(Box::new(Type::Str))),
            ],
            Type::Bool,
        ),
    );
    m.insert(
        "args.get_str".into(),
        sig(vec![("p", args_t.clone()), ("name", Type::Str)], Type::Str),
    );
    m.insert(
        "args.get_int".into(),
        sig(vec![("p", args_t.clone()), ("name", Type::Str)], Type::Int),
    );
    m.insert(
        "args.get_bool".into(),
        sig(vec![("p", args_t.clone()), ("name", Type::Str)], Type::Bool),
    );
    m.insert(
        "args.positional".into(),
        sig(
            vec![("p", args_t.clone()), ("i", Type::Int)],
            Type::Option(Box::new(Type::Str)),
        ),
    );
    m.insert(
        "args.subcommand".into(),
        sig(vec![("p", args_t.clone())], Type::Str),
    );
    m.insert(
        "args.help".into(),
        sig(vec![("p", args_t.clone()), ("prog", Type::Str)], Type::Str),
    );
    m.insert(
        "args.error".into(),
        sig(vec![("p", args_t.clone())], Type::Str),
    );
    m.insert(
        "args.was_help".into(),
        sig(vec![("p", args_t.clone())], Type::Bool),
    );
    // Pure-ZZ wrapper (zz/args/mod.zz): ArgsParser constructor namespace.
    m.insert("ArgsParser.new".into(), sig(vec![], args_t.clone()));
    m.insert(
        "crypto.jwt_encode_ed".into(),
        sig(
            vec![("payload", Type::Str), ("sk", Type::Str)],
            result_str2(),
        ),
    );
    m.insert(
        "crypto.jwt_decode_ed".into(),
        sig(vec![("token", Type::Str), ("pk", Type::Str)], result_str2()),
    );
    m.insert(
        "std.str.pad_right".into(),
        sig(
            vec![("s", Type::Str), ("len", Type::Int), ("pad", Type::Str)],
            Type::Str,
        ),
    );
    // Method-dispatch aliases for pure-ZZ string helpers
    m.insert(
        "str.repeat".into(),
        sig(vec![("s", Type::Str), ("n", Type::Int)], Type::Str),
    );
    m.insert(
        "str.count".into(),
        sig(vec![("s", Type::Str), ("sub", Type::Str)], Type::Int),
    );
    m.insert(
        "str.is_empty".into(),
        sig(vec![("s", Type::Str)], Type::Bool),
    );
    m.insert("str.reverse".into(), sig(vec![("s", Type::Str)], Type::Str));
    m.insert(
        "str.pad_left".into(),
        sig(
            vec![("s", Type::Str), ("len", Type::Int), ("pad", Type::Str)],
            Type::Str,
        ),
    );
    m.insert(
        "str.pad_right".into(),
        sig(
            vec![("s", Type::Str), ("len", Type::Int), ("pad", Type::Str)],
            Type::Str,
        ),
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
            Type::Array(Box::new(t.clone())),
        ),
    );

    // vec.* methods (for method dispatch: [1,2].push(3))
    m.insert(
        "vec.len".into(),
        sig_t(vec![("v", Type::Array(Box::new(t.clone())))], Type::Int),
    );
    // bytes.* methods (for method dispatch: `b.len()` on byte buffers).
    m.insert("bytes.len".into(), sig(vec![("v", Type::Bytes)], Type::Int));
    m.insert(
        "std.bytes.len".into(),
        sig(vec![("v", Type::Bytes)], Type::Int),
    );
    m.insert(
        "vec.push".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone()))), ("x", t.clone())],
            Type::Array(Box::new(t.clone())),
        ),
    );
    // vec.append — alias for vec.push
    m.insert(
        "vec.append".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone()))), ("x", t.clone())],
            Type::Array(Box::new(t.clone())),
        ),
    );
    m.insert(
        "vec.pop".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone())))],
            Type::Array(Box::new(t.clone())),
        ),
    );
    m.insert(
        "vec.reverse".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone())))],
            Type::Array(Box::new(t.clone())),
        ),
    );
    m.insert(
        "vec.join".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone()))), ("sep", Type::Str)],
            Type::Str,
        ),
    );
    m.insert(
        "vec.contains".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone()))), ("x", t.clone())],
            Type::Bool,
        ),
    );
    m.insert(
        "vec.sort".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone())))],
            Type::Array(Box::new(t.clone())),
        ),
    );
    m.insert(
        "vec.insert".into(),
        sig_t(
            vec![
                ("v", Type::Array(Box::new(t.clone()))),
                ("idx", Type::Int),
                ("x", t.clone()),
            ],
            Type::Array(Box::new(t.clone())),
        ),
    );
    m.insert(
        "vec.remove".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone()))), ("idx", Type::Int)],
            Type::Array(Box::new(t.clone())),
        ),
    );

    // Pure-ZZ stdlib: vec helpers (compiled from zz/collections/vec.zz)
    // min_val/max_val are generic <T> returning Option<T>;
    // sum_f/product_f handle float arrays with Kahan precision.
    m.insert(
        "std.vec.fold".into(),
        sig(
            vec![
                ("arr", Type::Array(Box::new(Type::Int))),
                ("init", Type::Int),
            ],
            Type::Int,
        ),
    );
    m.insert(
        "std.vec.sum".into(),
        sig(vec![("arr", Type::Array(Box::new(Type::Int)))], Type::Int),
    );
    m.insert(
        "std.vec.product".into(),
        sig(vec![("arr", Type::Array(Box::new(Type::Int)))], Type::Int),
    );
    // Generic min/max — return Option for empty safety
    m.insert(
        "std.vec.min_val".into(),
        sig_t(
            vec![("arr", Type::Array(Box::new(t.clone())))],
            Type::Option(Box::new(t.clone())),
        ),
    );
    m.insert(
        "std.vec.max_val".into(),
        sig_t(
            vec![("arr", Type::Array(Box::new(t.clone())))],
            Type::Option(Box::new(t.clone())),
        ),
    );
    // Float aggregation
    let float_arr = || Type::Array(Box::new(Type::Float));
    m.insert(
        "std.vec.sum_f".into(),
        sig(vec![("arr", float_arr())], Type::Float),
    );
    m.insert(
        "std.vec.product_f".into(),
        sig(vec![("arr", float_arr())], Type::Float),
    );
    m.insert(
        "std.vec.concat".into(),
        sig(
            vec![
                ("a", Type::Array(Box::new(Type::Int))),
                ("b", Type::Array(Box::new(Type::Int))),
            ],
            Type::Array(Box::new(Type::Int)),
        ),
    );
    m.insert(
        "std.vec.flatten".into(),
        sig(
            vec![(
                "arr",
                Type::Array(Box::new(Type::Array(Box::new(Type::Int)))),
            )],
            Type::Array(Box::new(Type::Int)),
        ),
    );
    m.insert(
        "std.vec.index_of".into(),
        sig(
            vec![
                ("arr", Type::Array(Box::new(Type::Int))),
                ("target", Type::Int),
            ],
            Type::Int,
        ),
    );
    m.insert(
        "std.vec.last_index_of".into(),
        sig(
            vec![
                ("arr", Type::Array(Box::new(Type::Int))),
                ("target", Type::Int),
            ],
            Type::Int,
        ),
    );
    // Method-dispatch aliases for pure-ZZ vec helpers
    m.insert(
        "vec.fold".into(),
        sig(
            vec![
                ("arr", Type::Array(Box::new(Type::Int))),
                ("init", Type::Int),
            ],
            Type::Int,
        ),
    );
    m.insert(
        "vec.sum".into(),
        sig(vec![("arr", Type::Array(Box::new(Type::Int)))], Type::Int),
    );
    m.insert(
        "vec.product".into(),
        sig(vec![("arr", Type::Array(Box::new(Type::Int)))], Type::Int),
    );
    m.insert(
        "vec.min_val".into(),
        sig_t(
            vec![("arr", Type::Array(Box::new(t.clone())))],
            Type::Option(Box::new(t.clone())),
        ),
    );
    m.insert(
        "vec.max_val".into(),
        sig_t(
            vec![("arr", Type::Array(Box::new(t.clone())))],
            Type::Option(Box::new(t.clone())),
        ),
    );
    m.insert(
        "vec.sum_f".into(),
        sig(vec![("arr", float_arr())], Type::Float),
    );
    m.insert(
        "vec.product_f".into(),
        sig(vec![("arr", float_arr())], Type::Float),
    );
    m.insert(
        "vec.concat".into(),
        sig(
            vec![
                ("a", Type::Array(Box::new(Type::Int))),
                ("b", Type::Array(Box::new(Type::Int))),
            ],
            Type::Array(Box::new(Type::Int)),
        ),
    );
    m.insert(
        "vec.flatten".into(),
        sig(
            vec![(
                "arr",
                Type::Array(Box::new(Type::Array(Box::new(Type::Int)))),
            )],
            Type::Array(Box::new(Type::Int)),
        ),
    );
    m.insert(
        "vec.index_of".into(),
        sig(
            vec![
                ("arr", Type::Array(Box::new(Type::Int))),
                ("target", Type::Int),
            ],
            Type::Int,
        ),
    );
    m.insert(
        "vec.last_index_of".into(),
        sig(
            vec![
                ("arr", Type::Array(Box::new(Type::Int))),
                ("target", Type::Int),
            ],
            Type::Int,
        ),
    );

    // option.* methods (for method dispatch: .some(1).unwrap_or(0))
    let t = Type::Named("T".to_string());
    m.insert(
        "option.unwrap".into(),
        sig_t(vec![("opt", Type::Option(Box::new(t.clone())))], t.clone()),
    );
    m.insert(
        "option.unwrap_or".into(),
        sig_t(
            vec![
                ("opt", Type::Option(Box::new(t.clone()))),
                ("default", t.clone()),
            ],
            t.clone(),
        ),
    );
    m.insert(
        "option.expect".into(),
        sig_t(
            vec![
                ("opt", Type::Option(Box::new(t.clone()))),
                ("msg", Type::Str),
            ],
            t.clone(),
        ),
    );

    // result.* methods (for method dispatch: .ok(1).unwrap_or(0))
    let t = Type::Named("T".to_string());
    let e = Type::Named("E".to_string());
    let result_t = Type::Result(Box::new(t.clone()), Box::new(e.clone()));
    m.insert(
        "result.unwrap".into(),
        FuncSig {
            generics: vec!["T".to_string(), "E".to_string()],
            bounds: Vec::new(),
            params: vec![("res".to_string(), result_t.clone())],
            has_default: vec![],
            ret: t.clone(),
            is_extern: false,
            extern_c_symbol: None,
        },
    );
    m.insert(
        "result.unwrap_or".into(),
        FuncSig {
            generics: vec!["T".to_string(), "E".to_string()],
            bounds: Vec::new(),
            params: vec![
                ("res".to_string(), result_t.clone()),
                ("default".to_string(), t.clone()),
            ],
            has_default: vec![],
            ret: t.clone(),
            is_extern: false,
            extern_c_symbol: None,
        },
    );
    m.insert(
        "result.expect".into(),
        FuncSig {
            generics: vec!["T".to_string(), "E".to_string()],
            bounds: Vec::new(),
            params: vec![
                ("res".to_string(), result_t),
                ("msg".to_string(), Type::Str),
            ],
            has_default: vec![],
            ret: t,
            is_extern: false,
            extern_c_symbol: None,
        },
    );

    // std.json
    let json_t = Type::Json;
    let t = Type::Named("T".to_string());
    m.insert(
        "std.json.parse".into(),
        sig(
            vec![("s", Type::Str)],
            Type::Result(Box::new(json_t.clone()), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.json.stringify".into(),
        sig_t(
            vec![("v", t.clone())],
            Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.json.get".into(),
        sig(
            vec![("j", json_t.clone()), ("key", Type::Str)],
            Type::Result(Box::new(json_t.clone()), Box::new(Type::Str)),
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
        sig(vec![("j", json_t.clone())], Type::Bool),
    );
    m.insert("std.json.null".into(), sig(vec![], json_t.clone()));

    // json.* short-form (for pure-ZZ and import-free use)
    m.insert(
        "json.parse".into(),
        sig(
            vec![("s", Type::Str)],
            Type::Result(Box::new(json_t.clone()), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "json.stringify".into(),
        sig_t(
            vec![("v", t.clone())],
            Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "json.get".into(),
        sig(
            vec![("j", json_t.clone()), ("key", Type::Str)],
            Type::Result(Box::new(json_t.clone()), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "json.as_str".into(),
        sig(vec![("j", json_t.clone())], Type::Str),
    );
    m.insert(
        "json.as_int".into(),
        sig(vec![("j", json_t.clone())], Type::Int),
    );
    m.insert(
        "json.as_float".into(),
        sig(vec![("j", json_t.clone())], Type::Float),
    );
    m.insert(
        "json.as_bool".into(),
        sig(vec![("j", json_t.clone())], Type::Bool),
    );
    m.insert("json.null".into(), sig(vec![], json_t.clone()));
    m.insert(
        "json.pretty".into(),
        sig(vec![("j", json_t.clone())], Type::Str),
    );
    m.insert(
        "json.type".into(),
        sig(vec![("j", json_t.clone())], Type::Str),
    );
    m.insert(
        "json.len".into(),
        sig(vec![("j", json_t.clone())], Type::Int),
    );
    m.insert(
        "json.keys".into(),
        sig(
            vec![("j", json_t.clone())],
            Type::Array(Box::new(Type::Str)),
        ),
    );
    m.insert(
        "json.has".into(),
        sig(vec![("j", json_t.clone()), ("key", Type::Str)], Type::Bool),
    );
    m.insert(
        "json.merge".into(),
        sig(
            vec![("a", json_t.clone()), ("b", json_t.clone())],
            json_t.clone(),
        ),
    );
    m.insert(
        "json.deep_get".into(),
        sig(
            vec![("j", json_t.clone()), ("path", Type::Str)],
            Type::Result(Box::new(json_t.clone()), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "json.array_push".into(),
        sig_t(
            vec![("j", json_t.clone()), ("val", t.clone())],
            json_t.clone(),
        ),
    );

    // std.json — Phase 2.6 extensions
    m.insert(
        "std.json.pretty".into(),
        sig(vec![("j", json_t.clone())], Type::Str),
    );
    m.insert(
        "std.json.type".into(),
        sig(vec![("j", json_t.clone())], Type::Str),
    );
    m.insert(
        "std.json.len".into(),
        sig(vec![("j", json_t.clone())], Type::Int),
    );
    m.insert(
        "std.json.keys".into(),
        sig(
            vec![("j", json_t.clone())],
            Type::Array(Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.json.has".into(),
        sig(vec![("j", json_t.clone()), ("key", Type::Str)], Type::Bool),
    );
    m.insert(
        "std.json.merge".into(),
        sig(
            vec![("a", json_t.clone()), ("b", json_t.clone())],
            json_t.clone(),
        ),
    );
    m.insert(
        "std.json.deep_get".into(),
        sig(
            vec![("j", json_t.clone()), ("path", Type::Str)],
            Type::Result(Box::new(json_t.clone()), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.json.array_push".into(),
        sig_t(
            vec![("j", json_t.clone()), ("val", t.clone())],
            json_t.clone(),
        ),
    );

    // Pure-ZZ json helpers (compiled from zz/json/mod.zz)
    m.insert(
        "std.json.validate".into(),
        sig(vec![("s", Type::Str)], Type::Bool),
    );
    m.insert(
        "std.json.parse_or".into(),
        sig(
            vec![("s", Type::Str), ("default", json_t.clone())],
            json_t.clone(),
        ),
    );
    m.insert(
        "std.json.parse_or_null".into(),
        sig(vec![("s", Type::Str)], json_t.clone()),
    );
    m.insert(
        "std.json.path_exists".into(),
        sig(vec![("j", json_t.clone()), ("path", Type::Str)], Type::Bool),
    );
    m.insert(
        "std.json.path_get_or".into(),
        sig(
            vec![
                ("j", json_t.clone()),
                ("path", Type::Str),
                ("default", json_t.clone()),
            ],
            json_t.clone(),
        ),
    );
    m.insert(
        "std.json.is_null".into(),
        sig(vec![("j", json_t.clone())], Type::Bool),
    );
    m.insert(
        "std.json.is_bool".into(),
        sig(vec![("j", json_t.clone())], Type::Bool),
    );
    m.insert(
        "std.json.is_number".into(),
        sig(vec![("j", json_t.clone())], Type::Bool),
    );
    m.insert(
        "std.json.is_string".into(),
        sig(vec![("j", json_t.clone())], Type::Bool),
    );
    m.insert(
        "std.json.is_array".into(),
        sig(vec![("j", json_t.clone())], Type::Bool),
    );
    m.insert(
        "std.json.is_object".into(),
        sig(vec![("j", json_t.clone())], Type::Bool),
    );
    m.insert(
        "std.json.is_empty".into(),
        sig(vec![("j", json_t.clone())], Type::Bool),
    );

    // json.* short-form for pure-ZZ helpers
    m.insert(
        "json.validate".into(),
        sig(vec![("s", Type::Str)], Type::Bool),
    );
    m.insert(
        "json.parse_or".into(),
        sig(
            vec![("s", Type::Str), ("default", json_t.clone())],
            json_t.clone(),
        ),
    );
    m.insert(
        "json.parse_or_null".into(),
        sig(vec![("s", Type::Str)], json_t.clone()),
    );
    m.insert(
        "json.path_exists".into(),
        sig(vec![("j", json_t.clone()), ("path", Type::Str)], Type::Bool),
    );
    m.insert(
        "json.path_get_or".into(),
        sig(
            vec![
                ("j", json_t.clone()),
                ("path", Type::Str),
                ("default", json_t.clone()),
            ],
            json_t.clone(),
        ),
    );
    m.insert(
        "json.is_null".into(),
        sig(vec![("j", json_t.clone())], Type::Bool),
    );
    m.insert(
        "json.is_bool".into(),
        sig(vec![("j", json_t.clone())], Type::Bool),
    );
    m.insert(
        "json.is_number".into(),
        sig(vec![("j", json_t.clone())], Type::Bool),
    );
    m.insert(
        "json.is_string".into(),
        sig(vec![("j", json_t.clone())], Type::Bool),
    );
    m.insert(
        "json.is_array".into(),
        sig(vec![("j", json_t.clone())], Type::Bool),
    );
    m.insert(
        "json.is_object".into(),
        sig(vec![("j", json_t.clone())], Type::Bool),
    );
    m.insert(
        "json.is_empty".into(),
        sig(vec![("j", json_t.clone())], Type::Bool),
    );

    // std.crypto — digests (hex strings), HMAC, CSPRNG, constant-time eq.
    // Both spellings registered (like json).
    m.insert(
        "std.crypto.sha256".into(),
        sig(vec![("s", Type::Str)], Type::Str),
    );
    m.insert(
        "std.crypto.sha512".into(),
        sig(vec![("s", Type::Str)], Type::Str),
    );
    m.insert(
        "std.crypto.hmac_sha256".into(),
        sig(vec![("key", Type::Str), ("msg", Type::Str)], Type::Str),
    );
    m.insert(
        "std.crypto.random_bytes".into(),
        sig(vec![("n", Type::Int)], Type::Str),
    );
    m.insert(
        "std.crypto.ct_eq".into(),
        sig(vec![("a", Type::Str), ("b", Type::Str)], Type::Bool),
    );
    m.insert(
        "crypto.sha256".into(),
        sig(vec![("s", Type::Str)], Type::Str),
    );
    m.insert(
        "crypto.sha512".into(),
        sig(vec![("s", Type::Str)], Type::Str),
    );
    m.insert(
        "crypto.hmac_sha256".into(),
        sig(vec![("key", Type::Str), ("msg", Type::Str)], Type::Str),
    );
    m.insert(
        "crypto.random_bytes".into(),
        sig(vec![("n", Type::Int)], Type::Str),
    );
    m.insert(
        "crypto.ct_eq".into(),
        sig(vec![("a", Type::Str), ("b", Type::Str)], Type::Bool),
    );
    // Password hashing (Argon2id + bcrypt). Hashes are PHC strings;
    // verification is total (malformed input verifies as false).
    m.insert(
        "std.crypto.argon2_hash".into(),
        sig(vec![("pw", Type::Str)], Type::Str),
    );
    m.insert(
        "std.crypto.argon2_verify".into(),
        sig(vec![("hash", Type::Str), ("pw", Type::Str)], Type::Bool),
    );
    m.insert(
        "std.crypto.bcrypt_hash".into(),
        sig(vec![("pw", Type::Str)], Type::Str),
    );
    m.insert(
        "std.crypto.bcrypt_verify".into(),
        sig(vec![("hash", Type::Str), ("pw", Type::Str)], Type::Bool),
    );
    m.insert(
        "crypto.argon2_hash".into(),
        sig(vec![("pw", Type::Str)], Type::Str),
    );
    m.insert(
        "crypto.argon2_verify".into(),
        sig(vec![("hash", Type::Str), ("pw", Type::Str)], Type::Bool),
    );
    m.insert(
        "crypto.bcrypt_hash".into(),
        sig(vec![("pw", Type::Str)], Type::Str),
    );
    m.insert(
        "crypto.bcrypt_verify".into(),
        sig(vec![("hash", Type::Str), ("pw", Type::Str)], Type::Bool),
    );

    // std.encoding
    let result_str = || Type::Result(Box::new(Type::Str), Box::new(Type::Str));
    m.insert(
        "std.encoding.base64_encode".into(),
        sig(vec![("data", Type::Str)], Type::Str),
    );
    m.insert(
        "std.encoding.base64_decode".into(),
        sig(vec![("encoded", Type::Str)], result_str()),
    );
    m.insert(
        "std.encoding.hex_encode".into(),
        sig(vec![("data", Type::Str)], Type::Str),
    );
    m.insert(
        "std.encoding.hex_decode".into(),
        sig(vec![("encoded", Type::Str)], result_str()),
    );
    m.insert(
        "std.encoding.url_encode".into(),
        sig(vec![("data", Type::Str)], Type::Str),
    );
    m.insert(
        "std.encoding.url_decode".into(),
        sig(vec![("encoded", Type::Str)], result_str()),
    );

    // std.regexp — compiled patterns are opaque handles; the tag selects
    // the `regexp.*` method namespace. `compile` is fallible (bad patterns
    // yield `.err`). Both spellings registered (like json).
    let regexp_t = Type::Opaque("regexp".to_string());
    let regexp_result = || Type::Result(Box::new(regexp_t.clone()), Box::new(Type::Str));
    m.insert(
        "std.regexp.compile".into(),
        sig(vec![("pat", Type::Str)], regexp_result()),
    );
    m.insert(
        "std.regexp.is_match".into(),
        sig(vec![("re", regexp_t.clone()), ("s", Type::Str)], Type::Bool),
    );
    m.insert(
        "std.regexp.find".into(),
        sig(
            vec![("re", regexp_t.clone()), ("s", Type::Str)],
            Type::Option(Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.regexp.replace_all".into(),
        sig(
            vec![
                ("re", regexp_t.clone()),
                ("s", Type::Str),
                ("rep", Type::Str),
            ],
            Type::Str,
        ),
    );
    m.insert(
        "std.regexp.captures".into(),
        sig(
            vec![("re", regexp_t.clone()), ("s", Type::Str)],
            Type::Array(Box::new(Type::Str)),
        ),
    );
    m.insert(
        "regexp.compile".into(),
        sig(vec![("pat", Type::Str)], regexp_result()),
    );
    m.insert(
        "regexp.is_match".into(),
        sig(vec![("re", regexp_t.clone()), ("s", Type::Str)], Type::Bool),
    );
    m.insert(
        "regexp.find".into(),
        sig(
            vec![("re", regexp_t.clone()), ("s", Type::Str)],
            Type::Option(Box::new(Type::Str)),
        ),
    );
    m.insert(
        "regexp.replace_all".into(),
        sig(
            vec![
                ("re", regexp_t.clone()),
                ("s", Type::Str),
                ("rep", Type::Str),
            ],
            Type::Str,
        ),
    );
    m.insert(
        "regexp.captures".into(),
        sig(
            vec![("re", regexp_t.clone()), ("s", Type::Str)],
            Type::Array(Box::new(Type::Str)),
        ),
    );
    // Pure-ZZ wrapper (zz/regexp/mod.zz): `Regexp` struct constructor and
    // the email-validation helper.
    m.insert(
        "Regexp.new".into(),
        sig(vec![("pat", Type::Str)], regexp_result()),
    );
    m.insert(
        "std.regexp.is_email".into(),
        sig(vec![("s", Type::Str)], Type::Bool),
    );
    m.insert(
        "regexp.is_email".into(),
        sig(vec![("s", Type::Str)], Type::Bool),
    );

    // std.http — Client
    let result_response = || Type::Result(Box::new(Type::Response), Box::new(Type::Str));
    let dict_str = || Type::Dict(Box::new(Type::Str), Box::new(Type::Str));
    m.insert(
        "std.http.get".into(),
        sig(
            vec![("url", Type::Str), ("headers", dict_str())],
            result_response(),
        ),
    );
    m.insert(
        "std.http.post".into(),
        sig(
            vec![
                ("url", Type::Str),
                ("body", Type::Str),
                ("headers", dict_str()),
            ],
            result_response(),
        ),
    );
    m.insert(
        "std.http.put".into(),
        sig(
            vec![
                ("url", Type::Str),
                ("body", Type::Str),
                ("headers", dict_str()),
            ],
            result_response(),
        ),
    );
    m.insert(
        "std.http.delete".into(),
        sig(
            vec![("url", Type::Str), ("headers", dict_str())],
            result_response(),
        ),
    );

    // std.http — Response methods (dispatched via method_namespace "http")
    m.insert(
        "http.status".into(),
        sig(vec![("res", Type::Response)], Type::Int),
    );
    m.insert(
        "http.text".into(),
        sig(vec![("res", Type::Response)], Type::Str),
    );
    m.insert(
        "http.json".into(),
        sig(vec![("res", Type::Response)], Type::Json),
    );
    m.insert(
        "http.headers".into(),
        sig(
            vec![("res", Type::Response)],
            Type::Dict(Box::new(Type::Str), Box::new(Type::Str)),
        ),
    );

    // std.http — Server methods (dispatched via method_namespace "http")
    m.insert(
        "http.log".into(),
        sig(
            vec![("server", Type::HttpServer), ("enabled", Type::Bool)],
            Type::HttpServer,
        ),
    );
    m.insert(
        "http.pipe".into(),
        sig(
            vec![
                ("server", Type::HttpServer),
                (
                    "middleware",
                    Type::Func(
                        vec![Type::Dict(Box::new(Type::Str), Box::new(Type::Str))],
                        Box::new(Type::Result(
                            Box::new(Type::Dict(Box::new(Type::Str), Box::new(Type::Str))),
                            Box::new(Type::Dict(Box::new(Type::Str), Box::new(Type::Str))),
                        )),
                    ),
                ),
            ],
            Type::HttpServer,
        ),
    );
    m.insert(
        "http.serve_dir".into(),
        sig(
            vec![("server", Type::HttpServer), ("dir", Type::Str)],
            Type::HttpServer,
        ),
    );

    // std.http — Server (per-route model)
    let server_t = Type::HttpServer;
    // Handler receives a request dict and returns a string, dict, array, or response.
    // We use a loose Func type: Dict → Str (the checker doesn't enforce return strictly).
    let handler_t = Type::Func(
        vec![Type::Dict(Box::new(Type::Str), Box::new(Type::Str))],
        Box::new(Type::Str),
    );
    m.insert("std.http.server".into(), sig(vec![], server_t.clone()));
    m.insert(
        "std.http.route_get".into(),
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
        "std.http.route_post".into(),
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
        "std.http.route_put".into(),
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
        "std.http.route_delete".into(),
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
            Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.http.listen".into(),
        sig(vec![("server", server_t), ("port", Type::Int)], Type::Unit),
    );

    // std.http — Phase 5B features
    let dict_str_str = Type::Dict(Box::new(Type::Str), Box::new(Type::Str));
    let _result_bool = Type::Result(Box::new(Type::Bool), Box::new(Type::Str));

    m.insert(
        "std.http.log".into(),
        sig(
            vec![("server", Type::HttpServer), ("enabled", Type::Bool)],
            Type::HttpServer,
        ),
    );
    m.insert(
        "std.http.pipe".into(),
        sig(
            vec![
                ("server", Type::HttpServer),
                (
                    "middleware",
                    Type::Func(
                        vec![Type::Dict(Box::new(Type::Str), Box::new(Type::Str))],
                        Box::new(Type::Result(
                            Box::new(Type::Dict(Box::new(Type::Str), Box::new(Type::Str))),
                            Box::new(Type::Dict(Box::new(Type::Str), Box::new(Type::Str))),
                        )),
                    ),
                ),
            ],
            Type::HttpServer,
        ),
    );
    m.insert(
        "std.http.serve_dir".into(),
        sig(
            vec![("server", Type::HttpServer), ("dir", Type::Str)],
            Type::HttpServer,
        ),
    );
    m.insert(
        "std.http.test".into(),
        sig(
            vec![
                ("server", Type::HttpServer),
                ("method", Type::Str),
                ("path", Type::Str),
                ("body", Type::Str),
            ],
            Type::Response,
        ),
    );
    m.insert(
        "std.http.param".into(),
        sig(
            vec![("req", dict_str_str.clone()), ("name", Type::Str)],
            Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.http.query".into(),
        sig(vec![("req", dict_str_str.clone())], dict_str_str.clone()),
    );
    m.insert(
        "std.http.header".into(),
        sig(
            vec![("req", dict_str_str.clone()), ("name", Type::Str)],
            Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.http.body_json".into(),
        sig(vec![("req", dict_str_str.clone())], Type::Json),
    );
    m.insert(
        "std.http.body_form".into(),
        sig(vec![("req", dict_str_str.clone())], dict_str_str.clone()),
    );

    // std.net — TCP networking
    let result_tcp_stream = || Type::Result(Box::new(Type::TcpStream), Box::new(Type::Str));
    let result_tcp_listener = || Type::Result(Box::new(Type::TcpListener), Box::new(Type::Str));
    let result_int = || Type::Result(Box::new(Type::Int), Box::new(Type::Str));
    let result_str = || Type::Result(Box::new(Type::Str), Box::new(Type::Str));
    let result_bool = || Type::Result(Box::new(Type::Bool), Box::new(Type::Str));
    m.insert(
        "std.net.tcp_connect".into(),
        sig(
            vec![("addr", Type::Str), ("timeout_ms", Type::Int)],
            result_tcp_stream(),
        ),
    );
    m.insert(
        "std.net.tcp_listen".into(),
        sig(vec![("addr", Type::Str)], result_tcp_listener()),
    );
    m.insert(
        "std.net.tcp_accept".into(),
        sig(vec![("listener", Type::TcpListener)], result_tcp_stream()),
    );
    m.insert(
        "std.net.tcp_write".into(),
        sig(
            vec![("stream", Type::TcpStream), ("data", Type::Str)],
            result_int(),
        ),
    );
    m.insert(
        "std.net.tcp_read".into(),
        sig(
            vec![("stream", Type::TcpStream), ("max_bytes", Type::Int)],
            result_str(),
        ),
    );
    m.insert(
        "std.net.tcp_readline".into(),
        sig(vec![("stream", Type::TcpStream)], result_str()),
    );
    m.insert(
        "std.net.tcp_close".into(),
        sig(vec![("stream", Type::TcpStream)], result_bool()),
    );
    m.insert(
        "std.net.peer_addr".into(),
        sig(vec![("stream", Type::TcpStream)], result_str()),
    );
    m.insert(
        "std.net.local_addr".into(),
        sig(vec![("stream", Type::TcpStream)], result_str()),
    );
    m.insert(
        "std.net.set_read_timeout".into(),
        sig(
            vec![("stream", Type::TcpStream), ("ms", Type::Int)],
            result_bool(),
        ),
    );
    m.insert(
        "std.net.set_write_timeout".into(),
        sig(
            vec![("stream", Type::TcpStream), ("ms", Type::Int)],
            result_bool(),
        ),
    );

    // std.fs — comprehensive non-blocking filesystem. All fallible ops
    // return `Result<_, str>` with unified `fs:<op>:<code>: <path>`
    // diagnostics (see `natives/fs/mod.rs`); predicates return `bool`.
    // Open handles are `Opaque("file")` and dispatch `file.*` methods.
    let file_t = Type::Opaque("file".to_string());
    // FS provider handles (`fs.FS` interface: Os / Mem / Tar / Embed).
    let fs_t = Type::Opaque("zzfs".to_string());
    let result_unit = || Type::Result(Box::new(Type::Unit), Box::new(Type::Str));
    let result_str = || Type::Result(Box::new(Type::Str), Box::new(Type::Str));
    let result_int = || Type::Result(Box::new(Type::Int), Box::new(Type::Str));
    let result_file = || Type::Result(Box::new(file_t.clone()), Box::new(Type::Str));
    let result_str_arr = || {
        Type::Result(
            Box::new(Type::Array(Box::new(Type::Str))),
            Box::new(Type::Str),
        )
    };
    let result_bytes = || Type::Result(Box::new(Type::Bytes), Box::new(Type::Str));
    let result_stat = || {
        Type::Result(
            Box::new(Type::Dict(Box::new(Type::Str), Box::new(Type::Str))),
            Box::new(Type::Str),
        )
    };
    for (name, params, ret) in [
        ("std.fs.read_file", vec![("path", Type::Str)], result_str()),
        ("std.fs.read", vec![("path", Type::Str)], result_str()),
        (
            "std.fs.read_to_string",
            vec![("path", Type::Str)],
            result_str(),
        ),
        (
            "std.fs.read_bytes",
            vec![("path", Type::Str)],
            result_bytes(),
        ),
        (
            "std.fs.write_file",
            vec![("path", Type::Str), ("contents", Type::Str)],
            result_unit(),
        ),
        (
            "std.fs.write",
            vec![("path", Type::Str), ("content", Type::Str)],
            result_unit(),
        ),
        (
            "std.fs.append",
            vec![("path", Type::Str), ("content", Type::Str)],
            result_unit(),
        ),
        (
            "std.fs.copy",
            vec![("src", Type::Str), ("dst", Type::Str)],
            result_unit(),
        ),
        (
            "std.fs.move",
            vec![("src", Type::Str), ("dst", Type::Str)],
            result_unit(),
        ),
        (
            "std.fs.rename",
            vec![("src", Type::Str), ("dst", Type::Str)],
            result_unit(),
        ),
        ("std.fs.exists", vec![("path", Type::Str)], Type::Bool),
        ("std.fs.is_file", vec![("path", Type::Str)], Type::Bool),
        ("std.fs.is_dir", vec![("path", Type::Str)], Type::Bool),
        (
            "std.fs.remove_file",
            vec![("path", Type::Str)],
            result_unit(),
        ),
        ("std.fs.remove", vec![("path", Type::Str)], result_unit()),
        ("std.fs.mkdir", vec![("path", Type::Str)], result_unit()),
        ("std.fs.mkdir_all", vec![("path", Type::Str)], result_unit()),
        (
            "std.fs.read_dir",
            vec![("path", Type::Str)],
            result_str_arr(),
        ),
        (
            "std.fs.readdir",
            vec![("path", Type::Str)],
            result_str_arr(),
        ),
        (
            "std.fs.remove_dir_all",
            vec![("path", Type::Str)],
            result_unit(),
        ),
        (
            "std.fs.walk_dir",
            vec![("path", Type::Str)],
            result_str_arr(),
        ),
        ("std.fs.stat", vec![("path", Type::Str)], result_stat()),
        (
            "std.fs.open",
            vec![("path", Type::Str), ("mode", Type::Str)],
            result_file(),
        ),
        (
            "std.fs.read_chunk",
            vec![("f", file_t.clone()), ("n", Type::Int)],
            result_str(),
        ),
        (
            "std.fs.read_chunk_bytes",
            vec![("f", file_t.clone()), ("n", Type::Int)],
            result_bytes(),
        ),
        (
            "std.fs.write_chunk",
            vec![("f", file_t.clone()), ("data", Type::Str)],
            result_int(),
        ),
        (
            "std.fs.seek",
            vec![("f", file_t.clone()), ("pos", Type::Int)],
            result_int(),
        ),
        ("std.fs.flush", vec![("f", file_t.clone())], result_unit()),
        ("std.fs.close", vec![("f", file_t.clone())], result_unit()),
        // Pure cross-platform path lexing (total, no I/O).
        ("std.fs.normalize", vec![("path", Type::Str)], Type::Str),
        (
            "std.fs.join",
            vec![("a", Type::Str), ("b", Type::Str)],
            Type::Str,
        ),
        ("std.fs.basename", vec![("path", Type::Str)], Type::Str),
        ("std.fs.dirname", vec![("path", Type::Str)], Type::Str),
        ("std.fs.is_absolute", vec![("path", Type::Str)], Type::Bool),
        ("std.fs.extension", vec![("path", Type::Str)], Type::Str),
        // FS providers + `*_at` family (see `natives/fs/vfs.rs`).
        (
            "std.fs.osfs",
            vec![],
            Type::Result(Box::new(fs_t.clone()), Box::new(Type::Str)),
        ),
        (
            "std.fs.memfs",
            vec![],
            Type::Result(Box::new(fs_t.clone()), Box::new(Type::Str)),
        ),
        (
            "std.fs.tarfs",
            vec![("path", Type::Str)],
            Type::Result(Box::new(fs_t.clone()), Box::new(Type::Str)),
        ),
        (
            "std.fs.embedfs",
            vec![],
            Type::Result(Box::new(fs_t.clone()), Box::new(Type::Str)),
        ),
        (
            "std.fs.read_to_string_at",
            vec![("fsys", fs_t.clone()), ("path", Type::Str)],
            result_str(),
        ),
        (
            "std.fs.read_bytes_at",
            vec![("fsys", fs_t.clone()), ("path", Type::Str)],
            result_bytes(),
        ),
        (
            "std.fs.write_at",
            vec![
                ("fsys", fs_t.clone()),
                ("path", Type::Str),
                ("content", Type::Str),
            ],
            result_unit(),
        ),
        (
            "std.fs.append_at",
            vec![
                ("fsys", fs_t.clone()),
                ("path", Type::Str),
                ("content", Type::Str),
            ],
            result_unit(),
        ),
        (
            "std.fs.exists_at",
            vec![("fsys", fs_t.clone()), ("path", Type::Str)],
            Type::Bool,
        ),
        (
            "std.fs.is_file_at",
            vec![("fsys", fs_t.clone()), ("path", Type::Str)],
            Type::Bool,
        ),
        (
            "std.fs.is_dir_at",
            vec![("fsys", fs_t.clone()), ("path", Type::Str)],
            Type::Bool,
        ),
        (
            "std.fs.read_dir_at",
            vec![("fsys", fs_t.clone()), ("path", Type::Str)],
            result_str_arr(),
        ),
        (
            "std.fs.mkdir_all_at",
            vec![("fsys", fs_t.clone()), ("path", Type::Str)],
            result_unit(),
        ),
        (
            "std.fs.remove_file_at",
            vec![("fsys", fs_t.clone()), ("path", Type::Str)],
            result_unit(),
        ),
        (
            "File.open",
            vec![("path", Type::Str), ("mode", Type::Str)],
            result_file(),
        ),
        (
            "file.read_chunk",
            vec![("f", file_t.clone()), ("n", Type::Int)],
            result_str(),
        ),
        (
            "file.read_chunk_bytes",
            vec![("f", file_t.clone()), ("n", Type::Int)],
            result_bytes(),
        ),
        (
            "file.write_chunk",
            vec![("f", file_t.clone()), ("data", Type::Str)],
            result_int(),
        ),
        (
            "file.seek",
            vec![("f", file_t.clone()), ("pos", Type::Int)],
            result_int(),
        ),
        ("file.flush", vec![("f", file_t.clone())], result_unit()),
        ("file.close", vec![("f", file_t.clone())], result_unit()),
    ] {
        m.insert(name.into(), sig(params, ret));
    }

    // std.env
    m.insert(
        "std.env.get_var".into(),
        sig(vec![("name", Type::Str)], Type::Option(Box::new(Type::Str))),
    );
    m.insert(
        "std.env.var".into(),
        sig(
            vec![("name", Type::Str)],
            Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.env.args".into(),
        sig(vec![], Type::Array(Box::new(Type::Str))),
    );

    // Built-in: `typeof(v)` — accepts any value, returns its type name.
    let t = Type::Named("T".to_string());
    m.insert("typeof".into(), sig_t(vec![("v", t.clone())], Type::Str));

    // Built-in: `append(arr, val)` — returns array with val appended.
    // Used as a statement: compiler write-back stores result to arr.
    let t_arr = Type::Array(Box::new(Type::Named("T".to_string())));
    m.insert(
        "append".into(),
        sig_t(vec![("arr", t_arr.clone()), ("val", t.clone())], t_arr),
    );

    // Built-in conversions.
    // `str(v)` — stringify any value (total).
    m.insert("str".into(), sig_t(vec![("v", t.clone())], Type::Str));
    // `int(v)` — parse a string, truncate a float, or pass through an int.
    // Invalid parses yield `.none`.
    m.insert(
        "int".into(),
        sig_t(vec![("v", t.clone())], Type::Option(Box::new(Type::Int))),
    );
    // `float(v)` — widen an int to float, identity for float, parse from str.
    m.insert("float".into(), sig_t(vec![("v", t.clone())], Type::Float));

    // std.math — pure-ZZ integer implementations (from zz/math/mod.zz)
    // These shadow the native Rust versions at type-check time.
    // Native Rust versions still handle float overloads at runtime.
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

    // ── std.math utilities & rounding ──
    m.insert(
        "std.math.round".into(),
        sig_t(vec![("x", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.trunc".into(),
        sig_t(vec![("x", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.clamp".into(),
        sig_t(
            vec![("val", t.clone()), ("min", t.clone()), ("max", t.clone())],
            Type::Float,
        ),
    );
    m.insert(
        "std.math.signum".into(),
        sig_t(vec![("x", t.clone())], t.clone()),
    );
    m.insert(
        "std.math.hypot".into(),
        sig_t(vec![("x", t.clone()), ("y", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.is_nan".into(),
        sig_t(vec![("x", t.clone())], Type::Bool),
    );
    m.insert(
        "std.math.is_inf".into(),
        sig_t(vec![("x", t.clone())], Type::Bool),
    );

    // ── std.math number theory ──
    m.insert(
        "std.math.root".into(),
        sig_t(vec![("x", t.clone()), ("n", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.isqrt".into(),
        sig(
            vec![("n", Type::Int)],
            Type::Result(Box::new(Type::Int), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.math.factorial".into(),
        sig(
            vec![("n", Type::Int)],
            Type::Result(Box::new(Type::Int), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.math.gcd".into(),
        sig(vec![("a", Type::Int), ("b", Type::Int)], Type::Int),
    );
    m.insert(
        "std.math.lcm".into(),
        sig(vec![("a", Type::Int), ("b", Type::Int)], Type::Int),
    );

    // ── std.math trigonometry ──
    m.insert(
        "std.math.sin".into(),
        sig_t(vec![("x", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.cos".into(),
        sig_t(vec![("x", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.tan".into(),
        sig_t(vec![("x", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.asin".into(),
        sig_t(vec![("x", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.acos".into(),
        sig_t(vec![("x", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.atan".into(),
        sig_t(vec![("x", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.sin_deg".into(),
        sig_t(vec![("x", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.cos_deg".into(),
        sig_t(vec![("x", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.tan_deg".into(),
        sig_t(vec![("x", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.to_radians".into(),
        sig_t(vec![("deg", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.to_degrees".into(),
        sig_t(vec![("rad", t.clone())], Type::Float),
    );

    // ── std.math logarithms & exponents ──
    m.insert(
        "std.math.log".into(),
        sig_t(vec![("x", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.log10".into(),
        sig_t(vec![("x", t.clone())], Type::Float),
    );
    m.insert(
        "std.math.exp".into(),
        sig_t(vec![("x", t.clone())], Type::Float),
    );

    // ── std.math linear algebra ──
    let float_arr = Type::Array(Box::new(Type::Float));
    let float_arr_arr = Type::Array(Box::new(Type::Array(Box::new(Type::Float))));
    m.insert(
        "std.math.dot_product".into(),
        sig(
            vec![("v1", float_arr.clone()), ("v2", float_arr.clone())],
            Type::Result(Box::new(Type::Float), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.math.magnitude".into(),
        sig(vec![("v", float_arr.clone())], Type::Float),
    );
    m.insert(
        "std.math.matrix_mul".into(),
        sig(
            vec![("m1", float_arr_arr.clone()), ("m2", float_arr_arr.clone())],
            Type::Result(Box::new(float_arr_arr), Box::new(Type::Str)),
        ),
    );

    // ── std.math statistics & random ──
    m.insert(
        "std.math.mean".into(),
        sig(
            vec![("list", float_arr.clone())],
            Type::Result(Box::new(Type::Float), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.math.median".into(),
        sig(
            vec![("list", float_arr.clone())],
            Type::Result(Box::new(Type::Float), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.math.rand_range".into(),
        sig_t(vec![("min", t.clone()), ("max", t.clone())], Type::Float),
    );

    // Pure-ZZ stdlib: math helpers (compiled from zz/math/mod.zz)
    // min/max are generic <T>; min_arr/max_arr return Option<T>;
    // sum_f/product_f/mean_f/median_f handle float arrays with Kahan precision.
    m.insert(
        "std.math.sum".into(),
        sig(vec![("arr", Type::Array(Box::new(Type::Int)))], Type::Int),
    );
    m.insert(
        "std.math.product".into(),
        sig(vec![("arr", Type::Array(Box::new(Type::Int)))], Type::Int),
    );
    m.insert(
        "std.math.count".into(),
        sig(
            vec![
                ("arr", Type::Array(Box::new(Type::Int))),
                ("target", Type::Int),
            ],
            Type::Int,
        ),
    );
    // Generic min/max — work for int, float, str (anything comparable)
    m.insert(
        "std.math.min".into(),
        sig_t(vec![("a", t.clone()), ("b", t.clone())], t.clone()),
    );
    m.insert(
        "std.math.max".into(),
        sig_t(vec![("a", t.clone()), ("b", t.clone())], t.clone()),
    );
    m.insert(
        "std.math.is_even".into(),
        sig(vec![("n", Type::Int)], Type::Bool),
    );
    m.insert(
        "std.math.is_odd".into(),
        sig(vec![("n", Type::Int)], Type::Bool),
    );
    // Generic array min/max — return Option for empty safety
    m.insert(
        "std.math.min_arr".into(),
        sig_t(
            vec![("arr", Type::Array(Box::new(t.clone())))],
            Type::Option(Box::new(t.clone())),
        ),
    );
    m.insert(
        "std.math.max_arr".into(),
        sig_t(
            vec![("arr", Type::Array(Box::new(t.clone())))],
            Type::Option(Box::new(t.clone())),
        ),
    );
    // Float aggregation — Kahan compensated summation
    let float_arr = || Type::Array(Box::new(Type::Float));
    m.insert(
        "std.math.sum_f".into(),
        sig(vec![("arr", float_arr())], Type::Float),
    );
    m.insert(
        "std.math.product_f".into(),
        sig(vec![("arr", float_arr())], Type::Float),
    );
    m.insert(
        "std.math.mean_f".into(),
        sig(
            vec![("arr", float_arr())],
            Type::Result(Box::new(Type::Float), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.math.median_f".into(),
        sig(
            vec![("arr", float_arr())],
            Type::Result(Box::new(Type::Float), Box::new(Type::Str)),
        ),
    );
    // Method-dispatch aliases for pure-ZZ math helpers
    m.insert(
        "math.sum".into(),
        sig(vec![("arr", Type::Array(Box::new(Type::Int)))], Type::Int),
    );
    m.insert(
        "math.product".into(),
        sig(vec![("arr", Type::Array(Box::new(Type::Int)))], Type::Int),
    );
    m.insert(
        "math.count".into(),
        sig(
            vec![
                ("arr", Type::Array(Box::new(Type::Int))),
                ("target", Type::Int),
            ],
            Type::Int,
        ),
    );
    m.insert(
        "math.min".into(),
        sig_t(vec![("a", t.clone()), ("b", t.clone())], t.clone()),
    );
    m.insert(
        "math.max".into(),
        sig_t(vec![("a", t.clone()), ("b", t.clone())], t.clone()),
    );
    m.insert(
        "math.is_even".into(),
        sig(vec![("n", Type::Int)], Type::Bool),
    );
    m.insert(
        "math.is_odd".into(),
        sig(vec![("n", Type::Int)], Type::Bool),
    );
    m.insert(
        "math.min_arr".into(),
        sig_t(
            vec![("arr", Type::Array(Box::new(t.clone())))],
            Type::Option(Box::new(t.clone())),
        ),
    );
    m.insert(
        "math.max_arr".into(),
        sig_t(
            vec![("arr", Type::Array(Box::new(t.clone())))],
            Type::Option(Box::new(t.clone())),
        ),
    );
    m.insert(
        "math.sum_f".into(),
        sig(vec![("arr", float_arr())], Type::Float),
    );
    m.insert(
        "math.product_f".into(),
        sig(vec![("arr", float_arr())], Type::Float),
    );
    m.insert(
        "math.mean_f".into(),
        sig(
            vec![("arr", float_arr())],
            Type::Result(Box::new(Type::Float), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "math.median_f".into(),
        sig(
            vec![("arr", float_arr())],
            Type::Result(Box::new(Type::Float), Box::new(Type::Str)),
        ),
    );

    // std.time
    m.insert("std.time.now_ms".into(), sig(vec![], Type::Int));
    m.insert(
        "std.time.sleep_ms".into(),
        sig(vec![("ms", Type::Int)], Type::Unit),
    );
    m.insert("std.time.now_nanos".into(), sig(vec![], Type::Int));
    m.insert("std.time.now_micros".into(), sig(vec![], Type::Int));
    m.insert("std.time.monotonic_nanos".into(), sig(vec![], Type::Int));
    m.insert(
        "std.time.sleep_micros".into(),
        sig(vec![("micros", Type::Int)], Type::Unit),
    );
    // Bare `time.*` aliases (like `vec.*`): back direct calls, method
    // dispatch, and references from pure-ZZ helpers (`Duration.sleep`).
    m.insert("time.now_ms".into(), sig(vec![], Type::Int));
    m.insert(
        "time.sleep_ms".into(),
        sig(vec![("ms", Type::Int)], Type::Unit),
    );
    m.insert("time.now_nanos".into(), sig(vec![], Type::Int));
    m.insert("time.now_micros".into(), sig(vec![], Type::Int));
    m.insert("time.monotonic_nanos".into(), sig(vec![], Type::Int));
    m.insert(
        "time.sleep_micros".into(),
        sig(vec![("micros", Type::Int)], Type::Unit),
    );
    // Pure-ZZ durations (zz/time/mod.zz): spans as integer microseconds.
    m.insert(
        "time.micros".into(),
        sig(vec![("us", Type::Int)], Type::Int),
    );
    m.insert(
        "time.millis".into(),
        sig(vec![("ms", Type::Int)], Type::Int),
    );
    m.insert("time.secs".into(), sig(vec![("s", Type::Int)], Type::Int));
    m.insert(
        "time.to_micros".into(),
        sig(vec![("d", Type::Int)], Type::Int),
    );
    m.insert(
        "time.to_millis".into(),
        sig(vec![("d", Type::Int)], Type::Int),
    );
    m.insert(
        "time.to_secs".into(),
        sig(vec![("d", Type::Int)], Type::Int),
    );
    m.insert(
        "time.to_nanos".into(),
        sig(vec![("d", Type::Int)], Type::Int),
    );
    m.insert("time.sleep".into(), sig(vec![("d", Type::Int)], Type::Unit));

    // std.colors — pure-ZZ ANSI styling (zz/colors/mod.zz). Every wrapper
    // takes the text first so it composes with `|>` pipelines. Both
    // spellings registered (like `std.json.*` / `json.*`).
    let color_wraps = [
        "black",
        "red",
        "green",
        "yellow",
        "blue",
        "magenta",
        "cyan",
        "white",
        "bright_black",
        "bright_red",
        "bright_green",
        "bright_yellow",
        "bright_blue",
        "bright_magenta",
        "bright_cyan",
        "bright_white",
        "bg_black",
        "bg_red",
        "bg_green",
        "bg_yellow",
        "bg_blue",
        "bg_magenta",
        "bg_cyan",
        "bg_white",
        "bold",
        "dim",
        "italic",
        "underline",
        "reset",
        "strip",
    ];
    for name in color_wraps {
        let s = sig(vec![("s", Type::Str)], Type::Str);
        m.insert(format!("std.colors.{name}"), s.clone());
        m.insert(format!("colors.{name}"), s);
    }
    for (name, params, ret) in [
        ("clamp255", vec![("v", Type::Int)], Type::Int),
        (
            "rgb",
            vec![
                ("s", Type::Str),
                ("r", Type::Int),
                ("g", Type::Int),
                ("b", Type::Int),
            ],
            Type::Str,
        ),
        (
            "bg_rgb",
            vec![
                ("s", Type::Str),
                ("r", Type::Int),
                ("g", Type::Int),
                ("b", Type::Int),
            ],
            Type::Str,
        ),
        ("hex_val", vec![("c", Type::Str)], Type::Int),
        (
            "hex_byte",
            vec![("hi", Type::Str), ("lo", Type::Str)],
            Type::Int,
        ),
        (
            "hex",
            vec![("s", Type::Str), ("code", Type::Str)],
            Type::Str,
        ),
        (
            "hex6",
            vec![
                ("s", Type::Str),
                ("digits", Type::Array(Box::new(Type::Str))),
            ],
            Type::Str,
        ),
        (
            "hex3",
            vec![
                ("s", Type::Str),
                ("digits", Type::Array(Box::new(Type::Str))),
            ],
            Type::Str,
        ),
    ] {
        let s = sig(params, ret);
        m.insert(format!("std.colors.{name}"), s.clone());
        m.insert(format!("colors.{name}"), s);
    }

    // std.sqlz — SQLite foundation (CANONICAL module name).
    //
    // `open(path) -> db`, `exec(db, sql) -> int` (rows changed),
    // `query(db, sql) -> [T]` (rows mapped to structs), `close(db)`.
    // The SQL param is checked by `verify_sql_params` (Fmt segments become
    // `?N` bound params); `query`'s return unifies with the caller's
    // annotation (`let users: [User] = sqlz.query(...)`) so the runtime can
    // map columns positionally into that struct.
    //
    // `std.db` / `db.*` are zero-overhead aliases (identical sigs below).
    let db_row_t = Type::Named("T".to_string());
    m.insert(
        "std.sqlz.open".into(),
        sig(vec![("path", Type::Str)], Type::Db),
    );
    m.insert(
        "std.sqlz.exec".into(),
        sig(vec![("db", Type::Db), ("sql", Type::Str)], Type::Int),
    );
    m.insert(
        "std.sqlz.query".into(),
        sig_t(
            vec![("db", Type::Db), ("sql", Type::Str)],
            Type::Array(Box::new(db_row_t.clone())),
        ),
    );
    m.insert(
        "std.sqlz.close".into(),
        sig(vec![("db", Type::Db)], Type::Unit),
    );
    // Method-dispatch aliases (`sqlz.open(...)` after `import std.sqlz`).
    m.insert("sqlz.open".into(), sig(vec![("path", Type::Str)], Type::Db));
    m.insert(
        "sqlz.exec".into(),
        sig(vec![("db", Type::Db), ("sql", Type::Str)], Type::Int),
    );
    m.insert(
        "sqlz.query".into(),
        sig_t(
            vec![("db", Type::Db), ("sql", Type::Str)],
            Type::Array(Box::new(db_row_t.clone())),
        ),
    );
    m.insert("sqlz.close".into(), sig(vec![("db", Type::Db)], Type::Unit));
    // Alias: `std.db` / `db.*` point directly at the `std.sqlz` sigs.
    m.insert(
        "std.db.open".into(),
        sig(vec![("path", Type::Str)], Type::Db),
    );
    m.insert(
        "std.db.exec".into(),
        sig(vec![("db", Type::Db), ("sql", Type::Str)], Type::Int),
    );
    m.insert(
        "std.db.query".into(),
        sig_t(
            vec![("db", Type::Db), ("sql", Type::Str)],
            Type::Array(Box::new(db_row_t.clone())),
        ),
    );
    m.insert(
        "std.db.close".into(),
        sig(vec![("db", Type::Db)], Type::Unit),
    );
    m.insert("db.open".into(), sig(vec![("path", Type::Str)], Type::Db));
    m.insert(
        "db.exec".into(),
        sig(vec![("db", Type::Db), ("sql", Type::Str)], Type::Int),
    );
    m.insert(
        "db.query".into(),
        sig_t(
            vec![("db", Type::Db), ("sql", Type::Str)],
            Type::Array(Box::new(db_row_t.clone())),
        ),
    );
    m.insert("db.close".into(), sig(vec![("db", Type::Db)], Type::Unit));

    // std.sqlz.postgres — async-minded PostgreSQL wire-protocol driver.
    //
    // `connect(conninfo) -> db` (URL or keyword form), `exec(db, sql)`,
    // `query(db, sql) -> [T]`, `close(db)`. Import as
    // `import std.sqlz.postgres as pg`. Handles are `Type::Db`, so method
    // syntax (`mydb.query(...)`) dispatches through the shared sqlz path;
    // `pg.query(db, sql)` is the explicit-receiver spelling.
    let pg_row_t = Type::Named("T".to_string());
    m.insert(
        "std.sqlz.postgres.connect".into(),
        sig(vec![("conninfo", Type::Str)], Type::Db),
    );
    m.insert(
        "std.sqlz.postgres.exec".into(),
        sig(vec![("db", Type::Db), ("sql", Type::Str)], Type::Int),
    );
    m.insert(
        "std.sqlz.postgres.query".into(),
        sig_t(
            vec![("db", Type::Db), ("sql", Type::Str)],
            Type::Array(Box::new(pg_row_t.clone())),
        ),
    );
    m.insert(
        "std.sqlz.postgres.close".into(),
        sig(vec![("db", Type::Db)], Type::Unit),
    );

    // std.sqlz.mysql — MySQL wire-protocol driver (binary prepared
    // statements). Same shape as postgres: import as
    // `import std.sqlz.mysql as my`.
    let my_row_t = Type::Named("T".to_string());
    m.insert(
        "std.sqlz.mysql.connect".into(),
        sig(vec![("conninfo", Type::Str)], Type::Db),
    );
    m.insert(
        "std.sqlz.mysql.exec".into(),
        sig(vec![("db", Type::Db), ("sql", Type::Str)], Type::Int),
    );
    m.insert(
        "std.sqlz.mysql.query".into(),
        sig_t(
            vec![("db", Type::Db), ("sql", Type::Str)],
            Type::Array(Box::new(my_row_t.clone())),
        ),
    );
    m.insert(
        "std.sqlz.mysql.close".into(),
        sig(vec![("db", Type::Db)], Type::Unit),
    );

    // sqlz.transaction — unified closure transactions for all backends.
    //
    // `transaction(db, fn(tx) { ... }) -> Result[T, str]`: BEGIN, run the
    // closure with the transaction handle, COMMIT on clean return
    // (`.ok(value)`), ROLLBACK on closure error (`.err(message)`).
    // `db.transaction` is the same zero-overhead alias pattern as the
    // other sqlz methods.
    let tx_t = Type::Named("T".to_string());
    let tx_sig = sig_t(
        vec![
            ("db", Type::Db),
            ("f", Type::Func(vec![Type::Db], Box::new(tx_t.clone()))),
        ],
        Type::Result(Box::new(tx_t.clone()), Box::new(Type::Str)),
    );
    m.insert("std.sqlz.transaction".into(), tx_sig.clone());
    m.insert("sqlz.transaction".into(), tx_sig.clone());
    m.insert("std.db.transaction".into(), tx_sig.clone());
    m.insert("db.transaction".into(), tx_sig);

    // std.chan — concurrency primitives
    let t = Type::Named("T".to_string());
    m.insert("std.chan".into(), sig(vec![], Type::Chan));
    m.insert(
        "std.chan.send".into(),
        sig_t(vec![("ch", Type::Chan), ("v", t.clone())], Type::Unit),
    );
    m.insert(
        "std.chan.recv".into(),
        sig_t(vec![("ch", Type::Chan)], t.clone()),
    );
    m.insert(
        "std.chan.try_recv".into(),
        sig_t(vec![("ch", Type::Chan)], Type::Option(Box::new(t.clone()))),
    );

    // std.task — spawn and join
    m.insert(
        "std.task.spawn".into(),
        sig_t(
            vec![("f", Type::Func(vec![t.clone()], Box::new(t.clone())))],
            Type::TaskJoin,
        ),
    );
    m.insert(
        "std.task.join".into(),
        sig_t(vec![("handle", Type::TaskJoin)], t.clone()),
    );
    m.insert(
        "std.task.try_join".into(),
        sig_t(
            vec![("handle", Type::TaskJoin)],
            Type::Option(Box::new(Type::Result(
                Box::new(t.clone()),
                Box::new(Type::Str),
            ))),
        ),
    );

    // ── Test assertions (lockstep with natives/assert) ──────────────
    // Top-level builtins available without import; also `std.test.*`.
    let assert_sig = FuncSig {
        generics: Vec::new(),
        bounds: Vec::new(),
        params: vec![("cond".to_string(), Type::Bool)],
        has_default: vec![false],
        ret: Type::Unit,
        is_extern: false,
        extern_c_symbol: None,
    };
    for name in ["assert", "std.test.assert"] {
        m.insert(name.into(), assert_sig.clone());
    }
    let t_eq = Type::Named("T".to_string());
    let assert_eq_sig = FuncSig {
        generics: vec!["T".to_string()],
        bounds: Vec::new(),
        params: vec![
            ("left".to_string(), t_eq.clone()),
            ("right".to_string(), t_eq.clone()),
        ],
        has_default: vec![false, false],
        ret: Type::Unit,
        is_extern: false,
        extern_c_symbol: None,
    };
    for name in ["assert_eq", "std.test.assert_eq"] {
        m.insert(name.into(), assert_eq_sig.clone());
    }
    let assert_ne_sig = FuncSig {
        generics: vec!["T".to_string()],
        bounds: Vec::new(),
        params: vec![
            ("left".to_string(), t_eq.clone()),
            ("right".to_string(), t_eq.clone()),
        ],
        has_default: vec![false, false],
        ret: Type::Unit,
        is_extern: false,
        extern_c_symbol: None,
    };
    for name in ["assert_ne", "std.test.assert_ne"] {
        m.insert(name.into(), assert_ne_sig.clone());
    }
    let approx_sig = FuncSig {
        generics: Vec::new(),
        bounds: Vec::new(),
        params: vec![
            ("left".to_string(), Type::Float),
            ("right".to_string(), Type::Float),
            ("epsilon".to_string(), Type::Float),
        ],
        has_default: vec![false, false, false],
        ret: Type::Unit,
        is_extern: false,
        extern_c_symbol: None,
    };
    for name in ["assert_approx_eq", "std.test.assert_approx_eq"] {
        m.insert(name.into(), approx_sig.clone());
    }
    let fail_sig = FuncSig {
        generics: Vec::new(),
        bounds: Vec::new(),
        params: vec![("msg".to_string(), Type::Str)],
        has_default: vec![false],
        ret: Type::Unit,
        is_extern: false,
        extern_c_symbol: None,
    };
    for name in ["fail", "panic", "std.test.fail", "std.test.panic"] {
        m.insert(name.into(), fail_sig.clone());
    }

    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stdlib_consts;

    #[test]
    fn has_all_modules() {
        let funcs = stdlib_funcs();
        assert!(funcs.contains_key("println"));
        assert!(funcs.contains_key("print"));
        assert!(funcs.contains_key("input"));
        assert!(funcs.contains_key("std.str.length"));
        assert!(funcs.contains_key("std.vec.push"));
        assert!(funcs.contains_key("std.json.parse"));
        assert!(funcs.contains_key("std.json.stringify"));
        assert!(funcs.contains_key("std.http.server"));
        assert!(funcs.contains_key("std.http.handle"));
        assert!(funcs.contains_key("std.http.listen"));
        assert!(funcs.contains_key("std.http.log"));
        assert!(funcs.contains_key("std.http.pipe"));
        assert!(funcs.contains_key("std.http.serve_dir"));
        assert!(funcs.contains_key("std.http.test"));
        assert!(funcs.contains_key("std.http.param"));
        assert!(funcs.contains_key("std.http.query"));
        assert!(funcs.contains_key("std.http.header"));
        assert!(funcs.contains_key("std.http.body_json"));
        assert!(funcs.contains_key("std.http.body_form"));
        assert!(funcs.contains_key("std.fs.read_to_string"));
        assert!(funcs.contains_key("std.fs.read_bytes"));
        assert!(funcs.contains_key("std.fs.write"));
        assert!(funcs.contains_key("std.fs.append"));
        assert!(funcs.contains_key("std.fs.copy"));
        assert!(funcs.contains_key("std.fs.move"));
        assert!(funcs.contains_key("std.fs.rename"));
        assert!(funcs.contains_key("std.fs.is_file"));
        assert!(funcs.contains_key("std.fs.is_dir"));
        assert!(funcs.contains_key("std.fs.remove_file"));
        assert!(funcs.contains_key("std.fs.mkdir"));
        assert!(funcs.contains_key("std.fs.mkdir_all"));
        assert!(funcs.contains_key("std.fs.read_dir"));
        assert!(funcs.contains_key("std.fs.remove_dir_all"));
        assert!(funcs.contains_key("std.fs.walk_dir"));
        assert!(funcs.contains_key("std.fs.stat"));
        assert!(funcs.contains_key("std.fs.open"));
        assert!(funcs.contains_key("std.fs.read_chunk"));
        assert!(funcs.contains_key("std.fs.write_chunk"));
        assert!(funcs.contains_key("std.fs.seek"));
        assert!(funcs.contains_key("std.fs.flush"));
        assert!(funcs.contains_key("std.fs.close"));
        assert!(funcs.contains_key("File.open"));
        assert!(funcs.contains_key("file.read_chunk"));
        assert!(funcs.contains_key("file.write_chunk"));
        assert!(funcs.contains_key("file.seek"));
        assert!(funcs.contains_key("file.flush"));
        assert!(funcs.contains_key("file.close"));
        assert!(funcs.contains_key("std.env.get_var"));
        assert!(funcs.contains_key("std.env.var"));
        assert!(funcs.contains_key("std.env.args"));
        assert!(funcs.contains_key("std.math.abs"));
        assert!(funcs.contains_key("std.math.random"));
        assert!(funcs.contains_key("std.math.round"));
        assert!(funcs.contains_key("std.math.trunc"));
        assert!(funcs.contains_key("std.math.clamp"));
        assert!(funcs.contains_key("std.math.isqrt"));
        assert!(funcs.contains_key("std.math.factorial"));
        assert!(funcs.contains_key("std.math.gcd"));
        assert!(funcs.contains_key("std.math.lcm"));
        assert!(funcs.contains_key("std.math.sin"));
        assert!(funcs.contains_key("std.math.cos"));
        assert!(funcs.contains_key("std.math.tan"));
        assert!(funcs.contains_key("std.math.asin"));
        assert!(funcs.contains_key("std.math.acos"));
        assert!(funcs.contains_key("std.math.atan"));
        assert!(funcs.contains_key("std.math.sin_deg"));
        assert!(funcs.contains_key("std.math.cos_deg"));
        assert!(funcs.contains_key("std.math.tan_deg"));
        assert!(funcs.contains_key("std.math.to_radians"));
        assert!(funcs.contains_key("std.math.to_degrees"));
        assert!(funcs.contains_key("std.math.log"));
        assert!(funcs.contains_key("std.math.log10"));
        assert!(funcs.contains_key("std.math.exp"));
        assert!(funcs.contains_key("std.math.dot_product"));
        assert!(funcs.contains_key("std.math.magnitude"));
        assert!(funcs.contains_key("std.math.matrix_mul"));
        assert!(funcs.contains_key("std.math.mean"));
        assert!(funcs.contains_key("std.math.median"));
        assert!(funcs.contains_key("std.math.rand_range"));
        assert!(funcs.contains_key("std.time.now_ms"));
        assert!(funcs.contains_key("typeof"));
        assert!(funcs.contains_key("str"));
        assert!(funcs.contains_key("int"));
        assert!(funcs.contains_key("float"));
        assert!(funcs.contains_key("append"));
        assert!(funcs.contains_key("std.task.spawn"));
        assert!(funcs.contains_key("std.task.join"));
        assert!(funcs.contains_key("std.task.try_join"));
        assert!(funcs.contains_key("std.sqlz.open"));
        assert!(funcs.contains_key("std.sqlz.exec"));
        assert!(funcs.contains_key("std.sqlz.query"));
        assert!(funcs.contains_key("std.sqlz.close"));
        assert!(funcs.contains_key("sqlz.open"));
        assert!(funcs.contains_key("sqlz.exec"));
        assert!(funcs.contains_key("sqlz.query"));
        assert!(funcs.contains_key("sqlz.close"));
        assert!(funcs.contains_key("std.db.open"));
        assert!(funcs.contains_key("std.db.exec"));
        assert!(funcs.contains_key("std.db.query"));
        assert!(funcs.contains_key("std.db.close"));
        assert!(funcs.contains_key("db.open"));
        assert!(funcs.contains_key("db.exec"));
        assert!(funcs.contains_key("db.query"));
        assert!(funcs.contains_key("db.close"));
        assert!(funcs.contains_key("std.sqlz.postgres.connect"));
        assert!(funcs.contains_key("std.sqlz.postgres.exec"));
        assert!(funcs.contains_key("std.sqlz.postgres.query"));
        assert!(funcs.contains_key("std.sqlz.postgres.close"));
        assert!(funcs.contains_key("std.sqlz.mysql.connect"));
        assert!(funcs.contains_key("std.sqlz.mysql.exec"));
        assert!(funcs.contains_key("std.sqlz.mysql.query"));
        assert!(funcs.contains_key("std.sqlz.mysql.close"));
        assert!(funcs.contains_key("std.sqlz.transaction"));
        assert!(funcs.contains_key("sqlz.transaction"));
        assert!(funcs.contains_key("std.db.transaction"));
        assert!(funcs.contains_key("db.transaction"));
        // Math constants are static values, not zero-arg functions.
        let consts = stdlib_consts();
        assert!(consts.contains_key("std.math.PI"));
        assert!(consts.contains_key("std.math.E"));
        assert!(consts.contains_key("std.math.TAU"));
        assert!(funcs.contains_key("std.colors.red"));
        assert!(funcs.contains_key("std.colors.rgb"));
        assert!(funcs.contains_key("std.colors.hex"));
        assert!(funcs.contains_key("colors.red"));
        assert!(funcs.contains_key("colors.bold"));
        assert!(funcs.contains_key("colors.strip"));
        assert_eq!(funcs.len(), 586);
    }

    #[test]
    fn vec_funcs_are_generic() {
        let funcs = stdlib_funcs();
        assert_eq!(funcs["std.vec.push"].generics, vec!["T"]);
        assert_eq!(funcs["std.vec.push"].params[1].1, Type::Named("T".into()));
    }
}
