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
        has_default: vec![],
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
        has_default: vec![],
        ret,
    }
}

/// Build a signature generic over `T, U`.
fn sig_tu(params: Vec<(&str, Type)>, ret: Type) -> FuncSig {
    FuncSig {
        generics: vec!["T".to_string(), "U".to_string()],
        params: params
            .into_iter()
            .map(|(n, t)| (n.to_string(), t))
            .collect(),
        has_default: vec![],
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

    // Top-level builtins — available without import.
    m.insert("print".into(), sig_t(vec![("v", t.clone())], Type::Unit));
    m.insert("println".into(), sig_t(vec![("v", t.clone())], Type::Unit));
    m.insert("input".into(), sig(vec![], Type::Str));

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
            params: vec![("res".to_string(), result_t.clone())],
            has_default: vec![],
            ret: t.clone(),
        },
    );
    m.insert(
        "result.unwrap_or".into(),
        FuncSig {
            generics: vec!["T".to_string(), "E".to_string()],
            params: vec![
                ("res".to_string(), result_t.clone()),
                ("default".to_string(), t.clone()),
            ],
            has_default: vec![],
            ret: t.clone(),
        },
    );
    m.insert(
        "result.expect".into(),
        FuncSig {
            generics: vec!["T".to_string(), "E".to_string()],
            params: vec![
                ("res".to_string(), result_t),
                ("msg".to_string(), Type::Str),
            ],
            has_default: vec![],
            ret: t,
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
    m.insert("std.json.null".into(), sig(vec![], json_t));

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
    let result_bool = Type::Result(Box::new(Type::Bool), Box::new(Type::Str));

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
    m.insert(
        "std.fs.read_to_string".into(),
        sig(
            vec![("path", Type::Str)],
            Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.fs.write".into(),
        sig(
            vec![("path", Type::Str), ("content", Type::Str)],
            Type::Result(Box::new(Type::Unit), Box::new(Type::Str)),
        ),
    );
    m.insert(
        "std.fs.remove_file".into(),
        sig(
            vec![("path", Type::Str)],
            Type::Result(Box::new(Type::Unit), Box::new(Type::Str)),
        ),
    );

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

    // ── std.math constants ──
    m.insert("std.math.PI".into(), sig(vec![], Type::Float));
    m.insert("std.math.E".into(), sig(vec![], Type::Float));
    m.insert("std.math.TAU".into(), sig(vec![], Type::Float));
    m.insert("std.math.INF".into(), sig(vec![], Type::Float));
    m.insert("std.math.NAN".into(), sig(vec![], Type::Float));

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
        assert!(funcs.contains_key("std.fs.write"));
        assert!(funcs.contains_key("std.fs.remove_file"));
        assert!(funcs.contains_key("std.env.get_var"));
        assert!(funcs.contains_key("std.env.var"));
        assert!(funcs.contains_key("std.env.args"));
        assert!(funcs.contains_key("std.math.abs"));
        assert!(funcs.contains_key("std.math.random"));
        assert!(funcs.contains_key("std.math.PI"));
        assert!(funcs.contains_key("std.math.E"));
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
        assert_eq!(funcs.len(), 157);
    }

    #[test]
    fn vec_funcs_are_generic() {
        let funcs = stdlib_funcs();
        assert_eq!(funcs["std.vec.push"].generics, vec!["T"]);
        assert_eq!(funcs["std.vec.push"].params[1].1, Type::Named("T".into()));
    }
}
