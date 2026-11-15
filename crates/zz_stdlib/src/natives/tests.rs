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
        Value::Array(std::sync::Arc::new(vec![
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
            vec![Value::Array(std::sync::Arc::new(vec![
                Value::Int(1),
                Value::Int(2)
            ]))]
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
            vec![
                Value::Array(std::sync::Arc::new(vec![Value::Int(1)])),
                Value::Int(2),
            ]
        )
        .unwrap(),
        Value::Array(std::sync::Arc::new(vec![Value::Int(1), Value::Int(2)]))
    );
}

#[test]
fn vec_pop_removes_last() {
    assert_eq!(
        call(
            "std.vec.pop",
            vec![Value::Array(std::sync::Arc::new(vec![
                Value::Int(1),
                Value::Int(2)
            ]))]
        )
        .unwrap(),
        Value::Array(std::sync::Arc::new(vec![Value::Int(1)]))
    );
}

#[test]
fn vec_pop_empty_errors() {
    let err = call(
        "std.vec.pop",
        vec![Value::Array(std::sync::Arc::new(Vec::new()))],
    )
    .unwrap_err();
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
    // Exceptions are classified once in `zz_stdlib_meta::inventory`:
    // - pure-ZZ functions are implemented in embedded `.zz` sources and run
    //   as compiled ZZ (no `NativeEntry`);
    // - `std.math.*` numeric constants are `Float` values in
    //   `stdlib_consts`, not functions.
    // The same inventory is the tier source for the AOT catalog, so this
    // test cannot drift away from the table.
    use zz_stdlib_meta::inventory;
    let funcs = stdlib_funcs();
    let natives = stdlib_natives();
    let missing: Vec<String> = funcs
        .keys()
        .filter(|k| {
            !natives.contains_key(*k) && !inventory::is_pure_zz(k) && !inventory::is_const(k)
        })
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

/// Snapshot tests share two process-global env vars; serialize them so
/// parallel libtest workers never interleave dir/mode settings.
static SNAPSHOT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Env helpers for snapshot tests: the lock above serializes them, so the
/// process-global unsafety contract holds (same as `std.env.set`).
fn snap_set_var(k: &str, v: &std::path::Path) {
    unsafe { std::env::set_var(k, v) };
}
fn snap_set_str(k: &str, v: &str) {
    unsafe { std::env::set_var(k, v) };
}
fn snap_remove_var(k: &str) {
    unsafe { std::env::remove_var(k) };
}

fn snap_str(s: &str) -> Value {
    Value::Str(s.to_string().into())
}

fn snap_call(actual: &str, name: &str) -> Result<Value, EvalError> {
    call("assert_snapshot", vec![snap_str(actual), snap_str(name)])
}

fn unique_snap_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zz-snap-unit-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn snapshot_match_and_missing() {
    let _guard = SNAPSHOT_ENV_LOCK.lock().unwrap();
    let dir = unique_snap_dir("match");
    snap_set_var("ZZ_SNAPSHOTS_DIR", &dir);
    snap_remove_var("ZZ_UPDATE_SNAPSHOTS");
    std::fs::write(dir.join("hi.snap"), "hello").unwrap();
    assert_eq!(snap_call("hello", "hi").unwrap(), Value::Unit);
    let err = snap_call("hello", "nope").unwrap_err();
    assert!(
        err.message.contains("Snapshot missing"),
        "got {}",
        err.message
    );
    snap_remove_var("ZZ_SNAPSHOTS_DIR");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn snapshot_mismatch_and_update_roundtrip() {
    let _guard = SNAPSHOT_ENV_LOCK.lock().unwrap();
    let dir = unique_snap_dir("mis");
    snap_set_var("ZZ_SNAPSHOTS_DIR", &dir);
    snap_remove_var("ZZ_UPDATE_SNAPSHOTS");
    std::fs::write(dir.join("m.snap"), "aaa\nbbb\n").unwrap();
    let err = snap_call("aaa\nCCC\n", "m").unwrap_err();
    assert!(
        err.message.contains("Snapshot mismatch"),
        "got {}",
        err.message
    );
    // Update mode writes, verify mode reads back.
    snap_set_str("ZZ_UPDATE_SNAPSHOTS", "1");
    assert_eq!(snap_call("fresh", "auto").unwrap(), Value::Unit);
    assert_eq!(std::fs::read(dir.join("auto.snap")).unwrap(), b"fresh");
    snap_remove_var("ZZ_UPDATE_SNAPSHOTS");
    assert_eq!(snap_call("fresh", "auto").unwrap(), Value::Unit);
    snap_remove_var("ZZ_SNAPSHOTS_DIR");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn snapshot_rejects_traversal_names() {
    let _guard = SNAPSHOT_ENV_LOCK.lock().unwrap();
    let dir = unique_snap_dir("trav");
    snap_set_var("ZZ_SNAPSHOTS_DIR", &dir);
    snap_remove_var("ZZ_UPDATE_SNAPSHOTS");
    for bad in ["", "../x", "a/b", "a\\b", "..", ".", "v1..v2"] {
        let err = snap_call("x", bad).unwrap_err();
        assert!(
            err.message.contains("without path separators"),
            "name {bad:?} got {}",
            err.message
        );
    }
    snap_remove_var("ZZ_SNAPSHOTS_DIR");
    std::fs::remove_dir_all(&dir).ok();
}
