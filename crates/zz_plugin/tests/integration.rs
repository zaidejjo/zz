use std::collections::HashMap;
use std::io::Write;
use tempfile::NamedTempFile;

fn write_manifest(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f.flush().unwrap();
    f
}

/// Test that manifest loading produces correct FuncSig entries
/// and that those entries work with the ZZ type checker.
#[test]
fn test_manifest_func_sigs_work_with_checker() {
    let f = write_manifest(
        r#"// Version: 1
// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func add(a: int, b: int) -> int
    func scale(val: float, factor: float) -> float
    func noop()
}
"#,
    );

    let manifest = zz_plugin::load_manifest(f.path()).unwrap();
    assert_eq!(manifest.funcs.len(), 3);

    let add = manifest.funcs.get("add").unwrap();
    assert_eq!(add.params.len(), 2);
    assert!(matches!(add.params[0].1, zz_checker::Type::Int));
    assert!(matches!(add.params[1].1, zz_checker::Type::Int));
    assert!(matches!(add.ret, zz_checker::Type::Int));
    assert!(add.is_extern);

    let scale = manifest.funcs.get("scale").unwrap();
    assert!(matches!(scale.params[0].1, zz_checker::Type::Float));
    assert!(matches!(scale.ret, zz_checker::Type::Float));

    let noop = manifest.funcs.get("noop").unwrap();
    assert!(noop.params.is_empty());
    assert!(matches!(noop.ret, zz_checker::Type::Void));

    // Merge into initial_funcs and verify checker accepts them
    let mut initial_funcs = zz_stdlib::stdlib_funcs();
    for (name, sig) in &manifest.funcs {
        initial_funcs.insert(name.clone(), sig.clone());
    }

    let src = "func main() { add(1, 2); scale(3.14, 2.0); noop(); }";
    let parsed = zz_frontend::parser::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );

    let checked = zz_checker::check_program(
        &parsed.program,
        HashMap::new(),
        initial_funcs,
        HashMap::new(),
    );

    let has_errors = checked
        .errors
        .iter()
        .any(|e| e.severity == zz_frontend::diag::Severity::Error);
    assert!(
        !has_errors,
        "type-check errors: {:?}",
        checked
            .errors
            .iter()
            .map(|e| &e.message)
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_manifest_rejects_non_c_types() {
    let f = write_manifest(
        r#"// Version: 1
// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func bad(items: [int]) -> int
}
"#,
    );

    let err = zz_plugin::load_manifest(f.path()).unwrap_err();
    assert!(
        err.to_string().contains("non-C"),
        "expected non-C error, got: {err}"
    );
}

#[test]
fn test_manifest_metadata_validation() {
    let f = write_manifest(
        r#"// Version: 1

extern "C" {
    func add(a: int, b: int) -> int
}
"#,
    );

    let err = zz_plugin::load_manifest(f.path()).unwrap_err();
    assert!(
        err.to_string().contains("Rustc"),
        "expected missing Rustc error, got: {err}"
    );
}

#[test]
fn test_manifest_pointer_types() {
    let f = write_manifest(
        r#"// Version: 1
// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func load(ptr: *const void) -> *mut void
    func process(ptr: *mut void, size: int) -> int
}
"#,
    );

    let manifest = zz_plugin::load_manifest(f.path()).unwrap();
    assert_eq!(manifest.funcs.len(), 2);

    let load = manifest.funcs.get("load").unwrap();
    assert!(matches!(
        load.params[0].1,
        zz_checker::Type::Ptr { mutable: false, .. }
    ));
    assert!(matches!(
        load.ret,
        zz_checker::Type::Ptr { mutable: true, .. }
    ));
}

#[test]
fn test_manifest_multiple_functions() {
    let f = write_manifest(
        r#"// Version: 1
// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func f1() -> int
    func f2(a: int) -> float
    func f3(a: int, b: float, c: bool) -> void
    func f4(ptr: *mut void, size: int) -> *const void
}
"#,
    );

    let manifest = zz_plugin::load_manifest(f.path()).unwrap();
    assert_eq!(manifest.funcs.len(), 4);
    assert!(manifest.funcs.contains_key("f1"));
    assert!(manifest.funcs.contains_key("f2"));
    assert!(manifest.funcs.contains_key("f3"));
    assert!(manifest.funcs.contains_key("f4"));
}

#[test]
fn test_manifest_dotted_names_with_override() {
    let f = write_manifest(
        r#"// Version: 1
// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func toy.add(a: int, b: int) -> int = "toy_add_impl";
    func toy.starts_with(s: str, prefix: str) -> int
    func toy.noop()
}
"#,
    );

    let manifest = zz_plugin::load_manifest(f.path()).unwrap();
    assert_eq!(manifest.funcs.len(), 3);

    // Keyed by ZZ-visible dotted name.
    let add = manifest.funcs.get("toy.add").unwrap();
    assert!(add.is_extern);
    assert_eq!(add.extern_c_symbol.as_deref(), Some("toy_add_impl"));
    assert_eq!(add.c_symbol("toy.add"), "toy_add_impl");

    // No override: C symbol derives from dots-to-underscores.
    let sw = manifest.funcs.get("toy.starts_with").unwrap();
    assert_eq!(sw.extern_c_symbol, None);
    assert_eq!(sw.c_symbol("toy.starts_with"), "toy_starts_with");
    assert!(matches!(sw.params[0].1, zz_checker::Type::Str));

    let noop = manifest.funcs.get("toy.noop").unwrap();
    assert!(matches!(noop.ret, zz_checker::Type::Void));
}

#[test]
fn test_manifest_str_return_rejected_with_guidance() {
    let f = write_manifest(
        r#"// Version: 1
// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func toy.greet(name: str) -> str
}
"#,
    );

    let err = zz_plugin::load_manifest(f.path()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("returns str") && msg.contains("getter pattern"),
        "expected str-return guidance, got: {msg}"
    );
}
