use super::run;
use zz_runtime::Value;

#[test]
fn fs_write_and_read_file() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("zz_stdlib_test_{}.txt", std::process::id()));
    let path_str = path.to_string_lossy().to_string();

    let v = run(&format!(
        "import std.fs\nfs.write_file(\"{path_str}\", \"hello fs\")"
    ))
    .unwrap();
    assert_eq!(v, Value::Result(Box::new(Ok(Value::Unit))));

    let v = run(&format!("import std.fs\nfs.read_file(\"{path_str}\")")).unwrap();
    assert_eq!(
        v,
        Value::Result(Box::new(Ok(Value::Str("hello fs".to_string().into()))))
    );

    let v = run(&format!("import std.fs\nfs.exists(\"{path_str}\")")).unwrap();
    assert_eq!(v, Value::Bool(true));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn fs_read_missing_file_is_err() {
    let v = run("import std.fs\nfs.read_file(\"/tmp/zz_no_such_file_zz\")").unwrap();
    match v {
        Value::Result(r) => match &*r {
            Err(e) => {
                assert!(
                    e.to_string().contains("No such file"),
                    "unexpected error: {e}"
                );
            }
            Ok(_) => panic!("expected err result, got ok"),
        },
        other => panic!("expected err result, got {other}"),
    }
}

#[test]
fn fs_exists_missing_is_false() {
    let v = run("import std.fs\nfs.exists(\"/tmp/zz_no_such_file_zz\")").unwrap();
    assert_eq!(v, Value::Bool(false));
}
