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
        readonly: false,
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
        readonly: false,
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
        readonly: false,
        extern_c_symbol: None,
    }
}

/// Build a signature where the trailing `n_defaults` parameters may be
/// omitted at call sites (checker fills nothing — natives that declare
/// defaults must tolerate the shorter arity at runtime; see the
/// arity-flexible natives in `natives/mod.rs`).
fn sig_defaults(params: Vec<(&str, Type)>, n_defaults: usize, ret: Type) -> FuncSig {
    let total = params.len();
    let mut has_default = vec![false; total];
    for slot in has_default
        .iter_mut()
        .skip(total.saturating_sub(n_defaults))
    {
        *slot = true;
    }
    FuncSig {
        generics: Vec::new(),
        bounds: Vec::new(),
        params: params
            .into_iter()
            .map(|(n, t)| (n.to_string(), t))
            .collect(),
        has_default,
        ret,
        is_extern: false,
        readonly: false,
        extern_c_symbol: None,
    }
}

/// All standard library function signatures, keyed by qualified name
/// (e.g. `std.str.length`). Console I/O lives here as bare builtins
/// (`print`, `println`, `input`).
///
/// The table is built once per process and cloned on each call: building
/// from scratch costs ~475 inserts with fresh `Type` trees, while cloning
/// the cached table is a single pass with no type construction. Hot paths
/// (`import` namespace registration runs once per import) should prefer
/// [`stdlib_funcs_cached`] to avoid even the clone.
pub fn stdlib_funcs() -> HashMap<String, FuncSig> {
    stdlib_funcs_cached().clone()
}

/// Borrow the process-wide cached signature table. Zero build cost after
/// the first call (a single atomic load + `HashMap` traversal).
pub fn stdlib_funcs_cached() -> &'static HashMap<String, FuncSig> {
    static CACHED: std::sync::OnceLock<HashMap<String, FuncSig>> = std::sync::OnceLock::new();
    CACHED.get_or_init(build_stdlib_funcs)
}

