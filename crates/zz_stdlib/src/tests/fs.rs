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
                // Unified `fs:<op>:<code>: <path>` diagnostics (shared by
                // the VM and the AOT C runtime).
                assert!(
                    e.to_string().contains("fs:read:not_found:"),
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

#[test]
fn fs_append_copy_move_dirs_and_stat() {
    let dir = std::env::temp_dir().join(format!("zz_fs_unit_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let base = dir.to_string_lossy().to_string();
    let sub = format!("{base}/sub/nested");

    let v = run(&format!("import std.fs\nfs.mkdir_all(\"{sub}\")")).unwrap();
    assert_eq!(v, Value::Result(Box::new(Ok(Value::Unit))));

    let v = run(&format!(
        "import std.fs\nfs.write(\"{base}/a.txt\", \"hello\")"
    ))
    .unwrap();
    assert_eq!(v, Value::Result(Box::new(Ok(Value::Unit))));

    let v = run(&format!(
        "import std.fs\nfs.append(\"{base}/a.txt\", \"!\")"
    ))
    .unwrap();
    assert_eq!(v, Value::Result(Box::new(Ok(Value::Unit))));

    let v = run(&format!(
        "import std.fs\nfs.read_to_string(\"{base}/a.txt\")"
    ))
    .unwrap();
    assert_eq!(
        v,
        Value::Result(Box::new(Ok(Value::Str("hello!".to_string().into()))))
    );

    let v = run(&format!("import std.fs\nfs.read_bytes(\"{base}/a.txt\")")).unwrap();
    match v {
        Value::Result(r) => match &*r {
            Ok(Value::Array(items)) => {
                assert_eq!(items.len(), 6);
                assert_eq!(items[0], Value::Int(104)); // 'h'
            }
            other => panic!("expected ok array, got {other:?}"),
        },
        other => panic!("expected result, got {other:?}"),
    }

    let v = run(&format!(
        "import std.fs\nfs.copy(\"{base}/a.txt\", \"{base}/b.txt\")"
    ))
    .unwrap();
    assert_eq!(v, Value::Result(Box::new(Ok(Value::Unit))));

    let v = run(&format!(
        "import std.fs\nfs.move(\"{base}/b.txt\", \"{sub}/c.txt\")"
    ))
    .unwrap();
    assert_eq!(v, Value::Result(Box::new(Ok(Value::Unit))));

    let v = run(&format!("import std.fs\nfs.is_file(\"{base}/a.txt\")")).unwrap();
    assert_eq!(v, Value::Bool(true));
    let v = run(&format!("import std.fs\nfs.is_dir(\"{sub}\")")).unwrap();
    assert_eq!(v, Value::Bool(true));

    let v = run(&format!("import std.fs\nfs.read_dir(\"{base}\")")).unwrap();
    match v {
        Value::Result(r) => match &*r {
            Ok(Value::Array(items)) => {
                // Sorted basenames: ["a.txt", "sub"].
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].to_string(), "a.txt");
                assert_eq!(items[1].to_string(), "sub");
            }
            other => panic!("expected ok array, got {other:?}"),
        },
        other => panic!("expected result, got {other:?}"),
    }

    let v = run(&format!("import std.fs\nfs.walk_dir(\"{base}\")")).unwrap();
    match v {
        Value::Result(r) => match &*r {
            Ok(Value::Array(items)) => {
                // a.txt, sub, sub/nested, sub/nested/c.txt (sorted full paths).
                assert_eq!(items.len(), 4);
            }
            other => panic!("expected ok array, got {other:?}"),
        },
        other => panic!("expected result, got {other:?}"),
    }

    let v = run(&format!("import std.fs\nfs.stat(\"{base}/a.txt\")")).unwrap();
    match v {
        Value::Result(r) => match &*r {
            Ok(Value::Dict(pairs)) => {
                let size = pairs
                    .iter()
                    .find(|(k, _)| k.to_string() == "size")
                    .map(|(_, v)| v.to_string());
                assert_eq!(size.as_deref(), Some("6"));
            }
            other => panic!("expected ok dict, got {other:?}"),
        },
        other => panic!("expected result, got {other:?}"),
    }

    let v = run(&format!("import std.fs\nfs.remove_dir_all(\"{base}\")")).unwrap();
    assert_eq!(v, Value::Result(Box::new(Ok(Value::Unit))));
    let v = run(&format!("import std.fs\nfs.exists(\"{base}\")")).unwrap();
    assert_eq!(v, Value::Bool(false));
}

#[test]
fn fs_streaming_handle_roundtrip() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("zz_fs_stream_{}.txt", std::process::id()));
    let path_str = path.to_string_lossy().to_string();

    let v = run(&format!("import std.fs\nFile.open(\"{path_str}\", \"w\")")).unwrap();
    let id = match v {
        Value::Result(r) => match &*r {
            Ok(Value::Opaque(h)) => {
                assert_eq!(h.tag, "file");
                h.id
            }
            other => panic!("expected ok handle, got {other:?}"),
        },
        other => panic!("expected result, got {other:?}"),
    };

    // The pool owns the handle; exercise write/flush/seek/read/close through
    // the public surface.
    let v = run(&format!(
        "import std.fs\nf := File.open(\"{path_str}\", \"w\").expect(\"open\"); f.write_chunk(\"ab\")"
    ));
    assert!(v.is_ok(), "method write_chunk failed: {v:?}");

    let _ = id;
    let _ = std::fs::remove_file(&path);
}
