use super::super::{stdlib_funcs, stdlib_natives};
use zz_runtime::{EvalError, Interp, Span, Value};

fn call(name: &str, args: Vec<Value>) -> Result<Value, EvalError> {
    let mut interp = Interp::new();
    let mut args = args;
    let entry = stdlib_natives()[name];
    (entry.f)(&mut interp, &mut args, Span::new(0, 0))
}

#[test]
fn str_length_counts_chars() {
    assert_eq!(
        call(
            "std.str.length",
            vec![Value::Str("héllo".to_string().into())]
        )
        .unwrap(),
        Value::Int(5)
    );
}

#[test]
fn str_split_splits() {
    assert_eq!(
        call(
            "std.str.split",
            vec![
                Value::Str("a,b,c".to_string().into()),
                Value::Str(",".to_string().into())
            ]
        )
        .unwrap(),
        Value::Array(Box::new(vec![
            Value::Str("a".to_string().into()),
            Value::Str("b".to_string().into()),
            Value::Str("c".to_string().into()),
        ]))
    );
}

#[test]
fn str_contains_finds_substring() {
    assert_eq!(
        call(
            "std.str.contains",
            vec![
                Value::Str("hello".to_string().into()),
                Value::Str("ell".to_string().into())
            ]
        )
        .unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        call(
            "std.str.contains",
            vec![
                Value::Str("hello".to_string().into()),
                Value::Str("xyz".to_string().into())
            ]
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
            vec![Value::Array(Box::new(vec![Value::Int(1), Value::Int(2)]))]
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
            vec![Value::Array(Box::new(vec![Value::Int(1)])), Value::Int(2),]
        )
        .unwrap(),
        Value::Array(Box::new(vec![Value::Int(1), Value::Int(2)]))
    );
}

#[test]
fn vec_pop_removes_last() {
    assert_eq!(
        call(
            "std.vec.pop",
            vec![Value::Array(Box::new(vec![Value::Int(1), Value::Int(2)]))]
        )
        .unwrap(),
        Value::Array(Box::new(vec![Value::Int(1)]))
    );
}

#[test]
fn vec_pop_empty_errors() {
    let err = call("std.vec.pop", vec![Value::Array(Box::default())]).unwrap_err();
    assert!(err.message.contains("empty array"), "{}", err.message);
}

#[test]
fn wrong_type_errors() {
    let err = call("std.str.length", vec![Value::Int(5)]).unwrap_err();
    assert!(err.message.contains("expects a string"), "{}", err.message);
}

/// `input()` is covered by hermetic stdin integration tests in
/// `crates/zz_cli/tests/e2e.rs` (`e2e_input_*`). A unit test here would read
/// the real process stdin and hang on interactive terminals (stdin is only
/// /dev/null under CI).