/// Uncached constructor (runs once via [`stdlib_funcs_cached`]).
fn build_stdlib_funcs() -> HashMap<String, FuncSig> {
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
    m.insert("range".into(), {
        let mut s = sig(
            vec![
                ("start", Type::Int),
                ("stop", Type::Int),
                ("step", Type::Int),
            ],
            range_t.clone(),
        );
        s.readonly = true;
        s
    });
    m.insert("len".into(), {
        let mut s = sig_t(vec![("v", t.clone())], Type::Int);
        s.readonly = true;
        s
    });
    // Unicode scalar conversions (#276, #263): `ord` reads the codepoint of
    // a single-character string, `chr` builds one (both reject bad input at
    // runtime). Bare like `len`; `str.*` forms below enable method syntax.
    m.insert("ord".into(), {
        let mut s = sig(vec![("ch", Type::Str)], Type::Int);
        s.readonly = true;
        s
    });
    m.insert("chr".into(), {
        let mut s = sig(vec![("cp", Type::Int)], Type::Str);
        s.readonly = true;
        s
    });
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
    m.insert("std.str.length".into(), {
        let mut s = sig(vec![("s", Type::Str)], Type::Int);
        s.readonly = true;
        s
    });
    m.insert(
        "std.str.split".into(),
        sig(
            vec![("s", Type::Str), ("sep", Type::Str)],
            Type::Array(Box::new(Type::Str)),
        ),
    );
    m.insert("std.str.contains".into(), {
        let mut s = sig(vec![("s", Type::Str), ("sub", Type::Str)], Type::Bool);
        s.readonly = true;
        s
    });
    m.insert("std.str.find".into(), {
        let mut s = sig(
            vec![("s", Type::Str), ("sub", Type::Str), ("from", Type::Int)],
            Type::Int,
        );
        s.readonly = true;
        s
    });
    m.insert("std.str.rfind".into(), {
        let mut s = sig(
            vec![("s", Type::Str), ("sub", Type::Str), ("from", Type::Int)],
            Type::Int,
        );
        s.readonly = true;
        s
    });
    m.insert(
        "std.str.bytes".into(),
        sig(vec![("s", Type::Str)], Type::Array(Box::new(Type::Int))),
    );
    m.insert("std.str.byte_len".into(), {
        let mut s = sig(vec![("s", Type::Str)], Type::Int);
        s.readonly = true;
        s
    });
    m.insert("std.str.ord".into(), {
        let mut s = sig(vec![("ch", Type::Str)], Type::Int);
        s.readonly = true;
        s
    });
    m.insert(
        "std.str.chr".into(),
        sig(vec![("cp", Type::Int)], Type::Str),
    );
    m.insert(
        "std.str.classify".into(),
        sig(
            vec![
                ("text", Type::Str),
                ("markers", Type::Array(Box::new(Type::Str))),
                ("bstart", Type::Str),
                ("bend", Type::Str),
                ("nested", Type::Bool),
                ("whole", Type::Bool),
            ],
            Type::Array(Box::new(Type::Int)),
        ),
    );
    m.insert(
        "std.str.starts_with_at".into(),
        sig(
            vec![("s", Type::Str), ("sub", Type::Str), ("pos", Type::Int)],
            Type::Bool,
        ),
    );
    m.insert(
        "std.str.ends_with_at".into(),
        sig(
            vec![("s", Type::Str), ("sub", Type::Str), ("pos", Type::Int)],
            Type::Bool,
        ),
    );
    m.insert(
        "std.str.trim_span".into(),
        sig(
            vec![("s", Type::Str), ("start", Type::Int), ("end", Type::Int)],
            Type::Array(Box::new(Type::Int)),
        ),
    );
    m.insert(
        "std.str.find_in".into(),
        sig(
            vec![
                ("s", Type::Str),
                ("sub", Type::Str),
                ("start", Type::Int),
                ("end", Type::Int),
            ],
            Type::Int,
        ),
    );
    m.insert(
        "std.str.rfind_in".into(),
        sig(
            vec![
                ("s", Type::Str),
                ("sub", Type::Str),
                ("start", Type::Int),
                ("end", Type::Int),
            ],
            Type::Int,
        ),
    );
    m.insert(
        "std.str.count_in".into(),
        sig(
            vec![
                ("s", Type::Str),
                ("sub", Type::Str),
                ("start", Type::Int),
                ("end", Type::Int),
            ],
            Type::Int,
        ),
    );
    // Canonical `std.str.*` entries: the single seeds behind the qualified
    // calls (`std.str.trim(s)`) and — via fallback — the bare
    // (`str.trim(s)`) and method (`s.trim()`) spellings. Selective imports
    // (`import std.str(trim)`) resolve against these. See
    // `canonical_std_name`.
    m.insert(
        "std.str.trim".into(),
        sig(vec![("s", Type::Str)], Type::Str),
    );
    m.insert(
        "std.str.to_upper".into(),
        sig(vec![("s", Type::Str)], Type::Str),
    );
    m.insert(
        "std.str.to_lower".into(),
        sig(vec![("s", Type::Str)], Type::Str),
    );
    m.insert(
        "std.str.replace".into(),
        sig(
            vec![("s", Type::Str), ("old", Type::Str), ("new", Type::Str)],
            Type::Str,
        ),
    );
    m.insert(
        "std.str.starts_with".into(),
        sig(vec![("s", Type::Str), ("prefix", Type::Str)], Type::Bool),
    );
    m.insert(
        "std.str.ends_with".into(),
        sig(vec![("s", Type::Str), ("suffix", Type::Str)], Type::Bool),
    );
    m.insert(
        "std.str.join".into(),
        sig(
            vec![
                ("items", Type::Array(Box::new(Type::Str))),
                ("sep", Type::Str),
            ],
            Type::Str,
        ),
    );
    m.insert(
        "std.str.trim_start".into(),
        sig(vec![("s", Type::Str)], Type::Str),
    );
    m.insert(
        "std.str.trim_end".into(),
        sig(vec![("s", Type::Str)], Type::Str),
    );

    // str.* methods (for method dispatch: "hello".trim())

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
    // NOTE: bare `span.end` keeps its seed: `span` is not an importable
    // module (like `tls.*`), so the canonical fallback can never produce
    // it — method calls on span handles (`sp.end()`) need the bare key.
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

    // std.term — terminal control (raw mode, single-key reads, size).
    // Fallible ops return Result so non-TTY stdin degrades gracefully;
    // `is_tty` is the total predicate. Both spellings.
    let res_unit2 = || Type::Result(Box::new(Type::Unit), Box::new(Type::Str));
    let res_int = || Type::Result(Box::new(Type::Int), Box::new(Type::Str));
    let res_bool = || Type::Result(Box::new(Type::Bool), Box::new(Type::Str));
    let res_size = || {
        Type::Result(
            Box::new(Type::Array(Box::new(Type::Int))),
            Box::new(Type::Str),
        )
    };
    m.insert("std.term.enable_raw".into(), sig(vec![], res_unit2()));
    m.insert("std.term.disable_raw".into(), sig(vec![], res_unit2()));
    m.insert("std.term.read_key".into(), sig(vec![], res_int()));
    m.insert(
        "std.term.poll".into(),
        sig(vec![("ms", Type::Int)], res_bool()),
    );
    m.insert("std.term.get_size".into(), sig(vec![], res_size()));
    m.insert("std.term.is_tty".into(), sig(vec![], Type::Bool));
    m.insert("std.term.flush".into(), sig(vec![], Type::Unit));

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
    // Pure-ZZ wrapper (zz/args/mod.zz): ArgsParser constructor namespace.
    m.insert("ArgsParser.new".into(), sig(vec![], args_t.clone()));
    m.insert(
        "std.str.pad_right".into(),
        sig(
            vec![("s", Type::Str), ("len", Type::Int), ("pad", Type::Str)],
            Type::Str,
        ),
    );
    // Method-dispatch aliases for pure-ZZ string helpers

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
    // bytes.* methods (for method dispatch: `b.len()` on byte buffers).
    m.insert(
        "std.bytes.len".into(),
        sig(vec![("v", Type::Bytes)], Type::Int),
    );
    m.insert(
        "std.bytes.to_str".into(),
        sig(
            vec![("vs", Type::Array(Box::new(Type::Int)))],
            Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.bytes.to_ints".into(),
        sig(vec![("b", Type::Bytes)], Type::Array(Box::new(Type::Int))),
    );
    // NOTE: `vec.append` was removed — canonical spelling is `vec.push`
    // plus the method form `xs.push(x)`. No bare `push`/`append` builtin.
    m.insert(
        "std.vec.reverse".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone())))],
            Type::Array(Box::new(t.clone())),
        ),
    );
    m.insert(
        "std.vec.join".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone()))), ("sep", Type::Str)],
            Type::Str,
        ),
    );
    m.insert(
        "std.vec.contains".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone()))), ("x", t.clone())],
            Type::Bool,
        ),
    );
    m.insert(
        "std.vec.sort".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone())))],
            Type::Array(Box::new(t.clone())),
        ),
    );
    // sort_by / sort_key / sort_by_desc — comparator / key-based ordering.
    // `cmp: |T, T| -> int` must return -1/0/1 (or any negative/zero/positive).
    // `key: |T| -> K` where K: int|str|float (natural-order comparable).
    let cmp_t = Type::Func(vec![t.clone(), t.clone()], Box::new(Type::Int));
    m.insert(
        "std.vec.sort_by".into(),
        sig_t(
            vec![
                ("v", Type::Array(Box::new(t.clone()))),
                ("cmp", cmp_t.clone()),
            ],
            Type::Array(Box::new(t.clone())),
        ),
    );
    m.insert(
        "std.vec.sort_by_desc".into(),
        sig_t(
            vec![
                ("v", Type::Array(Box::new(t.clone()))),
                ("cmp", cmp_t.clone()),
            ],
            Type::Array(Box::new(t.clone())),
        ),
    );
    let key_t = Type::Func(vec![t.clone()], Box::new(Type::Named("U".to_string())));
    m.insert(
        "std.vec.sort_key".into(),
        sig_tu(
            vec![("v", Type::Array(Box::new(t.clone()))), ("key", key_t)],
            Type::Array(Box::new(t.clone())),
        ),
    );
    m.insert(
        "std.vec.insert".into(),
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
        "std.vec.remove".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone()))), ("idx", Type::Int)],
            Type::Array(Box::new(t.clone())),
        ),
    );
    // vec.enumerate — method form of bare `enumerate`, so both
    // `xs.enumerate()` and `enumerate(xs)` type-check to `[(int, T)]`.
    // Pairs unpack directly in 2-variable `for` loops.
    // (Single canonical seed; the bare spelling resolves via fallback.)
    m.insert(
        "std.vec.enumerate".into(),
        sig_t(
            vec![("v", Type::Array(Box::new(t.clone())))],
            Type::Array(Box::new(Type::Tuple(vec![Type::Int, t.clone()]))),
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
            readonly: false,
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
            readonly: false,
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
            readonly: false,
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

    // std.crypto — digests (hex strings), HMAC, CSPRNG, constant-time eq.
    // Both spellings registered (like json).
    m.insert(
        "std.crypto.sha256".into(),
        sig(vec![("s", Type::Str)], Type::Str),
    );
    m.insert(
        "std.crypto.sha256_bytes".into(),
        sig(vec![("b", Type::Bytes)], Type::Str),
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

    // std.encoding
    let result_str = || Type::Result(Box::new(Type::Str), Box::new(Type::Str));
    let result_bytes = || Type::Result(Box::new(Type::Bytes), Box::new(Type::Str));
    m.insert(
        "std.encoding.base64_encode".into(),
        sig(vec![("data", Type::Str)], Type::Str),
    );
    m.insert(
        "std.encoding.base64_decode".into(),
        sig(vec![("encoded", Type::Str)], result_str()),
    );
    m.insert(
        "std.encoding.base64_decode_bytes".into(),
        sig(vec![("encoded", Type::Str)], result_bytes()),
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

    // std.http — Client (`headers` is optional, defaults to `{}`).
    let result_response = || Type::Result(Box::new(Type::Response), Box::new(Type::Str));
    let dict_str = || Type::Dict(Box::new(Type::Str), Box::new(Type::Str));
    // POST/PUT bodies accept text or raw bytes (binary asset uploads).
    let body_t = || Type::Union(vec![Type::Str, Type::Bytes]);
    m.insert(
        "std.http.get".into(),
        sig_defaults(
            vec![("url", Type::Str), ("headers", dict_str())],
            1,
            result_response(),
        ),
    );
    m.insert(
        "std.http.post".into(),
        sig_defaults(
            vec![
                ("url", Type::Str),
                ("body", body_t()),
                ("headers", dict_str()),
            ],
            1,
            result_response(),
        ),
    );
    m.insert(
        "std.http.put".into(),
        sig_defaults(
            vec![
                ("url", Type::Str),
                ("body", body_t()),
                ("headers", dict_str()),
            ],
            1,
            result_response(),
        ),
    );
    m.insert(
        "std.http.delete".into(),
        sig_defaults(
            vec![("url", Type::Str), ("headers", dict_str())],
            1,
            result_response(),
        ),
    );
    // Unified client: `fetch(url, method?, headers?, body?, timeout_ms?)`.
    // Options bag in fixed order (most-omitted last); see
    // `fill_default_args` for the runtime defaults.
    m.insert(
        "std.http.fetch".into(),
        sig_defaults(
            vec![
                ("url", Type::Str),
                ("method", Type::Str),
                ("headers", dict_str()),
                ("body", body_t()),
                ("timeout_ms", Type::Int),
            ],
            4,
            result_response(),
        ),
    );
    // Test-only insecure fetch (self-signed fixtures). Same shape as fetch.
    m.insert(
        "std.http.fetch_insecure".into(),
        sig_defaults(
            vec![
                ("url", Type::Str),
                ("method", Type::Str),
                ("headers", dict_str()),
                ("body", body_t()),
                ("timeout_ms", Type::Int),
            ],
            4,
            result_response(),
        ),
    );

    // Generic over the body like `json.stringify` so dicts/arrays of any
    // element type check.
    // JSON client: `post_json(url, body: T, headers?)` — serializes any
    // value via the JSON serializer + sets `Content-Type: application/json`.
    m.insert(
        "std.http.post_json".into(),
        FuncSig {
            generics: vec!["T".to_string()],
            bounds: Vec::new(),
            params: vec![
                ("url".to_string(), Type::Str),
                ("body".to_string(), Type::Named("T".to_string())),
                ("headers".to_string(), dict_str()),
            ],
            has_default: vec![false, false, true],
            ret: result_response(),
            is_extern: false,
            readonly: false,
            extern_c_symbol: None,
        },
    );

    // std.http — Response methods (dispatched via method_namespace "http")
    m.insert(
        "std.http.status".into(),
        sig(vec![("res", Type::Response)], Type::Int),
    );
    m.insert(
        "std.http.text".into(),
        sig(vec![("res", Type::Response)], Type::Str),
    );
    m.insert(
        "std.http.json".into(),
        sig(
            vec![("res", Type::Response)],
            Type::Result(Box::new(Type::Json), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.http.headers".into(),
        sig(
            vec![("res", Type::Response)],
            Type::Dict(Box::new(Type::Str), Box::new(Type::Str)),
        ),
    );

    // std.http — Server methods (dispatched via method_namespace "http")

    // std.http — Server (per-route model)
    let server_t = Type::HttpServer;
    // Handler receives a typed request and returns a string, dict, array, or
    // response. The runtime also accepts Dict/Array (auto-JSON); the checker
    // models Str and Response (status/headers control).
    // We use a loose Func type: HttpRequest → Str|Response.
    let handler_t = Type::Func(
        vec![Type::HttpRequest],
        Box::new(Type::Union(vec![Type::Str, Type::Response])),
    );
    // Hijack handlers take (request, tcp.stream); the return is ignored
    // (the socket outlives the call via Arc), so accept anything.
    let hijack_t = Type::Func(
        vec![Type::HttpRequest, Type::TcpStream],
        Box::new(Type::Union(vec![Type::Str, Type::Unit])),
    );
    m.insert(
        "std.http.hijack".into(),
        sig(
            vec![
                ("server", server_t.clone()),
                ("path", Type::Str),
                ("handler", hijack_t.clone()),
            ],
            server_t.clone(),
        ),
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
        sig(
            vec![("server", server_t.clone()), ("port", Type::Int)],
            Type::Unit,
        ),
    );
    // `listen_cfg(server, port, opts)` — socket limits + shutdown budget.
    // All keys optional ints: read_ms, max_reqs_conn, max_body_bytes,
    // shutdown_ms. Both spellings like every other `std.http.*` name.
    let dict_str_int = Type::Dict(Box::new(Type::Str), Box::new(Type::Int));
    m.insert(
        "std.http.listen_cfg".into(),
        sig(
            vec![
                ("server", server_t.clone()),
                ("port", Type::Int),
                ("opts", dict_str_int.clone()),
            ],
            Type::Unit,
        ),
    );

    m.insert(
        "std.http.listen_tls".into(),
        sig(
            vec![
                ("server", server_t.clone()),
                ("port", Type::Int),
                ("cert_path", Type::Str),
                ("key_path", Type::Str),
            ],
            Type::Unit,
        ),
    );

    m.insert(
        "std.http.listen_tls_cfg".into(),
        sig(
            vec![
                ("server", server_t.clone()),
                ("port", Type::Int),
                ("cert_path", Type::Str),
                ("key_path", Type::Str),
                ("opts", dict_str_int.clone()),
            ],
            Type::Unit,
        ),
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
                        vec![Type::HttpRequest],
                        Box::new(Type::Result(
                            Box::new(Type::HttpRequest),
                            // Short-circuit: `.err(response)` answers directly.
                            Box::new(Type::Response),
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
        "std.http.serve_dir_at".into(),
        sig(
            vec![
                ("server", Type::HttpServer),
                ("prefix", Type::Str),
                ("dir", Type::Str),
            ],
            Type::HttpServer,
        ),
    );

    // Post-middleware: `fn(req, res) -> res`, applied to handler output.
    let post_t = Type::Func(
        vec![Type::HttpRequest, Type::Response],
        Box::new(Type::Response),
    );
    m.insert(
        "std.http.pipe_post".into(),
        sig(
            vec![("server", Type::HttpServer), ("post", post_t.clone())],
            Type::HttpServer,
        ),
    );

    // `with_headers(res, extra)` merges headers (`extra` wins).
    m.insert(
        "std.http.with_headers".into(),
        sig(
            vec![("res", Type::Response), ("extra", dict_str())],
            Type::Response,
        ),
    );

    // Token bucket per client IP: `max_requests` per rolling `window_ms`.
    m.insert(
        "std.http.rate_limit".into(),
        sig(
            vec![
                ("server", Type::HttpServer),
                ("max_requests", Type::Int),
                ("window_ms", Type::Int),
            ],
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
        "std.http.test_req".into(),
        sig(
            vec![
                ("server", Type::HttpServer),
                ("method", Type::Str),
                ("path", Type::Str),
                ("headers", dict_str()),
                ("body", Type::Str),
            ],
            Type::Response,
        ),
    );
    m.insert(
        "std.http.respond".into(),
        sig_defaults(
            vec![
                ("status", Type::Int),
                ("body", Type::Str),
                ("headers", dict_str()),
            ],
            1,
            Type::Response,
        ),
    );
    m.insert(
        "std.http.param".into(),
        sig(
            vec![("req", Type::HttpRequest), ("name", Type::Str)],
            Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.http.query".into(),
        sig(vec![("req", Type::HttpRequest)], dict_str_str.clone()),
    );
    m.insert(
        "std.http.header".into(),
        sig(
            vec![("req", Type::HttpRequest), ("name", Type::Str)],
            Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.http.body_json".into(),
        sig(
            vec![("req", Type::HttpRequest)],
            Type::Result(Box::new(Type::Json), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.http.body_form".into(),
        sig(vec![("req", Type::HttpRequest)], dict_str_str.clone()),
    );
    m.insert(
        "std.http.body_bytes".into(),
        sig(vec![("req", Type::HttpRequest)], Type::Bytes),
    );

    m.insert(
        "std.http.route".into(),
        sig(
            vec![
                ("server", server_t.clone()),
                ("method", Type::Str),
                ("path", Type::Str),
                ("handler", handler_t.clone()),
            ],
            server_t.clone(),
        ),
    );

    // Response constructors: `ok(body)` → 200, `created(body)` → 201,
    // `not_found()` → 404, `redirect(url)` → 302 + `Location` header.
    for (name, params, ret) in [
        ("ok", vec![("body", Type::Str)], Type::Response),
        ("created", vec![("body", Type::Str)], Type::Response),
        ("not_found", vec![], Type::Response),
        ("redirect", vec![("url", Type::Str)], Type::Response),
    ] {
        let s = sig(params, ret);
        m.insert(format!("std.http.{name}"), s.clone());
    }
    // Pure-ZZ middleware kits (`zz/http/mod.zz`): `cors(server, origins)`
    // gates origins + answers preflights + echoes `Allow-Origin`;
    // `secure_headers(server)` injects CSP/nosniff/referrer/frame headers;
    // `secure_header_dict()` exposes the raw map for manual merges.
    let str_arr = || Type::Array(Box::new(Type::Str));
    m.insert(
        "std.http.cors".into(),
        sig(
            vec![("server", Type::HttpServer), ("origins", str_arr())],
            Type::HttpServer,
        ),
    );

    m.insert(
        "std.http.secure_headers".into(),
        sig(vec![("server", Type::HttpServer)], Type::HttpServer),
    );

    m.insert(
        "std.http.secure_header_dict".into(),
        sig(vec![], dict_str()),
    );

    // Pure-ZZ kits: CSRF token helpers (CSPRNG hex + constant-time compare)
    // and per-response `X-Request-Id` (uuid v7) post-middleware.
    for (name, params, ret) in [
        ("csrf_token", vec![], Type::Str),
        (
            "csrf_check",
            vec![("a", Type::Str), ("b", Type::Str)],
            Type::Bool,
        ),
    ] {
        let s = sig(params, ret);
        m.insert(format!("std.http.{name}"), s.clone());
    }
    m.insert(
        "std.http.request_id".into(),
        sig(vec![("server", Type::HttpServer)], Type::HttpServer),
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
    // NOTE: `std.net.tcp_close` (legacy no-op) was removed — `close` /
    // `shutdown` perform a real shutdown.
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
    // std.net — binary-safe transfers + real shutdown.
    let result_bytes = || Type::Result(Box::new(Type::Bytes), Box::new(Type::Str));
    let result_unit = || Type::Result(Box::new(Type::Unit), Box::new(Type::Str));
    m.insert(
        "std.net.tcp_read_bytes".into(),
        sig(
            vec![("stream", Type::TcpStream), ("max_bytes", Type::Int)],
            result_bytes(),
        ),
    );
    m.insert(
        "std.net.tcp_write_bytes".into(),
        sig(
            vec![("stream", Type::TcpStream), ("data", Type::Bytes)],
            result_int(),
        ),
    );
    // `std.net.shutdown` is the ergonomic name for `std.net.tcp_shutdown`
    // (single canonical seeds; the bare `net.*` spellings resolve via
    // fallback). `stream.close()` / `stream.shutdown()` type-check.
    m.insert(
        "std.net.tcp_shutdown".into(),
        sig(vec![("stream", Type::TcpStream)], result_unit()),
    );
    m.insert(
        "std.net.shutdown".into(),
        sig(vec![("stream", Type::TcpStream)], result_unit()),
    );
    // std.net — ergonomic method aliases (same arities/shapes as the
    // `tcp_*` canonicals, so `listener.accept()?`, `stream.read_line()?`,
    // `stream.close()?` type-check; both spellings registered like
    // `http.log` / `std.http.log`).
    for (short, params, ret) in [
        (
            "accept",
            vec![("listener", Type::TcpListener)],
            result_tcp_stream(),
        ),
        (
            "read",
            vec![("stream", Type::TcpStream), ("max_bytes", Type::Int)],
            result_str(),
        ),
        ("read_line", vec![("stream", Type::TcpStream)], result_str()),
        (
            "read_bytes",
            vec![("stream", Type::TcpStream), ("max_bytes", Type::Int)],
            result_bytes(),
        ),
        (
            "read_exact",
            vec![("stream", Type::TcpStream), ("n", Type::Int)],
            result_bytes(),
        ),
        (
            "write",
            vec![("stream", Type::TcpStream), ("data", Type::Str)],
            result_int(),
        ),
        (
            "write_bytes",
            vec![("stream", Type::TcpStream), ("data", Type::Bytes)],
            result_int(),
        ),
        ("peer_addr", vec![("stream", Type::TcpStream)], result_str()),
        (
            "local_addr",
            vec![("stream", Type::TcpStream)],
            result_str(),
        ),
        (
            "set_read_timeout",
            vec![("stream", Type::TcpStream), ("ms", Type::Int)],
            result_bool(),
        ),
        (
            "set_write_timeout",
            vec![("stream", Type::TcpStream), ("ms", Type::Int)],
            result_bool(),
        ),
        // `stream.close()` is documented as the ergonomic spelling for a
        // real both-directions shutdown; without this alias only
        // `stream.shutdown()` resolved and the documented example failed
        // to type-check.
        ("close", vec![("stream", Type::TcpStream)], result_unit()),
    ] {
        let s = sig(params, ret);
        m.insert(format!("std.net.{short}"), s.clone());
    }

    // std.cell — shared-mutable slots (pool-backed handles; each op
    // takes the pool lock once). Generic over the stored value type.
    let cell_t = Type::Opaque("cell".to_string());
    {
        let t = Type::Named("T".to_string());
        let cell_result = |inner: Type| Type::Result(Box::new(inner), Box::new(Type::Str));
        m.insert(
            "std.cell.new".into(),
            sig_t(vec![("v", t.clone())], cell_t.clone()),
        );
        m.insert(
            "std.cell.get".into(),
            sig_t(vec![("c", cell_t.clone())], cell_result(t.clone())),
        );
        m.insert(
            "std.cell.set".into(),
            sig_t(
                vec![("c", cell_t.clone()), ("v", t.clone())],
                cell_result(Type::Bool),
            ),
        );
        m.insert(
            "std.cell.drop".into(),
            sig(vec![("c", cell_t.clone())], Type::Bool),
        );
    }
    let unix_t = Type::Opaque("unix".to_string());
    let unix_listener_t = Type::Opaque("unix_listener".to_string());
    let result_unix = || Type::Result(Box::new(unix_t.clone()), Box::new(Type::Str));
    let result_unix_listener =
        || Type::Result(Box::new(unix_listener_t.clone()), Box::new(Type::Str));
    m.insert(
        "std.net.unix_connect".into(),
        sig(
            vec![("path", Type::Str), ("timeout_ms", Type::Int)],
            result_unix(),
        ),
    );
    m.insert(
        "std.net.unix_listen".into(),
        sig(vec![("path", Type::Str)], result_unix_listener()),
    );
    m.insert(
        "std.net.unix_accept".into(),
        sig(vec![("listener", unix_listener_t.clone())], result_unix()),
    );
    m.insert(
        "std.net.unix_listener_close".into(),
        sig(vec![("listener", unix_listener_t.clone())], result_bool()),
    );
    for (method, canon, params, ret) in [
        (
            "write",
            "unix_write",
            vec![("stream", unix_t.clone()), ("data", Type::Str)],
            result_int(),
        ),
        (
            "write_bytes",
            "unix_write_bytes",
            vec![("stream", unix_t.clone()), ("data", Type::Bytes)],
            result_int(),
        ),
        (
            "read",
            "unix_read",
            vec![("stream", unix_t.clone()), ("max_bytes", Type::Int)],
            result_str(),
        ),
        (
            "read_bytes",
            "unix_read_bytes",
            vec![("stream", unix_t.clone()), ("max_bytes", Type::Int)],
            result_bytes(),
        ),
        (
            "read_exact",
            "unix_read_exact",
            vec![("stream", unix_t.clone()), ("n", Type::Int)],
            result_bytes(),
        ),
        (
            "read_line",
            "unix_readline",
            vec![("stream", unix_t.clone())],
            result_str(),
        ),
        (
            "set_read_timeout",
            "unix_set_read_timeout",
            vec![("stream", unix_t.clone()), ("ms", Type::Int)],
            result_bool(),
        ),
        (
            "set_write_timeout",
            "unix_set_write_timeout",
            vec![("stream", unix_t.clone()), ("ms", Type::Int)],
            result_bool(),
        ),
        (
            "close",
            "unix_close",
            vec![("stream", unix_t.clone())],
            result_unit(),
        ),
        (
            "shutdown",
            "unix_shutdown",
            vec![("stream", unix_t.clone())],
            result_unit(),
        ),
    ] {
        let s = sig(params, ret);
        m.insert(format!("std.net.{canon}"), s.clone());
        m.insert(format!("std.unix.{method}"), s.clone());
    }
    for (method, params, ret) in [
        (
            "accept",
            vec![("listener", unix_listener_t.clone())],
            result_unix(),
        ),
        (
            "close",
            vec![("listener", unix_listener_t.clone())],
            result_bool(),
        ),
        (
            "shutdown",
            vec![("listener", unix_listener_t.clone())],
            result_bool(),
        ),
    ] {
        let s = sig(params, ret);
        m.insert(format!("std.unix_listener.{method}"), s);
    }
    m.insert(
        "std.net.tcp_read_exact".into(),
        sig(
            vec![("stream", Type::TcpStream), ("n", Type::Int)],
            result_bytes(),
        ),
    );
    // std.net — TLS client (Track T, T-A via rustls; see docs/adr/0001-tls.md).
    // Connections are `Opaque("tls")` so `c.write(..)` method syntax works
    // through tag dispatch; AOT lowers through the `zz_tls_*` staticlib FFI
    // (same driver as the VM, byte-identical by construction).
    let tls_t = Type::Opaque("tls".to_string());
    let result_tls = || Type::Result(Box::new(tls_t.clone()), Box::new(Type::Str));
    let tls_listener_t = Type::Opaque("tls_listener".to_string());
    let result_tls_listener =
        || Type::Result(Box::new(tls_listener_t.clone()), Box::new(Type::Str));
    m.insert(
        "std.net.tls_listen".into(),
        sig(
            vec![
                ("addr", Type::Str),
                ("cert_file", Type::Str),
                ("key_file", Type::Str),
            ],
            result_tls_listener(),
        ),
    );
    m.insert(
        "std.net.tls_accept".into(),
        sig(vec![("listener", tls_listener_t.clone())], result_tls()),
    );
    m.insert(
        "std.net.tls_listener_close".into(),
        sig(vec![("listener", tls_listener_t.clone())], result_bool()),
    );
    for (method, params, ret) in [
        (
            "accept",
            vec![("listener", tls_listener_t.clone())],
            result_tls(),
        ),
        (
            "close",
            vec![("listener", tls_listener_t.clone())],
            result_bool(),
        ),
        (
            "shutdown",
            vec![("listener", tls_listener_t.clone())],
            result_bool(),
        ),
    ] {
        let s = sig(params, ret);
        m.insert(format!("std.tls_listener.{method}"), s);
    }
    for (name, params) in [
        (
            "std.net.tls_connect",
            vec![
                ("addr", Type::Str),
                ("server_name", Type::Str),
                ("timeout_ms", Type::Int),
            ],
        ),
        (
            "std.net.tls_connect_insecure",
            vec![
                ("addr", Type::Str),
                ("server_name", Type::Str),
                ("timeout_ms", Type::Int),
            ],
        ),
    ] {
        m.insert(name.into(), sig(params, result_tls()));
    }
    m.insert(
        "std.net.tls_connect_with_ca".into(),
        sig(
            vec![
                ("addr", Type::Str),
                ("server_name", Type::Str),
                ("ca_file", Type::Str),
                ("timeout_ms", Type::Int),
            ],
            result_tls(),
        ),
    );
    // `tls.*` method namespace (mirrors the `tcp.*` set: canonical
    // `tls_readline` (no underscore, like `tcp_readline`); `close` is the
    // real shutdown like `net.close`).
    for (method, canon, params, ret) in [
        (
            "write",
            "tls_write",
            vec![("stream", tls_t.clone()), ("data", Type::Str)],
            result_int(),
        ),
        (
            "write_bytes",
            "tls_write_bytes",
            vec![("stream", tls_t.clone()), ("data", Type::Bytes)],
            result_int(),
        ),
        (
            "read",
            "tls_read",
            vec![("stream", tls_t.clone()), ("max_bytes", Type::Int)],
            result_str(),
        ),
        (
            "read_bytes",
            "tls_read_bytes",
            vec![("stream", tls_t.clone()), ("max_bytes", Type::Int)],
            result_bytes(),
        ),
        (
            "read_exact",
            "tls_read_exact",
            vec![("stream", tls_t.clone()), ("n", Type::Int)],
            result_bytes(),
        ),
        (
            "read_line",
            "tls_readline",
            vec![("stream", tls_t.clone())],
            result_str(),
        ),
        (
            "set_read_timeout",
            "tls_set_read_timeout",
            vec![("stream", tls_t.clone()), ("ms", Type::Int)],
            result_bool(),
        ),
        (
            "set_write_timeout",
            "tls_set_write_timeout",
            vec![("stream", tls_t.clone()), ("ms", Type::Int)],
            result_bool(),
        ),
        (
            "peer_addr",
            "tls_peer_addr",
            vec![("stream", tls_t.clone())],
            result_str(),
        ),
        (
            "close",
            "tls_close",
            vec![("stream", tls_t.clone())],
            result_unit(),
        ),
        (
            "shutdown",
            "tls_shutdown",
            vec![("stream", tls_t.clone())],
            result_unit(),
        ),
    ] {
        let s = sig(params, ret);
        m.insert(format!("std.net.{canon}"), s.clone());
        m.insert(format!("std.tls.{method}"), s.clone());
    }
    // NOTE: no bare `net.{canon}` line — those sigs never had natives
    // behind them (checked, then failed at runtime). Bare `tls.*`
    // spellings resolve via the canonical fallback (`tls` is an
    // importable module now).
    // `tls.connect*` constructor aliases so `tls.connect(..)` reads naturally.
    for (alias, params) in [
        (
            "connect",
            vec![
                ("addr", Type::Str),
                ("server_name", Type::Str),
                ("timeout_ms", Type::Int),
            ],
        ),
        (
            "connect_insecure",
            vec![
                ("addr", Type::Str),
                ("server_name", Type::Str),
                ("timeout_ms", Type::Int),
            ],
        ),
    ] {
        let s = sig(params, result_tls());
        m.insert(format!("std.tls.{alias}"), s);
    }
    m.insert(
        "std.tls.connect_with_ca".into(),
        sig(
            vec![
                ("addr", Type::Str),
                ("server_name", Type::Str),
                ("ca_file", Type::Str),
                ("timeout_ms", Type::Int),
            ],
            result_tls(),
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
    let result_bool = || Type::Result(Box::new(Type::Bool), Box::new(Type::Str));
    let result_int = || Type::Result(Box::new(Type::Int), Box::new(Type::Str));
    let result_file = || Type::Result(Box::new(file_t.clone()), Box::new(Type::Str));
    let result_str_arr = || {
        Type::Result(
            Box::new(Type::Array(Box::new(Type::Str))),
            Box::new(Type::Str),
        )
    };
    let result_bytes = || Type::Result(Box::new(Type::Bytes), Box::new(Type::Str));
    let result_int_arr = || {
        Type::Result(
            Box::new(Type::Array(Box::new(Type::Int))),
            Box::new(Type::Str),
        )
    };
    let result_stat = || {
        Type::Result(
            Box::new(Type::Dict(Box::new(Type::Str), Box::new(Type::Str))),
            Box::new(Type::Str),
        )
    };
    for (name, params, ret) in [
        ("std.fs.read", vec![("path", Type::Str)], result_str()),
        (
            "std.fs.read_bytes",
            vec![("path", Type::Str)],
            result_bytes(),
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
        // NOTE: `std.fs.rename` was removed (identical to `move`) — one
        // spelling: `std.fs.move`.
        ("std.fs.exists", vec![("path", Type::Str)], Type::Bool),
        ("std.fs.is_file", vec![("path", Type::Str)], Type::Bool),
        ("std.fs.is_dir", vec![("path", Type::Str)], Type::Bool),
        ("std.fs.remove", vec![("path", Type::Str)], result_unit()),
        ("std.fs.mkdir", vec![("path", Type::Str)], result_unit()),
        ("std.fs.mkdir_all", vec![("path", Type::Str)], result_unit()),
        (
            "std.fs.read_dir",
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
            "std.fs.scan_counts",
            vec![
                ("path", Type::Str),
                ("markers", Type::Array(Box::new(Type::Str))),
                ("bstart", Type::Str),
                ("bend", Type::Str),
                ("nested", Type::Bool),
                ("whole", Type::Bool),
            ],
            result_int_arr(),
        ),
        (
            "std.fs.is_generated",
            vec![
                ("path", Type::Str),
                ("markers", Type::Array(Box::new(Type::Str))),
            ],
            result_bool(),
        ),
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

    // std.path — pure-ZZ lexical path helpers (from zz/path/mod.zz).
    // Separator style follows `std.env.os()` (`"windows"` → `\`, else `/`).
    // `std.fs` keeps its native I/O operations; its `join` shares this
    // lexical algorithm for backward compatibility.
    for (name, params, ret) in [
        ("join", vec![("a", Type::Str), ("b", Type::Str)], Type::Str),
        (
            "join_all",
            vec![("parts", Type::Array(Box::new(Type::Str)))],
            Type::Str,
        ),
        ("normalize", vec![("path", Type::Str)], Type::Str),
        ("basename", vec![("path", Type::Str)], Type::Str),
        ("dirname", vec![("path", Type::Str)], Type::Str),
        ("is_absolute", vec![("path", Type::Str)], Type::Bool),
        ("extension", vec![("path", Type::Str)], Type::Str),
    ] {
        let s = sig(params, ret);
        m.insert(format!("std.path.{name}"), s.clone());
    }

    // std.env
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
    // std.env — cross-platform environment + OS identity.
    let opt_str = || Type::Option(Box::new(Type::Str));
    let res_str = || Type::Result(Box::new(Type::Str), Box::new(Type::Str));
    let res_unit = || Type::Result(Box::new(Type::Unit), Box::new(Type::Str));
    m.insert(
        "std.env.get".into(),
        sig(vec![("key", Type::Str)], opt_str()),
    );
    m.insert(
        "std.env.set".into(),
        sig(vec![("key", Type::Str), ("val", Type::Str)], res_unit()),
    );
    m.insert(
        "std.env.remove".into(),
        sig(vec![("key", Type::Str)], Type::Unit),
    );
    // NOTE: `std.env.unset` was removed — one spelling: `std.env.remove`.
    m.insert(
        "std.env.vars".into(),
        sig(vec![], Type::Dict(Box::new(Type::Str), Box::new(Type::Str))),
    );
    m.insert("std.env.cwd".into(), sig(vec![], res_str()));
    m.insert(
        "std.env.set_cwd".into(),
        sig(vec![("path", Type::Str)], res_unit()),
    );
    m.insert("std.env.exe_path".into(), sig(vec![], res_str()));
    m.insert("std.env.home_dir".into(), sig(vec![], opt_str()));
    m.insert("std.env.temp_dir".into(), sig(vec![], Type::Str));
    m.insert("std.env.user".into(), sig(vec![], opt_str()));
    m.insert("std.env.os".into(), sig(vec![], Type::Str));
    m.insert("std.env.arch".into(), sig(vec![], Type::Str));

    // Built-in: `typeof(v)` — accepts any value, returns its type name.
    let t = Type::Named("T".to_string());
    m.insert("typeof".into(), {
        let mut s = sig_t(vec![("v", t.clone())], Type::Str);
        s.readonly = true;
        s
    });

    // NOTE: the bare `append(arr, val)` builtin was removed. Push is
    // `vec.push(v, x)` plus the method form `xs.push(x)` (statement
    // write-back stores the result). This keeps a single push spelling.

    // Built-in: `clone(v)` — deep copy, fully independent value (#267).
    m.insert("clone".into(), sig_t(vec![("v", t.clone())], t.clone()));

    // Built-in: `weak(v)` — weak reference for cycle breaking.
    m.insert("weak".into(), sig_t(vec![("v", t.clone())], t.clone()));

    // Built-in: `upgrade(w)` — upgrade a weak reference to strong.
    m.insert("upgrade".into(), sig_t(vec![("v", t.clone())], t.clone()));

    // Built-in conversions.
    // `str(v)` — stringify any value (total).
    m.insert("str".into(), sig_t(vec![("v", t.clone())], Type::Str));
    // `dbg(v)` — debug print preserving `.some`/`.none`, returns `v`.
    m.insert("dbg".into(), sig_t(vec![("v", t.clone())], t.clone()));
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

    // ── std.math numeric constants ──
    // True `Float` values (see `stdlib_consts`), not zero-arg functions.
    // The checker resolves them without `()`; calling one is an error.
    // Fully-qualified `std.math.*` names need no import (like all `std.*`
    // functions); `import std.math` adds the short `math.*` spellings via
    // `register_module_namespace`.
    for name in [
        "PI", "E", "TAU", "SQRT_2", "SQRT_1_2", "LN_2", "LN_10", "LOG10_E", "LOG2_E", "INF", "NAN",
    ] {
        m.insert(format!("std.math.{name}"), sig(vec![], Type::Float));
    }

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
    // min/max are generic <T>; count counts occurrences.
    // sum/product/sum_f/product_f/min_val/max_val live in std.vec instead.
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
    // Float aggregation — Kahan compensated summation
    m.insert(
        "std.math.sum_f".into(),
        sig(vec![("arr", float_arr.clone())], Type::Float),
    );
    m.insert(
        "std.math.product_f".into(),
        sig(vec![("arr", float_arr.clone())], Type::Float),
    );
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
    m.insert(
        "std.math.mean_f".into(),
        sig(
            vec![("arr", float_arr.clone())],
            Type::Result(Box::new(Type::Float), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.math.median_f".into(),
        sig(
            vec![("arr", float_arr.clone())],
            Type::Result(Box::new(Type::Float), Box::new(Type::Str)),
        ),
    );
    // Method-dispatch aliases for pure-ZZ math helpers

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
    // Pure-ZZ calendar dates (zz/time date section): dict-based Date
    // `{year,month,day,hour,min,sec}` so no struct export needed.
    {
        let dict_si = Type::Dict(Box::new(Type::Str), Box::new(Type::Int));
        for (name, params, ret) in [
            (
                "make_date",
                vec![
                    ("y", Type::Int),
                    ("mo", Type::Int),
                    ("d", Type::Int),
                    ("h", Type::Int),
                    ("mi", Type::Int),
                    ("se", Type::Int),
                ],
                dict_si.clone(),
            ),
            ("epoch_fallback", vec![], dict_si.clone()),
            ("is_leap", vec![("y", Type::Int)], Type::Bool),
            (
                "days_in_month",
                vec![("y", Type::Int), ("m", Type::Int)],
                Type::Int,
            ),
            (
                "date_valid",
                vec![("y", Type::Int), ("m", Type::Int), ("d", Type::Int)],
                Type::Bool,
            ),
            (
                "days_from_civil",
                vec![("y", Type::Int), ("m", Type::Int), ("d", Type::Int)],
                Type::Int,
            ),
            ("civil_from_days", vec![("z", Type::Int)], dict_si.clone()),
            ("parse_rfc3339", vec![("s", Type::Str)], dict_si.clone()),
            ("pad2", vec![("n", Type::Int)], Type::Str),
            ("format_rfc3339", vec![("d", dict_si.clone())], Type::Str),
            ("to_epoch_days", vec![("d", dict_si.clone())], Type::Int),
            ("from_epoch_days", vec![("z", Type::Int)], dict_si.clone()),
            (
                "add_days",
                vec![("d", dict_si.clone()), ("n", Type::Int)],
                dict_si.clone(),
            ),
            (
                "diff_days",
                vec![("a", dict_si.clone()), ("b", dict_si.clone())],
                Type::Int,
            ),
        ] {
            let s = sig(params, ret);
            m.insert(format!("std.time.{name}"), s.clone());
        }
    }

    // std.map — pure-ZZ dict helpers (zz/collections/map.zz).
    {
        let dict_si = Type::Dict(Box::new(Type::Str), Box::new(Type::Int));
        let dict_ss = Type::Dict(Box::new(Type::Str), Box::new(Type::Str));
        let arr_str = Type::Array(Box::new(Type::Str));
        let arr_int = Type::Array(Box::new(Type::Int));
        for (name, params, ret) in [
            (
                "has",
                vec![("d", dict_si.clone()), ("key", Type::Str)],
                Type::Bool,
            ),
            (
                "get_or",
                vec![
                    ("d", dict_si.clone()),
                    ("key", Type::Str),
                    ("default", Type::Int),
                ],
                Type::Int,
            ),
            (
                "get_str",
                vec![
                    ("d", dict_ss.clone()),
                    ("key", Type::Str),
                    ("default", Type::Str),
                ],
                Type::Str,
            ),
            ("keys", vec![("d", dict_si.clone())], arr_str.clone()),
            ("keys_str", vec![("d", dict_ss.clone())], arr_str.clone()),
            ("values", vec![("d", dict_si.clone())], arr_int.clone()),
            ("values_str", vec![("d", dict_ss.clone())], arr_str.clone()),
            ("len", vec![("d", dict_si.clone())], Type::Int),
            ("is_empty", vec![("d", dict_si.clone())], Type::Bool),
            (
                "merge",
                vec![("a", dict_si.clone()), ("b", dict_si.clone())],
                dict_si.clone(),
            ),
            (
                "merge_str",
                vec![("a", dict_ss.clone()), ("b", dict_ss.clone())],
                dict_ss.clone(),
            ),
            (
                "remove",
                vec![("d", dict_si.clone()), ("key", Type::Str)],
                dict_si.clone(),
            ),
        ] {
            let s = sig(params, ret);
            m.insert(format!("std.map.{name}"), s.clone());
        }
    }

    // std.set — pure-ZZ set helpers on arrays (zz/collections/set.zz).
    {
        let arr_str = Type::Array(Box::new(Type::Str));
        let arr_int = Type::Array(Box::new(Type::Int));
        for (name, params, ret) in [
            (
                "has",
                vec![("s", arr_str.clone()), ("x", Type::Str)],
                Type::Bool,
            ),
            (
                "has_int",
                vec![("s", arr_int.clone()), ("x", Type::Int)],
                Type::Bool,
            ),
            (
                "insert",
                vec![("s", arr_str.clone()), ("x", Type::Str)],
                arr_str.clone(),
            ),
            (
                "insert_int",
                vec![("s", arr_int.clone()), ("x", Type::Int)],
                arr_int.clone(),
            ),
            (
                "remove",
                vec![("s", arr_str.clone()), ("x", Type::Str)],
                arr_str.clone(),
            ),
            (
                "remove_int",
                vec![("s", arr_int.clone()), ("x", Type::Int)],
                arr_int.clone(),
            ),
            (
                "union",
                vec![("a", arr_str.clone()), ("b", arr_str.clone())],
                arr_str.clone(),
            ),
            (
                "union_int",
                vec![("a", arr_int.clone()), ("b", arr_int.clone())],
                arr_int.clone(),
            ),
            (
                "intersect",
                vec![("a", arr_str.clone()), ("b", arr_str.clone())],
                arr_str.clone(),
            ),
            (
                "intersect_int",
                vec![("a", arr_int.clone()), ("b", arr_int.clone())],
                arr_int.clone(),
            ),
            (
                "diff",
                vec![("a", arr_str.clone()), ("b", arr_str.clone())],
                arr_str.clone(),
            ),
            ("len", vec![("s", arr_str.clone())], Type::Int),
            ("is_empty", vec![("s", arr_str.clone())], Type::Bool),
        ] {
            let s = sig(params, ret);
            m.insert(format!("std.set.{name}"), s.clone());
        }
    }

    // std.dec — exact decimal math on strings (zz/dec/mod.zz).
    {
        for (name, params, ret) in [
            ("is_valid", vec![("s", Type::Str)], Type::Bool),
            ("scale_of", vec![("s", Type::Str)], Type::Int),
            ("pow10", vec![("n", Type::Int)], Type::Int),
            (
                "to_scaled",
                vec![("s", Type::Str), ("scale", Type::Int)],
                Type::Int,
            ),
            (
                "from_scaled",
                vec![("v", Type::Int), ("scale", Type::Int)],
                Type::Str,
            ),
            ("trim_zeros", vec![("s", Type::Str)], Type::Str),
            ("add", vec![("a", Type::Str), ("b", Type::Str)], Type::Str),
            ("sub", vec![("a", Type::Str), ("b", Type::Str)], Type::Str),
            ("mul", vec![("a", Type::Str), ("b", Type::Str)], Type::Str),
            ("cmp", vec![("a", Type::Str), ("b", Type::Str)], Type::Int),
            ("eq", vec![("a", Type::Str), ("b", Type::Str)], Type::Bool),
            ("lt", vec![("a", Type::Str), ("b", Type::Str)], Type::Bool),
            ("gt", vec![("a", Type::Str), ("b", Type::Str)], Type::Bool),
            (
                "format",
                vec![("s", Type::Str), ("places", Type::Int)],
                Type::Str,
            ),
        ] {
            let s = sig(params, ret);
            m.insert(format!("std.dec.{name}"), s.clone());
        }
    }

    // External packages (NOT stdlib): TOML lives at ~/Projects/toml
    // as a versioned package. No `std.toml` / `toml.*` signatures here.

    // std.bytes + str.builder — linear builders (zz/bytes/mod.zz).
    {
        let arr_str = Type::Array(Box::new(Type::Str));
        let arr_int = Type::Array(Box::new(Type::Int));
        for (name, params, ret) in [
            ("builder", vec![], arr_str.clone()),
            (
                "push_part",
                vec![("b", arr_str.clone()), ("s", Type::Str)],
                arr_str.clone(),
            ),
            ("finish", vec![("b", arr_str.clone())], Type::Str),
            ("builder_len", vec![("b", arr_str.clone())], Type::Int),
            (
                "join_parts",
                vec![("parts", arr_str.clone()), ("sep", Type::Str)],
                Type::Str,
            ),
        ] {
            let s = sig(params, ret);
            m.insert(format!("std.str.{name}"), s);
        }
        for (name, params, ret) in [
            ("builder", vec![], arr_int.clone()),
            (
                "push_byte",
                vec![("b", arr_int.clone()), ("v", Type::Int)],
                arr_int.clone(),
            ),
            (
                "extend",
                vec![("b", arr_int.clone()), ("vs", arr_int.clone())],
                arr_int.clone(),
            ),
            ("len_of", vec![("vs", arr_int.clone())], Type::Int),
            ("from_ints", vec![("vs", arr_int.clone())], arr_int.clone()),
        ] {
            let s = sig(params, ret);
            m.insert(format!("std.bytes.{name}"), s);
        }
        // bytes.finish: the missing [int] -> bytes bridge (mod.zz builds
        // int-lists; only natives mint `bytes`). Fallible (range-checked),
        // unlike str.finish — binary assembly must never silently clamp.
        m.insert(
            "std.bytes.finish".into(),
            sig(
                vec![("vs", arr_int.clone())],
                Type::Result(Box::new(Type::Bytes), Box::new(Type::Str)),
            ),
        );
    }

    // std.csv — RFC4180 subset (zz/csv/mod.zz).
    {
        let arr_str = Type::Array(Box::new(Type::Str));
        let rows_t = Type::Array(Box::new(arr_str.clone()));
        let json_t = Type::Json;
        for (name, params, ret) in [
            ("delim_first", vec![("delimiter", Type::Str)], Type::Str),
            ("json_escape", vec![("s", Type::Str)], Type::Str),
            ("parse", vec![("src", Type::Str)], rows_t.clone()),
            (
                "parse_delim",
                vec![("src", Type::Str), ("delimiter", Type::Str)],
                rows_t.clone(),
            ),
            (
                "needs_quote",
                vec![("cell", Type::Str), ("d", Type::Str)],
                Type::Bool,
            ),
            ("escape_cell", vec![("cell", Type::Str)], Type::Str),
            ("stringify", vec![("rows", rows_t.clone())], Type::Str),
            (
                "stringify_delim",
                vec![("rows", rows_t.clone()), ("delimiter", Type::Str)],
                Type::Str,
            ),
            ("header", vec![("rows", rows_t.clone())], arr_str.clone()),
            ("records", vec![("rows", rows_t.clone())], rows_t.clone()),
            ("len", vec![("rows", rows_t.clone())], Type::Int),
            (
                "get_cell",
                vec![
                    ("rows", rows_t.clone()),
                    ("r", Type::Int),
                    ("c", Type::Int),
                    ("default", Type::Str),
                ],
                Type::Str,
            ),
            ("validate", vec![("src", Type::Str)], Type::Bool),
            ("to_json", vec![("rows", rows_t.clone())], json_t.clone()),
        ] {
            let s = sig(params, ret);
            m.insert(format!("std.csv.{name}"), s.clone());
        }
    }

    // std.property — pure-ZZ property testing (zz/property/mod.zz). Both
    // spellings registered. Higher-order signatures use `Type::Func` so
    // named functions and `|x| ...` closures both check as arguments.
    {
        let t = Type::Named("T".to_string());
        let arr_t = Type::Array(Box::new(t.clone()));
        let gen_t = Type::Func(vec![Type::Int, Type::Int], Box::new(t.clone()));
        let shrink_t = Type::Func(vec![t.clone()], Box::new(arr_t.clone()));
        let prop_t = Type::Func(vec![t.clone()], Box::new(Type::Bool));
        let prop_int = Type::Func(vec![Type::Int], Box::new(Type::Bool));
        for (name, s) in [
            ("seed", sig(vec![("n", Type::Int)], Type::Int)),
            ("norm_state", sig(vec![("s", Type::Int)], Type::Int)),
            ("step", sig(vec![("state", Type::Int)], Type::Int)),
            (
                "stream",
                sig(vec![("seed", Type::Int), ("i", Type::Int)], Type::Int),
            ),
            (
                "int_in",
                sig(
                    vec![
                        ("seed", Type::Int),
                        ("i", Type::Int),
                        ("lo", Type::Int),
                        ("hi", Type::Int),
                    ],
                    Type::Int,
                ),
            ),
            (
                "pick",
                sig(
                    vec![("seed", Type::Int), ("i", Type::Int), ("n", Type::Int)],
                    Type::Int,
                ),
            ),
            (
                "boolean",
                sig(vec![("seed", Type::Int), ("i", Type::Int)], Type::Bool),
            ),
            (
                "alpha_char",
                sig(vec![("seed", Type::Int), ("i", Type::Int)], Type::Str),
            ),
            (
                "alpha_str",
                sig(
                    vec![("seed", Type::Int), ("i", Type::Int), ("len", Type::Int)],
                    Type::Str,
                ),
            ),
            (
                "shrink_int",
                sig(vec![("v", Type::Int)], Type::Array(Box::new(Type::Int))),
            ),
            (
                "shrink_str",
                sig(vec![("s", Type::Str)], Type::Array(Box::new(Type::Str))),
            ),
            (
                "shrink_value",
                sig_t(
                    vec![
                        ("prop", prop_t.clone()),
                        ("shrink", shrink_t.clone()),
                        ("v", t.clone()),
                    ],
                    t.clone(),
                ),
            ),
            (
                "check",
                sig_t(
                    vec![
                        ("gen", gen_t.clone()),
                        ("shrink", shrink_t.clone()),
                        ("prop", prop_t.clone()),
                        ("iters", Type::Int),
                        ("seed", Type::Int),
                    ],
                    Type::Unit,
                ),
            ),
            (
                "for_all_int",
                sig(
                    vec![
                        ("lo", Type::Int),
                        ("hi", Type::Int),
                        ("prop", prop_int.clone()),
                        ("iters", Type::Int),
                        ("seed", Type::Int),
                    ],
                    Type::Unit,
                ),
            ),
        ] {
            m.insert(format!("std.property.{name}"), s.clone());
        }
    }

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
        "bg_bright_black",
        "bg_bright_red",
        "bg_bright_green",
        "bg_bright_yellow",
        "bg_bright_blue",
        "bg_bright_magenta",
        "bg_bright_cyan",
        "bg_bright_white",
        "bold",
        "dim",
        "italic",
        "underline",
        "blink",
        "reverse",
        "strikethrough",
        "reset",
        "strip",
    ];
    for name in color_wraps {
        let s = sig(vec![("s", Type::Str)], Type::Str);
        m.insert(format!("std.colors.{name}"), s.clone());
    }
    for (name, params, ret) in [
        ("clamp255", vec![("v", Type::Int)], Type::Int),
        (
            "color256",
            vec![("s", Type::Str), ("n", Type::Int)],
            Type::Str,
        ),
        (
            "bg_256",
            vec![("s", Type::Str), ("n", Type::Int)],
            Type::Str,
        ),
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
    // `sqlz` is the only SQLite module. Handle variables named `db`
    // (`db := sqlz.open(..); db.exec(..)`) are method calls and keep
    // working.
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
    // NOTE: no `db.transaction` — `sqlz.transaction` only.
    let tx_t = Type::Named("T".to_string());
    let tx_sig = sig_t(
        vec![
            ("db", Type::Db),
            ("f", Type::Func(vec![Type::Db], Box::new(tx_t.clone()))),
        ],
        Type::Result(Box::new(tx_t.clone()), Box::new(Type::Str)),
    );
    m.insert("std.sqlz.transaction".into(), tx_sig.clone());

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
        readonly: false,
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
        readonly: false,
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
        readonly: false,
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
        readonly: false,
        extern_c_symbol: None,
    };
    for name in ["assert_approx_eq", "std.test.assert_approx_eq"] {
        m.insert(name.into(), approx_sig.clone());
    }
    // ── std.test pro assertions (P1) ─────────────────────────────
    // All dual-spelling (bare + `std.test.*`), lockstep with
    // `natives/assert`, `runtime/core.c`, `lower/native_impl`.
    let str_pair_sig = FuncSig {
        generics: Vec::new(),
        bounds: Vec::new(),
        params: vec![
            ("hay".to_string(), Type::Str),
            ("needle".to_string(), Type::Str),
        ],
        has_default: vec![false, false],
        ret: Type::Unit,
        is_extern: false,
        readonly: false,
        extern_c_symbol: None,
    };
    // assert_contains reuses (Str, Str) shape; starts/ends need own names
    // for diagnostics but identical types.
    let starts_sig = FuncSig {
        generics: Vec::new(),
        bounds: Vec::new(),
        params: vec![
            ("s".to_string(), Type::Str),
            ("prefix".to_string(), Type::Str),
        ],
        has_default: vec![false, false],
        ret: Type::Unit,
        is_extern: false,
        readonly: false,
        extern_c_symbol: None,
    };
    let ends_sig = FuncSig {
        generics: Vec::new(),
        bounds: Vec::new(),
        params: vec![
            ("s".to_string(), Type::Str),
            ("suffix".to_string(), Type::Str),
        ],
        has_default: vec![false, false],
        ret: Type::Unit,
        is_extern: false,
        readonly: false,
        extern_c_symbol: None,
    };
    for name in ["assert_contains", "std.test.assert_contains"] {
        m.insert(name.into(), str_pair_sig.clone());
    }
    for name in ["assert_starts_with", "std.test.assert_starts_with"] {
        m.insert(name.into(), starts_sig.clone());
    }
    for name in ["assert_ends_with", "std.test.assert_ends_with"] {
        m.insert(name.into(), ends_sig.clone());
    }
    // assert_len accepts any value (array/str/dict/bytes); runtime validates.
    // Generic `T` keeps the checker permissive, mirrors `len`.
    let len_sig = FuncSig {
        generics: vec!["T".to_string()],
        bounds: Vec::new(),
        params: vec![
            ("val".to_string(), Type::Named("T".to_string())),
            ("n".to_string(), Type::Int),
        ],
        has_default: vec![false, false],
        ret: Type::Unit,
        is_extern: false,
        readonly: false,
        extern_c_symbol: None,
    };
    for name in ["assert_len", "std.test.assert_len"] {
        m.insert(name.into(), len_sig.clone());
    }
    // Numeric comparisons accept int|float (mixed ok); runtime uses to_float.
    let cmp_sig = FuncSig {
        generics: vec!["T".to_string(), "U".to_string()],
        bounds: Vec::new(),
        params: vec![
            ("left".to_string(), Type::Named("T".to_string())),
            ("right".to_string(), Type::Named("U".to_string())),
        ],
        has_default: vec![false, false],
        ret: Type::Unit,
        is_extern: false,
        readonly: false,
        extern_c_symbol: None,
    };
    for name in ["assert_gt", "std.test.assert_gt"] {
        m.insert(name.into(), cmp_sig.clone());
    }
    for name in ["assert_lt", "std.test.assert_lt"] {
        m.insert(name.into(), cmp_sig.clone());
    }
    let opt_t = Type::Named("T".to_string());
    let is_some_sig = FuncSig {
        generics: vec!["T".to_string()],
        bounds: Vec::new(),
        params: vec![("val".to_string(), Type::Option(Box::new(opt_t.clone())))],
        has_default: vec![false],
        ret: Type::Unit,
        is_extern: false,
        readonly: false,
        extern_c_symbol: None,
    };
    for name in ["assert_is_some", "std.test.assert_is_some"] {
        m.insert(name.into(), is_some_sig.clone());
    }
    for name in ["assert_is_none", "std.test.assert_is_none"] {
        m.insert(name.into(), is_some_sig.clone());
    }
    let res_t = Type::Named("T".to_string());
    let res_e = Type::Named("E".to_string());
    let is_ok_sig = FuncSig {
        generics: vec!["T".to_string(), "E".to_string()],
        bounds: Vec::new(),
        params: vec![(
            "val".to_string(),
            Type::Result(Box::new(res_t.clone()), Box::new(res_e.clone())),
        )],
        has_default: vec![false],
        ret: Type::Unit,
        is_extern: false,
        readonly: false,
        extern_c_symbol: None,
    };
    for name in ["assert_is_ok", "std.test.assert_is_ok"] {
        m.insert(name.into(), is_ok_sig.clone());
    }
    for name in ["assert_is_err", "std.test.assert_is_err"] {
        m.insert(name.into(), is_ok_sig.clone());
    }
    // ── std.test pro helpers (P2) ──────────────────────────────
    // Bare builtins + `std.test.*` (lockstep with `natives/test_helpers`,
    // `runtime/core.c` `zz_test_*`, `lower/native_impl`).
    let temp_dir_sig = FuncSig {
        generics: Vec::new(),
        bounds: Vec::new(),
        params: vec![("prefix".to_string(), Type::Str)],
        has_default: vec![false],
        ret: Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
        is_extern: false,
        readonly: false,
        extern_c_symbol: None,
    };
    for name in ["temp_dir", "std.test.temp_dir"] {
        m.insert(name.into(), temp_dir_sig.clone());
    }
    let free_port_sig = FuncSig {
        generics: Vec::new(),
        bounds: Vec::new(),
        params: Vec::new(),
        has_default: Vec::new(),
        ret: Type::Result(Box::new(Type::Int), Box::new(Type::Str)),
        is_extern: false,
        readonly: false,
        extern_c_symbol: None,
    };
    for name in ["free_port", "std.test.free_port"] {
        m.insert(name.into(), free_port_sig.clone());
    }
    // Optimization barrier for benchmarks: identity function the AOT C
    // compiler cannot see through (lockstep with `natives/test_helpers`
    // + `zz_black_box` C twin, which escapes the value via asm).
    let black_box_sig = FuncSig {
        generics: vec!["T".to_string()],
        bounds: Vec::new(),
        params: vec![("val".to_string(), Type::Named("T".to_string()))],
        has_default: vec![false],
        ret: Type::Named("T".to_string()),
        is_extern: false,
        readonly: false,
        extern_c_symbol: None,
    };
    for name in ["black_box", "std.test.black_box"] {
        m.insert(name.into(), black_box_sig.clone());
    }
    // Snapshot assertion: file-backed golden values under a snapshots dir.
    // Lockstep with `natives/assert::assert_snapshot_fn` + C twin.
    let snapshot_sig = FuncSig {
        generics: Vec::new(),
        bounds: Vec::new(),
        params: vec![
            ("actual".to_string(), Type::Str),
            ("name".to_string(), Type::Str),
        ],
        has_default: vec![false, false],
        ret: Type::Unit,
        is_extern: false,
        readonly: false,
        extern_c_symbol: None,
    };
    for name in ["assert_snapshot", "std.test.assert_snapshot"] {
        m.insert(name.into(), snapshot_sig.clone());
    }
    let fail_sig = FuncSig {
        generics: Vec::new(),
        bounds: Vec::new(),
        params: vec![("msg".to_string(), Type::Str)],
        has_default: vec![false],
        ret: Type::Unit,
        is_extern: false,
        readonly: false,
        extern_c_symbol: None,
    };
    for name in ["fail", "panic", "std.test.fail", "std.test.panic"] {
        m.insert(name.into(), fail_sig.clone());
    }

    m
}

#[cfg(test)]
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
        assert!(funcs.contains_key("std.http.body_bytes"));
        assert!(funcs.contains_key("std.http.respond"));
        assert!(funcs.contains_key("std.http.test_req"));
        assert!(funcs.contains_key("std.http.serve_dir_at"));
        assert!(funcs.contains_key("std.http.pipe_post"));
        assert!(funcs.contains_key("std.http.rate_limit"));
        assert!(funcs.contains_key("std.http.with_headers"));
        assert!(funcs.contains_key("std.http.listen_cfg"));
        assert!(funcs.contains_key("std.http.cors"));
        assert!(funcs.contains_key("std.http.secure_headers"));
        assert!(funcs.contains_key("std.encoding.base64_decode_bytes"));
        assert!(funcs.contains_key("std.fs.read_bytes"));
        assert!(funcs.contains_key("std.fs.write"));
        assert!(funcs.contains_key("std.fs.append"));
        assert!(funcs.contains_key("std.fs.copy"));
        assert!(funcs.contains_key("std.fs.move"));
        assert!(!funcs.contains_key("std.fs.rename"));
        assert!(funcs.contains_key("std.fs.is_file"));
        assert!(funcs.contains_key("std.fs.is_dir"));
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
        assert!(funcs.contains_key("clone"));
        assert!(funcs.contains_key("std.task.spawn"));
        assert!(funcs.contains_key("std.task.join"));
        assert!(funcs.contains_key("std.task.try_join"));
        assert!(funcs.contains_key("std.sqlz.open"));
        assert!(funcs.contains_key("std.sqlz.exec"));
        assert!(funcs.contains_key("std.sqlz.query"));
        assert!(funcs.contains_key("std.sqlz.close"));
        assert!(funcs.contains_key("std.sqlz.postgres.connect"));
        assert!(funcs.contains_key("std.sqlz.postgres.exec"));
        assert!(funcs.contains_key("std.sqlz.postgres.query"));
        assert!(funcs.contains_key("std.sqlz.postgres.close"));
        assert!(funcs.contains_key("std.sqlz.mysql.connect"));
        assert!(funcs.contains_key("std.sqlz.mysql.exec"));
        assert!(funcs.contains_key("std.sqlz.mysql.query"));
        assert!(funcs.contains_key("std.sqlz.mysql.close"));
        assert!(funcs.contains_key("std.sqlz.transaction"));
        // Math constants are static values, not zero-arg functions.
        let consts = stdlib_consts();
        assert!(consts.contains_key("std.math.PI"));
        assert!(consts.contains_key("std.math.E"));
        assert!(consts.contains_key("std.math.TAU"));
        // ... and carry checker signatures so `std.math.PI` resolves
        // import-free (like every other `std.*` name).
        assert!(funcs.contains_key("std.math.PI"));
        assert!(funcs.contains_key("std.math.E"));
        assert!(funcs.contains_key("std.math.INF"));
        assert!(funcs.contains_key("std.math.NAN"));
        // vec.enumerate: method spelling of bare `enumerate`.
        assert!(funcs.contains_key("std.vec.enumerate"));
        // path helpers (pure-ZZ).
        assert!(funcs.contains_key("std.path.join"));
        assert!(funcs.contains_key("std.path.join_all"));
        assert!(funcs.contains_key("std.path.normalize"));
        assert!(funcs.contains_key("std.path.basename"));
        assert!(funcs.contains_key("std.path.dirname"));
        assert!(funcs.contains_key("std.path.is_absolute"));
        assert!(funcs.contains_key("std.path.extension"));
        assert!(funcs.contains_key("std.colors.red"));
        assert!(funcs.contains_key("std.colors.rgb"));
        assert!(funcs.contains_key("std.colors.hex"));
        assert!(funcs.contains_key("std.colors.bold"));
        assert!(funcs.contains_key("std.colors.strip"));
        assert!(funcs.contains_key("std.colors.reverse"));
        assert!(funcs.contains_key("std.colors.strikethrough"));
        assert!(funcs.contains_key("std.colors.blink"));
        assert!(funcs.contains_key("std.colors.bg_bright_red"));
        assert!(funcs.contains_key("std.colors.bg_bright_white"));
        assert!(funcs.contains_key("std.colors.color256"));
        assert!(funcs.contains_key("std.colors.bg_256"));
        assert!(funcs.contains_key("dbg"));
        // net method aliases + binary transfers + shutdown.
        for name in [
            "std.net.tcp_read_bytes",
            "std.net.tcp_write_bytes",
            "std.net.tcp_shutdown",
            "std.net.shutdown",
            "std.net.accept",
            "std.net.read",
            "std.net.read_line",
            "std.net.read_bytes",
            "std.net.write",
            "std.net.write_bytes",
            "std.net.peer_addr",
            "std.net.local_addr",
            "std.net.set_read_timeout",
            "std.net.set_write_timeout",
            "std.net.tcp_read_exact",
            "std.net.tls_connect",
            "std.net.tls_connect_with_ca",
            "std.net.tls_connect_insecure",
            "std.net.tls_listen",
            "std.net.tls_accept",
        ] {
            assert!(funcs.contains_key(name), "missing {name}");
        }
        // http helpers (pure-ZZ, compiled from zz/http/mod.zz).
        for name in [
            "std.http.route",
            "std.http.ok",
            "std.http.created",
            "std.http.not_found",
            "std.http.redirect",
        ] {
            assert!(funcs.contains_key(name), "missing {name}");
        }
        // Unified client + JSON client.
        for name in ["std.http.fetch", "std.http.post_json"] {
            assert!(funcs.contains_key(name), "missing {name}");
        }
        // 9 canonical `std.str.*` twins of the `str.*` method entries
        // (trim, to_upper, to_lower, replace, starts_with, ends_with,
        // join, trim_start, trim_end).
        for name in [
            "std.str.trim",
            "std.str.to_upper",
            "std.str.to_lower",
            "std.str.replace",
            "std.str.starts_with",
            "std.str.ends_with",
            "std.str.join",
            "std.str.trim_start",
            "std.str.trim_end",
        ] {
            assert!(funcs.contains_key(name), "missing {name}");
        }
        // std.term — raw mode, single-key reads, size (both spellings).
        for name in [
            "std.term.enable_raw",
            "std.term.disable_raw",
            "std.term.read_key",
            "std.term.poll",
            "std.term.get_size",
            "std.term.is_tty",
            "std.term.flush",
        ] {
            assert!(funcs.contains_key(name), "missing {name}");
        }
        // New pure-ZZ modules (map, set, dec, bytes, csv, time dates).
        for name in [
            "std.map.has",
            "std.map.get_or",
            "std.map.merge",
            "std.map.keys",
            "std.set.has",
            "std.set.union",
            "std.set.intersect",
            "std.dec.add",
            "std.dec.cmp",
            "std.dec.format",
            "std.str.builder",
            "std.str.finish",
            "std.bytes.builder",
            "std.bytes.finish",
            "std.cell.new",
            "std.cell.get",
            "std.cell.set",
            "std.cell.drop",
            "std.net.unix_connect",
            "std.net.unix_listen",
            "std.net.unix_accept",
            "std.net.unix_listener_close",
            "std.net.unix_write",
            "std.net.unix_write_bytes",
            "std.net.unix_read",
            "std.net.unix_read_bytes",
            "std.net.unix_read_exact",
            "std.net.unix_readline",
            "std.net.unix_set_read_timeout",
            "std.net.unix_set_write_timeout",
            "std.net.unix_close",
            "std.net.unix_shutdown",
            "std.unix_listener.accept",
            "std.unix_listener.close",
            "std.net.tls_readline",
            "std.net.tls_shutdown",
            "std.bytes.push_byte",
            "std.csv.parse",
            "std.csv.stringify",
            "std.csv.to_json",
            "std.property.seed",
            "std.property.int_in",
            "std.property.check",
            "std.property.for_all_int",
            "std.time.parse_rfc3339",
            "std.time.format_rfc3339",
            "std.time.add_days",
            "std.str.starts_with_at",
            "std.str.ends_with_at",
            "std.str.trim_span",
            "std.str.find_in",
            "std.str.rfind_in",
            "std.str.count_in",
            "std.str.bytes",
            "std.str.byte_len",
            "std.bytes.to_str",
            "std.bytes.to_ints",
            "std.str.classify",
            "std.fs.scan_counts",
            "std.fs.is_generated",
            "std.str.find",
            "std.str.rfind",
            "ord",
            "std.str.ord",
            "chr",
            "std.str.chr",
            "assert_contains",
            "std.test.assert_contains",
            "assert_starts_with",
            "std.test.assert_starts_with",
            "assert_ends_with",
            "std.test.assert_ends_with",
            "assert_len",
            "std.test.assert_len",
            "assert_gt",
            "std.test.assert_gt",
            "assert_lt",
            "std.test.assert_lt",
            "assert_is_some",
            "std.test.assert_is_some",
            "assert_is_none",
            "std.test.assert_is_none",
            "assert_is_ok",
            "std.test.assert_is_ok",
            "assert_is_err",
            "std.test.assert_is_err",
            "temp_dir",
            "std.test.temp_dir",
            "free_port",
            "std.test.free_port",
            "assert_snapshot",
            "std.test.assert_snapshot",
            "black_box",
            "std.test.black_box",
        ] {
            assert!(funcs.contains_key(name), "missing {name}");
        }
        assert_eq!(funcs.len(), 685);
    }

    #[test]
    fn no_bare_twin_seeds() {
        // B invariant: seeds hold canonical `std.*` names only. A bare
        // `module.func` key may exist only when no `std.module.func` seed
        // exists - never as a twin. Namespaces without an importable
        // module behind them (`span.end`, ...) keep their bare seeds:
        // the test derives the rule from `canonical_std_name` itself
        // instead of a hardcoded list. Bare callers resolve via that
        // same fallback.
        let funcs = stdlib_funcs();
        let mut twins = Vec::new();
        for k in funcs.keys() {
            if k.contains('.')
                && !k.starts_with("std.")
                && funcs.contains_key(&format!("std.{k}"))
                && zz_checker::canonical_std_name(k).is_some()
            {
                twins.push(k.clone());
            }
        }
        assert!(
            twins.is_empty(),
            "bare twin seeds (use std.* only): {twins:?}"
        );
    }

    #[test]
    fn vec_funcs_are_generic() {
        let funcs = stdlib_funcs();
        assert_eq!(funcs["std.vec.push"].generics, vec!["T"]);
        assert_eq!(funcs["std.vec.push"].params[1].1, Type::Named("T".into()));
    }
}