#[test]
fn every_funcs_key_has_a_native() {
    // Drift census: the checker registry (`stdlib_funcs`) and the interpreter
    // registry (`stdlib_natives`) must stay in lockstep. Every signature the
    // checker knows must resolve to a runtime implementation.
    //
    // Exception: pure-ZZ stdlib functions (str.repeat, str.count, math.sum,
    // math.product, math.count, vec.fold) are implemented in .zz files and
    // compiled at startup — they have no Rust native entry.
    let pure_zz_funcs = [
        // str helpers
        "std.str.repeat",
        "std.str.count",
        "std.str.is_empty",
        "std.str.reverse",
        "std.str.pad_left",
        "std.str.pad_right",
        "str.repeat",
        "str.count",
        "str.is_empty",
        "str.reverse",
        "str.pad_left",
        "str.pad_right",
        // math helpers
        "std.math.sum",
        "std.math.product",
        "std.math.count",
        "std.math.min",
        "std.math.max",
        "std.math.is_even",
        "std.math.is_odd",
        "std.math.min_arr",
        "std.math.max_arr",
        "std.math.sum_f",
        "std.math.product_f",
        "std.math.mean_f",
        "std.math.median_f",
        "math.sum",
        "math.product",
        "math.count",
        "math.min",
        "math.max",
        "math.is_even",
        "math.is_odd",
        "math.min_arr",
        "math.max_arr",
        "math.sum_f",
        "math.product_f",
        "math.mean_f",
        "math.median_f",
        // vec helpers
        "std.vec.fold",
        "std.vec.sum",
        "std.vec.product",
        "std.vec.min_val",
        "std.vec.max_val",
        "std.vec.sum_f",
        "std.vec.product_f",
        "std.vec.concat",
        "std.vec.flatten",
        "std.vec.index_of",
        "std.vec.last_index_of",
        "vec.fold",
        "vec.sum",
        "vec.product",
        "vec.min_val",
        "vec.max_val",
        "vec.sum_f",
        "vec.product_f",
        "vec.concat",
        "vec.flatten",
        "vec.index_of",
        "vec.last_index_of",
        // json helpers (pure-ZZ, compiled from zz/json/mod.zz)
        "std.json.validate",
        "std.json.parse_or",
        "std.json.parse_or_null",
        "std.json.path_exists",
        "std.json.path_get_or",
        "std.json.is_null",
        "std.json.is_bool",
        "std.json.is_number",
        "std.json.is_string",
        "std.json.is_array",
        "std.json.is_object",
        "std.json.is_empty",
        "json.validate",
        "json.parse_or",
        "json.parse_or_null",
        "json.path_exists",
        "json.path_get_or",
        "json.is_null",
        "json.is_bool",
        "json.is_number",
        "json.is_string",
        "json.is_array",
        "json.is_object",
        "json.is_empty",
        // regexp helpers (pure-ZZ, compiled from zz/regexp/mod.zz)
        "Regexp.new",
        "std.regexp.is_email",
        "regexp.is_email",
        // Duration helpers (pure-ZZ, compiled from zz/time/mod.zz)
        "time.micros",
        "time.millis",
        "time.secs",
        "time.to_micros",
        "time.to_millis",
        "time.to_secs",
        "time.to_nanos",
        "time.sleep",
        // Calendar dates (pure-ZZ, zz/time date section, dict-based)
        "std.time.make_date",
        "time.make_date",
        "std.time.epoch_fallback",
        "time.epoch_fallback",
        "std.time.is_leap",
        "time.is_leap",
        "std.time.days_in_month",
        "time.days_in_month",
        "std.time.date_valid",
        "time.date_valid",
        "std.time.days_from_civil",
        "time.days_from_civil",
        "std.time.civil_from_days",
        "time.civil_from_days",
        "std.time.parse_rfc3339",
        "time.parse_rfc3339",
        "std.time.pad2",
        "time.pad2",
        "std.time.format_rfc3339",
        "time.format_rfc3339",
        "std.time.to_epoch_days",
        "time.to_epoch_days",
        "std.time.from_epoch_days",
        "time.from_epoch_days",
        "std.time.add_days",
        "time.add_days",
        "std.time.diff_days",
        "time.diff_days",
        // map helpers (pure-ZZ, zz/collections/map.zz)
        "std.map.has",
        "map.has",
        "std.map.get_or",
        "map.get_or",
        "std.map.get_str",
        "map.get_str",
        "std.map.keys",
        "map.keys",
        "std.map.keys_str",
        "map.keys_str",
        "std.map.values",
        "map.values",
        "std.map.values_str",
        "map.values_str",
        "std.map.len",
        "map.len",
        "std.map.is_empty",
        "map.is_empty",
        "std.map.merge",
        "map.merge",
        "std.map.merge_str",
        "map.merge_str",
        "std.map.remove",
        "map.remove",
        // set helpers (pure-ZZ, zz/collections/set.zz)
        "std.set.has",
        "set.has",
        "std.set.has_int",
        "set.has_int",
        "std.set.insert",
        "set.insert",
        "std.set.insert_int",
        "set.insert_int",
        "std.set.remove",
        "set.remove",
        "std.set.remove_int",
        "set.remove_int",
        "std.set.union",
        "set.union",
        "std.set.union_int",
        "set.union_int",
        "std.set.intersect",
        "set.intersect",
        "std.set.intersect_int",
        "set.intersect_int",
        "std.set.diff",
        "set.diff",
        "std.set.len",
        "set.len",
        "std.set.is_empty",
        "set.is_empty",
        // dec helpers (pure-ZZ, zz/dec/mod.zz)
        "std.dec.is_valid",
        "dec.is_valid",
        "std.dec.scale_of",
        "dec.scale_of",
        "std.dec.pow10",
        "dec.pow10",
        "std.dec.to_scaled",
        "dec.to_scaled",
        "std.dec.from_scaled",
        "dec.from_scaled",
        "std.dec.trim_zeros",
        "dec.trim_zeros",
        "std.dec.add",
        "dec.add",
        "std.dec.sub",
        "dec.sub",
        "std.dec.mul",
        "dec.mul",
        "std.dec.cmp",
        "dec.cmp",
        "std.dec.eq",
        "dec.eq",
        "std.dec.lt",
        "dec.lt",
        "std.dec.gt",
        "dec.gt",
        "std.dec.format",
        "dec.format",
        // builders (pure-ZZ, zz/bytes/mod.zz)
        "str.builder",
        "std.str.builder",
        "str.push_part",
        "std.str.push_part",
        "str.finish",
        "std.str.finish",
        "str.builder_len",
        "std.str.builder_len",
        "str.join_parts",
        "std.str.join_parts",
        "bytes.builder",
        "std.bytes.builder",
        "bytes.push_byte",
        "std.bytes.push_byte",
        "bytes.extend",
        "std.bytes.extend",
        "bytes.len_of",
        "std.bytes.len_of",
        "bytes.from_ints",
        "std.bytes.from_ints",
        // csv helpers (pure-ZZ, zz/csv/mod.zz)
        "std.csv.delim_first",
        "csv.delim_first",
        "std.csv.json_escape",
        "csv.json_escape",
        "std.csv.parse",
        "csv.parse",
        "std.csv.parse_delim",
        "csv.parse_delim",
        "std.csv.needs_quote",
        "csv.needs_quote",
        "std.csv.escape_cell",
        "csv.escape_cell",
        "std.csv.stringify",
        "csv.stringify",
        "std.csv.stringify_delim",
        "csv.stringify_delim",
        "std.csv.header",
        "csv.header",
        "std.csv.records",
        "csv.records",
        "std.csv.len",
        "csv.len",
        "std.csv.get_cell",
        "csv.get_cell",
        "std.csv.validate",
        "csv.validate",
        "std.csv.to_json",
        "csv.to_json",
        // ArgsParser constructor (pure-ZZ, compiled from zz/args/mod.zz)
        "ArgsParser.new",
        // path helpers (pure-ZZ, compiled from zz/path/mod.zz)
        "std.path.join",
        "std.path.join_all",
        "std.path.normalize",
        "std.path.basename",
        "std.path.dirname",
        "std.path.is_absolute",
        "std.path.extension",
        "path.join",
        "path.join_all",
        "path.normalize",
        "path.basename",
        "path.dirname",
        "path.is_absolute",
        "path.extension",
        // http helpers (pure-ZZ, compiled from zz/http/mod.zz)
        "std.http.use",
        "http.use",
        "std.http.ok",
        "std.http.created",
        "std.http.not_found",
        "std.http.redirect",
        "http.ok",
        "http.created",
        "http.not_found",
        "http.redirect",
        "std.http.cors",
        "http.cors",
        "std.http.secure_headers",
        "http.secure_headers",
        "std.http.secure_header_dict",
        "http.secure_header_dict",
        "std.http.csrf_token",
        "http.csrf_token",
        "std.http.csrf_check",
        "http.csrf_check",
        "std.http.request_id",
        "http.request_id",
        // math numeric constants (true Float values, see `stdlib_consts`;
        // no runtime function to implement)
        "std.math.PI",
        "std.math.E",
        "std.math.TAU",
        "std.math.SQRT_2",
        "std.math.SQRT_1_2",
        "std.math.LN_2",
        "std.math.LN_10",
        "std.math.LOG10_E",
        "std.math.LOG2_E",
        "std.math.INF",
        "std.math.NAN",
        // colors helpers (pure-ZZ, compiled from zz/colors/mod.zz)
        "std.colors.black",
        "std.colors.red",
        "std.colors.green",
        "std.colors.yellow",
        "std.colors.blue",
        "std.colors.magenta",
        "std.colors.cyan",
        "std.colors.white",
        "std.colors.bright_black",
        "std.colors.bright_red",
        "std.colors.bright_green",
        "std.colors.bright_yellow",
        "std.colors.bright_blue",
        "std.colors.bright_magenta",
        "std.colors.bright_cyan",
        "std.colors.bright_white",
        "std.colors.bg_black",
        "std.colors.bg_red",
        "std.colors.bg_green",
        "std.colors.bg_yellow",
        "std.colors.bg_blue",
        "std.colors.bg_magenta",
        "std.colors.bg_cyan",
        "std.colors.bg_white",
        "std.colors.bg_bright_black",
        "std.colors.bg_bright_red",
        "std.colors.bg_bright_green",
        "std.colors.bg_bright_yellow",
        "std.colors.bg_bright_blue",
        "std.colors.bg_bright_magenta",
        "std.colors.bg_bright_cyan",
        "std.colors.bg_bright_white",
        "std.colors.bold",
        "std.colors.dim",
        "std.colors.italic",
        "std.colors.underline",
        "std.colors.blink",
        "std.colors.reverse",
        "std.colors.strikethrough",
        "std.colors.reset",
        "std.colors.strip",
        "std.colors.clamp255",
        "std.colors.color256",
        "std.colors.bg_256",
        "std.colors.rgb",
        "std.colors.bg_rgb",
        "std.colors.hex_val",
        "std.colors.hex_byte",
        "std.colors.hex",
        "std.colors.hex6",
        "std.colors.hex3",
        "colors.black",
        "colors.red",
        "colors.green",
        "colors.yellow",
        "colors.blue",
        "colors.magenta",
        "colors.cyan",
        "colors.white",
        "colors.bright_black",
        "colors.bright_red",
        "colors.bright_green",
        "colors.bright_yellow",
        "colors.bright_blue",
        "colors.bright_magenta",
        "colors.bright_cyan",
        "colors.bright_white",
        "colors.bg_black",
        "colors.bg_red",
        "colors.bg_green",
        "colors.bg_yellow",
        "colors.bg_blue",
        "colors.bg_magenta",
        "colors.bg_cyan",
        "colors.bg_white",
        "colors.bg_bright_black",
        "colors.bg_bright_red",
        "colors.bg_bright_green",
        "colors.bg_bright_yellow",
        "colors.bg_bright_blue",
        "colors.bg_bright_magenta",
        "colors.bg_bright_cyan",
        "colors.bg_bright_white",
        "colors.bold",
        "colors.dim",
        "colors.italic",
        "colors.underline",
        "colors.blink",
        "colors.reverse",
        "colors.strikethrough",
        "colors.reset",
        "colors.strip",
        "colors.clamp255",
        "colors.color256",
        "colors.bg_256",
        "colors.rgb",
        "colors.bg_rgb",
        "colors.hex_val",
        "colors.hex_byte",
        "colors.hex",
        "colors.hex6",
        "colors.hex3",
    ];
    let funcs = stdlib_funcs();
    let natives = stdlib_natives();
    let missing: Vec<String> = funcs
        .keys()
        .filter(|k| !natives.contains_key(*k) && !pure_zz_funcs.contains(&k.as_str()))
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "stdlib_funcs keys without a stdlib_natives impl: {missing:?}"
    );
    let untyped: Vec<String> = natives
        .keys()
        .filter(|k| !funcs.contains_key(*k))
        .cloned()
        .collect();
    assert!(
        untyped.is_empty(),
        "stdlib_natives keys without a stdlib_funcs signature: {untyped:?}"
    );
}
