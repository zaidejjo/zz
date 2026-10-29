use super::*;
use std::fs;

/// Create a unique temp dir with the given (relative path → contents).
///
/// The pid matters: libtest runs these in one process (counter separates
/// them), but `cargo nextest` gives every test its own process, so the
/// counter alone restarts at 0 in each worker and two tests would share
/// `/tmp/zz_loader_test_0` — one wiping the other's project mid-run.
fn temp_project(files: &[(&str, &str)]) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("zz_loader_test_{}_{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for (rel, contents) in files {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, contents).unwrap();
    }
    dir
}

/// Check that there are no hard errors (warnings are OK).
fn no_errors(result: &LoadResult) -> bool {
    result.errors.iter().all(|e| {
        e.diags
            .iter()
            .all(|d| d.severity != zz_frontend::diag::Severity::Error)
    })
}

/// Flatten all diagnostic messages + notes (used to assert on both a message
/// and its `add pub` hint note).
fn diag_texts(result: &LoadResult) -> Vec<String> {
    result
        .errors
        .iter()
        .flat_map(|e| {
            e.diags.iter().flat_map(|d| {
                let mut parts = vec![d.message.clone()];
                parts.extend(d.notes.clone());
                parts
            })
        })
        .collect()
}

#[test]
fn feature_gated_module_import_names_the_feature() {
    let dir = temp_project(&[
        (
            "zz.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [features]\nlocales-ar = { modules = [\"locales/ar\"] }\n",
        ),
        ("src/main.zz", "import locales.ar\nx := ar.hello()"),
        (
            "src/locales/ar.zz",
            "pub func hello() -> str { return \"hi\" }",
        ),
    ]);
    let entry = dir.join("src/main.zz");
    let disabled = load_program_with_features(&entry, &FeatureSelection::default()).unwrap();
    assert!(
        !no_errors(&disabled),
        "gated module must not load by default"
    );
    let text = diag_texts(&disabled).join("\n");
    assert!(
        text.contains("module `locales/ar` is provided by feature `locales-ar`"),
        "needs exact module + feature, got:\n{text}"
    );
    assert!(
        text.contains("--features locales-ar"),
        "needs the enabling flag hint, got:\n{text}"
    );

    let enabled =
        load_program_with_features(&entry, &FeatureSelection::new(["locales-ar"], false)).unwrap();
    assert!(no_errors(&enabled), "errors: {:?}", enabled.errors);
    assert_eq!(enabled.programs.len(), 2);
    assert_eq!(enabled.bindings["main.x"], Type::Str);
}

#[test]
fn default_feature_enables_gated_module_without_flags() {
    let dir = temp_project(&[
        (
            "zz.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [features]\ndefault = [\"locales-ar\"]\n\
             locales-ar = { modules = [\"locales/ar\"] }\n",
        ),
        ("src/main.zz", "import locales.ar\nx := ar.hello()"),
        (
            "src/locales/ar.zz",
            "pub func hello() -> str { return \"hi\" }",
        ),
    ]);
    let result =
        load_program_with_features(&dir.join("src/main.zz"), &FeatureSelection::default()).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.x"], Type::Str);
}

#[test]
fn all_features_enables_gated_module() {
    let dir = temp_project(&[
        (
            "zz.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [features]\nlocales-ar = { modules = [\"locales/ar\"] }\n",
        ),
        ("src/main.zz", "import locales.ar\nx := ar.hello()"),
        (
            "src/locales/ar.zz",
            "pub func hello() -> str { return \"hi\" }",
        ),
    ]);
    let result = load_program_with_features(
        &dir.join("src/main.zz"),
        &FeatureSelection::new(Vec::<String>::new(), true),
    )
    .unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
}

#[test]
fn gated_optional_dependency_import_names_the_feature() {
    let dir = temp_project(&[
        (
            "zz.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\nextra = { version = \"1.0\", optional = true }\n\n\
             [features]\nextras = { deps = [\"extra\"] }\n",
        ),
        ("src/main.zz", "import extra\nx := 1"),
    ]);
    let result =
        load_program_with_features(&dir.join("src/main.zz"), &FeatureSelection::default()).unwrap();
    let text = diag_texts(&result).join("\n");
    assert!(
        text.contains("`extra`") && text.contains("provided by feature `extras`"),
        "needs feature-gated dep diagnostic, got:\n{text}"
    );
    assert!(
        text.contains("--features extras"),
        "needs the enabling flag hint, got:\n{text}"
    );
    assert!(
        !text.contains("not installed"),
        "must not suggest install for a disabled optional dep, got:\n{text}"
    );
}

#[test]
fn loads_relative_import() {
    let dir = temp_project(&[
        ("main.zz", "import math.utils\nx := utils.double(21)"),
        (
            "math/utils.zz",
            "pub func double(n: int) -> int { return n * 2 }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.programs.len(), 2);
    assert_eq!(result.bindings["main.x"], Type::Int);
}

#[test]
fn impl_method_keeps_its_key_when_a_free_function_shares_its_name() {
    // #500: the top-level rename pass used to prefix the *method* name when
    // a top-level function shared it, so `impl W { func get }` next to
    // `func get` registered `main.W.main.get` instead of `main.W.get`. The
    // method was then unreachable in every engine and the same-named free
    // function silently won the dot call.
    let dir = temp_project(&[(
        "main.zz",
        "struct W { v: int }\n\
         func get(w: W) -> int { return w.v }\n\
         impl W {\n\
             func get(self) -> str { return \"method\" }\n\
         }\n\
         w := W{ v: 1 }\n\
         r := w.get()\n\
         s := get(w)\n",
    )]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert!(
        result.funcs.contains_key("main.W.get"),
        "impl method not registered under `main.W.get`: {:?}",
        result.funcs.keys().collect::<Vec<_>>()
    );
    assert!(
        result.funcs.contains_key("main.get"),
        "free function not registered under `main.get`: {:?}",
        result.funcs.keys().collect::<Vec<_>>()
    );
    // The dot call binds the receiver's own `impl` method; the free
    // function stays reachable by name.
    assert_eq!(
        result.bindings["main.r"],
        Type::Str,
        "dot call must reach the method"
    );
    assert_eq!(
        result.bindings["main.s"],
        Type::Int,
        "by-name call must reach the free function"
    );
}

#[test]
fn imported_bindings_visible() {
    let dir = temp_project(&[
        ("main.zz", "import config\nx := config.base + 1"),
        ("config.zz", "pub base := 41"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.x"], Type::Int);
}

#[test]
fn import_alias_works() {
    let dir = temp_project(&[
        ("main.zz", "import math.utils as m\nx := m.double(21)"),
        (
            "math/utils.zz",
            "pub func double(n: int) -> int { return n * 2 }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.x"], Type::Int);
}

#[test]
fn module_internal_calls_rewritten() {
    // `double` calls `twice` internally; both are top-level in utils.zz.
    let dir = temp_project(&[
        ("main.zz", "import math.utils\nx := utils.double(21)"),
        (
            "math/utils.zz",
            "func twice(n: int) -> int { return n * 2 }\npub func double(n: int) -> int { return twice(n) }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.x"], Type::Int);
}

#[test]
fn shadowing_respected() {
    // Local `n` shadows the top-level `n` inside the func body.
    let dir = temp_project(&[
        ("main.zz", "import math.utils\nx := utils.double(21)"),
        (
            "math/utils.zz",
            "n := 100\npub func double(n: int) -> int { return n * 2 }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.x"], Type::Int);
}

#[test]
fn namespace_collision_detected() {
    // Two modules with the same stem claim the same namespace.
    let dir = temp_project(&[
        ("main.zz", "import a.utils\nimport b.utils\n1"),
        ("a/utils.zz", "func f() -> int { return 1 }"),
        ("b/utils.zz", "func g() -> int { return 2 }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(
        result.errors.iter().any(|e| e
            .diags
            .iter()
            .any(|d| d.message.contains("claimed by both"))),
        "expected namespace collision error, got: {:?}",
        result.errors
    );
}

#[test]
fn same_file_two_namespaces_compose_via_alias() {
    // #380: one file imported bare and aliased composes instead of
    // erroring. The first binding wins as canonical; alias references
    // rewrite through it so seeds keep one identity.
    let dir = temp_project(&[
        (
            "main.zz",
            "import utils\nimport utils as u\nx := utils.f()\ny := u.f()",
        ),
        ("utils.zz", "pub func f() -> int { return 1 }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.x"], Type::Int);
    assert_eq!(result.bindings["main.y"], Type::Int);
    assert!(
        result.funcs.contains_key("utils.f"),
        "canonical seed entry missing: {:?}",
        result.funcs.keys().collect::<Vec<_>>()
    );
}

#[test]
fn same_file_alias_before_bare_composes() {
    // Order-independence: aliased-first also binds once and rewrites.
    let dir = temp_project(&[
        (
            "main.zz",
            "import utils as u\nimport utils\nx := u.f()\ny := utils.f()",
        ),
        ("utils.zz", "pub func f() -> int { return 1 }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.x"], Type::Int);
    assert_eq!(result.bindings["main.y"], Type::Int);
}

#[test]
fn dep_submodules_share_stem_across_packages() {
    // #380: ask.keys + tui.keys coexist via aliases while tui.menu's
    // internal bare `import tui.keys` keeps working. Path deps stand in
    // for registry deps (same resolver path).
    let dir = temp_project(&[
        (
            "app/zz.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nask = { path = \"../askpkg\" }\ntui = { path = \"../tuipkg\" }\n",
        ),
        (
            "app/src/main.zz",
            "import ask.keys as akeys\nimport tui.keys as tkeys\nimport tui.menu\nimport tui.keys(up)\nx := akeys.up()\ny := tkeys.up()\nz := menu.menu_up()\nw := up()",
        ),
        ("askpkg/zz.toml", "[package]\nname = \"ask\"\nversion = \"0.1.0\"\n"),
        (
            "askpkg/src/keys.zz",
            "pub func up() -> int { return 1 }",
        ),
        ("tuipkg/zz.toml", "[package]\nname = \"tui\"\nversion = \"0.1.0\"\n"),
        ("tuipkg/src/keys.zz", "pub func up() -> int { return 2 }"),
        (
            "tuipkg/src/menu.zz",
            "import tui.keys\npub func menu_up() -> int { return keys.up() }",
        ),
    ]);
    // Minimal path-dep lockfile (checksum via save, like `zz install`).
    let mut lock = zz_pm::lock::Lockfile::new();
    for dep in ["ask", "tui"] {
        lock.deps.push(zz_pm::lock::LockedDep {
            name: dep.to_string(),
            version: "*".to_string(),
            source: "path".to_string(),
            hash: String::new(),
            commit: None,
            native: None,
        });
    }
    lock.save(&dir.join("app/zz.lock")).expect("write lock");
    let result = load_program(&dir.join("app/src/main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.x"], Type::Int);
    assert_eq!(result.bindings["main.y"], Type::Int);
    assert_eq!(result.bindings["main.z"], Type::Int);
    assert_eq!(result.bindings["main.w"], Type::Int);
    for want in ["akeys.up", "tkeys.up", "menu.menu_up"] {
        assert!(
            result.funcs.contains_key(want),
            "missing seed entry `{want}`: {:?}",
            result.funcs.keys().collect::<Vec<_>>()
        );
    }
    // Alias-rewritten references must not orphan their import (#380):
    // no false `unused import` for the multi-bound spellings.
    assert!(
        !diag_texts(&result)
            .iter()
            .any(|m| m.contains("unused import")),
        "alias imports must not warn: {:?}",
        diag_texts(&result)
    );
}

#[test]
fn circular_import_detected() {
    let dir = temp_project(&[("a.zz", "import b"), ("b.zz", "import a")]);
    let result = load_program(&dir.join("a.zz")).unwrap();
    assert!(
        result.errors.iter().any(|e| e
            .diags
            .iter()
            .any(|d| d.message.contains("circular import"))),
        "expected circular import error, got: {:?}",
        result.errors
    );
}

#[test]
fn transitive_cycle_detected() {
    let dir = temp_project(&[
        ("a.zz", "import b"),
        ("b.zz", "import c"),
        ("c.zz", "import a"),
    ]);
    let result = load_program(&dir.join("a.zz")).unwrap();
    assert!(
        result.errors.iter().any(|e| e
            .diags
            .iter()
            .any(|d| d.message.contains("circular import"))),
        "expected circular import error, got: {:?}",
        result.errors
    );
}

#[test]
fn diamond_import_is_fine() {
    // a imports b and c; both import d. No cycle.
    let dir = temp_project(&[
        ("a.zz", "import b\nimport c"),
        ("b.zz", "import d"),
        ("c.zz", "import d"),
        ("d.zz", "func f() -> int { return 1 }"),
    ]);
    let result = load_program(&dir.join("a.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    // d loaded once, before b and c.
    assert_eq!(result.programs.len(), 4);
}

#[test]
fn missing_module_errors() {
    let dir = temp_project(&[("main.zz", "import nope.missing\n1")]);
    let result = load_program(&dir.join("main.zz")).unwrap_err();
    assert!(result.contains("cannot read"), "{result}");
}

#[test]
fn unknown_stdlib_module_errors() {
    let dir = temp_project(&[("main.zz", "import std.nope\n1")]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(
        result.errors.iter().any(|e| e
            .diags
            .iter()
            .any(|d| d.message.contains("unknown standard library module"))),
        "expected unknown stdlib error, got: {:?}",
        result.errors
    );
}

#[test]
fn parse_error_in_module_reported() {
    let dir = temp_project(&[("main.zz", "import broken\n1"), ("broken.zz", "x :=")]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(
        result.errors.iter().any(|e| e.name.contains("broken.zz")),
        "expected error for broken.zz, got: {:?}",
        result.errors
    );
}

#[test]
fn type_error_in_module_reported() {
    let dir = temp_project(&[
        ("main.zz", "import broken\n1"),
        ("broken.zz", "x := 1 + \"a\""),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.diags.iter().any(|d| d.message.contains("cannot apply"))),
        "expected type error, got: {:?}",
        result.errors
    );
}

#[test]
fn full_program_runs_with_imports() {
    use zz_runtime::{Interp, Value};

    let dir = temp_project(&[
        (
            "main.zz",
            "import std.str\nimport math.utils\nn := utils.double(6)\nstr.length(\"abc\")",
        ),
        (
            "math/utils.zz",
            "pub func double(n: int) -> int { return n * 2 }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(3));
    // The imported function ran: main's `n` binding must hold.
    assert_eq!(result.bindings["main.n"], zz_checker::Type::Int);
}

#[test]
fn selective_struct_import_resolves_bare() {
    use zz_runtime::{Interp, Value};

    // `import models.product(Product)` must bind bare `Product`: structs
    // are types, not values, so no synthetic `Product := product.Product`
    // Decl is emitted (that forced a value lookup of a type name:
    // "undefined variable `product.Product`"). The bare name resolves
    // through the structs seed (checker) with an import-alias fallback
    // at runtime (VM + tree-walker).
    let dir = temp_project(&[
        (
            "main.zz",
            "import models.product(Product)\np := Product{id: 7}\np.id + p.get_id()",
        ),
        (
            "models/product.zz",
            "pub struct Product{\n    id: int,\n}\nimpl Product {\n    pub func get_id(self) -> int {\n        return self.id\n    }\n}",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert!(result.structs.contains_key("Product"));
    assert!(result.structs.contains_key("product.Product"));

    let mut interp = Interp::with_natives(result.natives.clone());
    // Wire selective-import aliases exactly like `zz run` does: the VM
    // compiler emits no code for imports, so bare names only resolve
    // through this map at runtime.
    interp.import_aliases = result.import_aliases.clone();
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(14));
}

#[test]
fn stdlib_import_needs_no_file() {
    let dir = temp_project(&[("main.zz", "import std.str\n1")]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.programs.len(), 1);
    assert!(result.funcs.contains_key("std.str.length"));
    // Namespaced copies are registered too.
    assert!(result.funcs.contains_key("str.length"));
    assert!(result.natives.contains_key("str.length"));
}

#[test]
fn struct_in_module_namespaced() {
    let dir = temp_project(&[
        (
            "main.zz",
            "import shapes\np := shapes.Point{ x: 1, y: 2 }\nz := shapes.dist(p)",
        ),
        (
            "shapes.zz",
            "pub struct Point { x: int, y: int }\npub func dist(p: Point) -> int { return p.x + p.y }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);
}

// ===========================================================================
// Phase 2 — Comprehensive visibility & module edge-case tests
// ===========================================================================

#[test]
fn multi_level_pub_chain_abc() {
    // A imports B (pub items), B imports C (pub items).
    // A can see C's pub items through B's namespace.
    let dir = temp_project(&[
        ("main.zz", "import a\nz := a.b.c_val + a.b.c_func(3)"),
        ("a.zz", "pub import b"),
        (
            "b.zz",
            "pub c_val := 10\npub func c_func(x: int) -> int { return x + 1 }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);
}

#[test]
fn multi_level_pub_chain_runtime() {
    // Full runtime test: multi-level import chain.
    // A imports B's pub func directly (not via re-export).
    use zz_runtime::{Interp, Value};

    let dir = temp_project(&[
        ("main.zz", "import a\nimport b\nz := b.add(3, 4)"),
        ("a.zz", "pub import b"),
        (
            "b.zz",
            "pub func add(a: int, b: int) -> int { return a + b }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(7));
}

#[test]
fn private_item_not_visible_cross_module() {
    // Private binding in module B is NOT accessible from A. The diagnostic
    // must say the item is private and tell the user to add `pub`.
    let dir = temp_project(&[
        ("main.zz", "import b\nz := b.secret"),
        ("b.zz", "secret := 42"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    let msgs = diag_texts(&result);
    assert!(
        msgs.iter()
            .any(|m| m.contains("variable `b.secret` is private")),
        "expected private-variable error, got: {msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.contains("add `pub` to `secret`")),
        "expected `add pub` hint, got: {msgs:?}"
    );
}

#[test]
fn private_func_not_visible_cross_module() {
    // Private function in module B is NOT accessible from A. The diagnostic
    // must say the function is private and tell the user to add `pub`.
    let dir = temp_project(&[
        ("main.zz", "import b\nz := b.helper(1)"),
        ("b.zz", "func helper(x: int) -> int { return x }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    let msgs = diag_texts(&result);
    assert!(
        msgs.iter()
            .any(|m| m.contains("function `b.helper` is private")),
        "expected private-function error, got: {msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.contains("add `pub` to `helper`")),
        "expected `add pub` hint, got: {msgs:?}"
    );
}

#[test]
fn pub_struct_accessible_cross_module() {
    // pub struct from imported module can be instantiated.
    let dir = temp_project(&[
        (
            "main.zz",
            "import shapes\np := shapes.Point { x: 1, y: 2 }\nz := p.x + p.y",
        ),
        ("shapes.zz", "pub struct Point { x: int, y: int }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);
}

#[test]
fn private_struct_not_instantiateable() {
    // Private struct from imported module cannot be instantiated. The
    // diagnostic must say the struct is private and tell the user to add `pub`.
    let dir = temp_project(&[
        ("main.zz", "import shapes\np := shapes.Secret { x: 1 }"),
        ("shapes.zz", "struct Secret { x: int }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    let msgs = diag_texts(&result);
    assert!(
        msgs.iter()
            .any(|m| m.contains("struct `shapes.Secret` is private")),
        "expected private-struct error, got: {msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.contains("add `pub` to `Secret`")),
        "expected `add pub` hint, got: {msgs:?}"
    );
}

#[test]
fn shadowing_imported_pub_var() {
    // Local variable shadows an imported pub variable.
    use zz_runtime::{Interp, Value};

    let dir = temp_project(&[
        (
            "main.zz",
            "import config\nconfig_val := config.val\nx := { val := 99\nval }\nprintln(x)\nconfig_val",
        ),
        (
            "config.zz",
            "pub val := 42",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    // Last expression is config_val (42).
    assert_eq!(last, Value::Int(42));
}

#[test]
fn pub_import_reexport_chain() {
    // A re-exports B — type checker accepts a.B.func through re-export.
    // (Runtime doesn't resolve re-exported dotted paths yet; this tests
    // the checker-level propagation only.)
    let dir = temp_project(&[
        ("main.zz", "import a\nz := a.math.double(21)"),
        ("a.zz", "pub import math"),
        ("math.zz", "pub func double(n: int) -> int { return n * 2 }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);
}

#[test]
fn pub_reexport_binding_accessible() {
    // Re-exported pub binding is accessible from importing module.
    let dir = temp_project(&[
        ("main.zz", "import lib\nz := lib.math.PI"),
        ("lib.zz", "pub import math"),
        ("math.zz", "pub PI := 314"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);
}

#[test]
fn private_not_visible_through_reexport() {
    // Private items from a re-exported module stay invisible.
    let dir = temp_project(&[
        ("main.zz", "import lib\nz := lib.math.secret"),
        ("lib.zz", "pub import math"),
        ("math.zz", "secret := 99"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(
        result.errors.iter().any(|e| e
            .diags
            .iter()
            .any(|d| d.severity == zz_frontend::diag::Severity::Error)),
        "expected error for private through re-export, got: {:?}",
        result.errors
    );
}

#[test]
fn same_module_imported_from_multiple_parents() {
    // Module D is imported (pub) by both B and C; main imports B and C.
    // D's pub items are visible via b.d.{item} and c.d.{item}.
    let dir = temp_project(&[
        ("main.zz", "import b\nimport c\nz := b.d.d_val + c.d.d_val"),
        ("b.zz", "pub import d"),
        ("c.zz", "pub import d"),
        ("d.zz", "pub d_val := 5"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);
}

#[test]
fn diamond_import_no_errors() {
    // A → B, A → C, B → D, C → D. D loaded once, no collision.
    let dir = temp_project(&[
        ("main.zz", "import b\nimport c\n1"),
        ("b.zz", "import d"),
        ("c.zz", "import d"),
        ("d.zz", "pub val := 1"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
}

#[test]
fn pub_struct_method_cross_module() {
    // Struct + method defined in one module, called from another.
    use zz_runtime::{Interp, Value};

    let dir = temp_project(&[
        (
            "main.zz",
            "import shapes\np := shapes.Point { x: 3, y: 4 }\nz := shapes.dist(p)",
        ),
        (
            "shapes.zz",
            "pub struct Point { x: int, y: int }\npub func dist(p: Point) -> int { return p.x + p.y }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(7));
}

#[test]
fn pub_binding_type_preserved_cross_module() {
    // Type of pub binding is correctly preserved when imported.
    let dir = temp_project(&[
        (
            "main.zz",
            "import types\ns := types.greeting\nn := types.count",
        ),
        ("types.zz", "pub greeting := \"hello\"\npub count := 42"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.s"], Type::Str);
    assert_eq!(result.bindings["main.n"], Type::Int);
}

#[test]
fn mixed_pub_private_items() {
    // Module has both pub and private items; only pub visible cross-module.
    let dir = temp_project(&[
        ("main.zz", "import mod\nz := mod.pub_func()"),
        (
            "mod.zz",
            "pub func pub_func() -> int { return priv_func() }\nfunc priv_func() -> int { return 42 }",
        ),
    ]);
    // This should work: pub_func calls priv_func internally (same module).
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);
}

#[test]
fn func_matching_file_stem_still_namespaced() {
    // `func main` in `main.zz` must rewrite to `main.main` (not stay
    // bare `main`): the `zz run` auto-call looks up `<stem>.main`.
    // Same for a struct whose name equals the file stem.
    let dir = temp_project(&[(
        "main.zz",
        "struct Main { v: int }\nfunc main() { 1 }\nfunc helper() -> int { return 2 }",
    )]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    let entry = result.programs.last().unwrap();
    let mut saw_main_fn = false;
    let mut saw_helper_fn = false;
    let mut saw_struct = false;
    for stmt in &entry.stmts {
        if let zz_frontend::ast::Stmt::Func { name, .. } = stmt {
            if name.join(".") == "main.main" {
                saw_main_fn = true;
            }
            if name.join(".") == "main.helper" {
                saw_helper_fn = true;
            }
        }
        if let zz_frontend::ast::Stmt::Struct { name, .. } = stmt {
            if name.join(".") == "main.Main" {
                saw_struct = true;
            }
        }
    }
    assert!(saw_main_fn, "func main should rewrite to main.main");
    assert!(saw_helper_fn, "func helper should rewrite to main.helper");
    assert!(saw_struct, "struct Main should rewrite to main.Main");
}

#[test]
fn pub_item_unused_no_warning() {
    // pub items must NOT generate unused-variable warnings.
    let dir = temp_project(&[(
        "main.zz",
        "pub x := 42\npub func f() -> int { return 1 }\npub struct S { v: int }\nfunc main() { println(1) }",
    )]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    // No errors, and specifically no "unused variable" warnings for x, f, S.
    let has_unused = result.errors.iter().any(|e| {
        e.diags
            .iter()
            .any(|d| d.message.contains("unused variable"))
    });
    assert!(
        !has_unused,
        "pub items should not warn: {:?}",
        result.errors
    );
}

#[test]
fn private_item_unused_does_warn() {
    // Private items that are unused DO generate warnings.
    let dir = temp_project(&[("main.zz", "secret := 42\nfunc main() { println(1) }")]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    let has_unused = result.errors.iter().any(|e| {
        e.diags
            .iter()
            .any(|d| d.message.contains("unused variable `secret`"))
    });
    assert!(has_unused, "private unused item should warn");
}

#[test]
fn import_alias_pub_items_accessible() {
    // import ... as alias makes pub items accessible via alias.
    let dir = temp_project(&[
        (
            "main.zz",
            "import shapes as s\np := s.Point { x: 1, y: 2 }\nz := s.dist(p)",
        ),
        (
            "shapes.zz",
            "pub struct Point { x: int, y: int }\npub func dist(p: Point) -> int { return p.x + p.y }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);
}

#[test]
fn import_alias_private_inaccessible() {
    // import ... as alias does NOT expose private items.
    let dir = temp_project(&[
        ("main.zz", "import shapes as s\nz := s.hidden()"),
        ("shapes.zz", "func hidden() -> int { return 42 }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(
        result.errors.iter().any(|e| e
            .diags
            .iter()
            .any(|d| d.severity == zz_frontend::diag::Severity::Error)),
        "expected error for private item via alias, got: {:?}",
        result.errors
    );
}

#[test]
fn pub_reexport_alias_with_struct() {
    // pub import shapes as s re-exports the shapes namespace.
    let dir = temp_project(&[
        ("main.zz", "import lib\nz := lib.math.double(10)"),
        ("lib.zz", "pub import math as math"),
        ("math.zz", "pub func double(n: int) -> int { return n * 2 }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);
}

#[test]
fn three_level_reexport_chain() {
    // A re-exports B, B re-exports C. Can main access C through A?
    let dir = temp_project(&[
        ("main.zz", "import a\nz := a.b.c_val"),
        ("a.zz", "pub import b"),
        ("b.zz", "pub import c"),
        ("c.zz", "pub c_val := 77"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    // This tests transitive re-export. May or may not work depending on
    // implementation depth. Check result.
    if no_errors(&result) {
        assert_eq!(result.bindings["main.z"], Type::Int);
    }
    // At minimum, should not panic.
}

#[test]
fn pub_func_private_binding_interaction() {
    // A pub function uses a private binding internally. Both should work.
    use zz_runtime::{Interp, Value};

    let dir = temp_project(&[
        ("main.zz", "import calc\nz := calc.compute(5)"),
        (
            "calc.zz",
            "multiplier := 3\npub func compute(x: int) -> int { return x * multiplier }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(15));
}

#[test]
fn entry_file_private_items_accessible_to_self() {
    // Private items in the entry file are accessible within that file.
    use zz_runtime::{Interp, Value};

    let dir = temp_project(&[(
        "main.zz",
        "secret := 42\nfunc helper(x: int) -> int { return x + secret }\nresult := helper(8)",
    )]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(50));
}

#[test]
fn pub_struct_field_access_cross_module() {
    // pub struct fields are accessible from importing module.
    use zz_runtime::{Interp, Value};

    let dir = temp_project(&[
        (
            "main.zz",
            "import geo\np := geo.Point { x: 10, y: 20 }\nz := geo.manhattan(p)",
        ),
        (
            "geo.zz",
            "pub struct Point { x: int, y: int }\npub func manhattan(p: Point) -> int { return p.x + p.y }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(30));
}

#[test]
fn struct_field_access_in_module() {
    use zz_runtime::{Interp, Value};

    // `p.x` inside the module must resolve through the namespaced
    // binding `shapes.p`.
    let dir = temp_project(&[
        ("main.zz", "import shapes\nz := shapes.get_x()"),
        (
            "shapes.zz",
            "pub struct Point { x: int, y: int }\npub p := Point{ x: 42, y: 0 }\npub func get_x() -> int { return p.x }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(42));
}

#[test]
fn indexing_and_slicing_in_module() {
    use zz_runtime::{Interp, Value};

    // Index/slice expressions inside a module must be rewritten too
    // (they reference a namespaced binding as their object).
    let dir = temp_project(&[
        ("main.zz", "import stats\nz := stats.first()\nw := stats.mid()"),
        (
            "stats.zz",
            "pub scores := [10, 20, 30]\npub func first() -> int { return scores[0] }\npub func mid() -> [int] { return scores[1:3] }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);
    assert_eq!(result.bindings["main.w"], Type::Array(Box::new(Type::Int)));

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(
        last,
        Value::Array(std::sync::Arc::new(vec![Value::Int(20), Value::Int(30)]))
    );
}

#[test]
fn pipeline_in_module() {
    use zz_runtime::{Interp, Value};

    // Piped calls in a module rewrite the callee path correctly.
    let dir = temp_project(&[
        ("main.zz", "import math\nz := math.apply()"),
        (
            "math.zz",
            "func inc(n: int) -> int { return n + 1 }\npub func apply() -> int { return 5 |> inc }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(6));
}

#[test]
fn method_call_in_module() {
    use zz_runtime::{Interp, Value};

    // `p.dist()` inside the defining module: the method name is
    // qualified to `shapes.dist`.
    let dir = temp_project(&[
        ("main.zz", "import shapes\nz := shapes.apply()"),
        (
            "shapes.zz",
            "pub struct Point { x: int, y: int }\npub func dist(p: Point) -> int { return p.x + p.y }\npub func apply() -> int { p := Point { x: 3, y: 4 }\nreturn p.dist() }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(7));
}

#[test]
fn method_call_cross_module() {
    use zz_runtime::{Interp, Value};

    // `p.dist()` from another module: the method resolves through the
    // receiver's struct type (`shapes.Point` → `shapes.dist`).
    let dir = temp_project(&[
        (
            "main.zz",
            "import shapes\np := shapes.Point { x: 3, y: 4 }\nz := p.dist()",
        ),
        (
            "shapes.zz",
            "pub struct Point { x: int, y: int }\npub func dist(p: Point) -> int { return p.x + p.y }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(7));
}

#[test]
fn cross_module_struct_def() {
    // `struct shapes.Point` in shapes.zz → main uses `shapes.Point`.
    use zz_runtime::{Interp, Value};

    let dir = temp_project(&[
        (
            "main.zz",
            "import shapes\np := shapes.Point { x: 2, y: 3 }\nz := p.x",
        ),
        ("shapes.zz", "pub struct shapes.Point { x: int, y: int }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(2));
}

#[test]
fn cross_module_dotted_func_def() {
    // `func shapes.mk_point(...)` → main calls `shapes.mk_point(1, 2)`.
    use zz_runtime::{Interp, Value};

    let dir = temp_project(&[
        (
            "main.zz",
            "import shapes\np := shapes.mk_point(10, 20)\nz := p.x",
        ),
        (
            "shapes.zz",
            "pub struct shapes.Point { x: int, y: int }\npub func shapes.mk_point(x: int, y: int) -> shapes.Point { return shapes.Point { x: x, y: y } }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(10));
}

#[test]
fn impl_method_without_pub_is_private_cross_module() {
    // An impl method without `pub` must NOT be callable cross-module, and the
    // diagnostic must say it's private and tell the user to add `pub`.
    let dir = temp_project(&[
        (
            "main.zz",
            "import shapes as s\np := s.Point{ x: 5, y: 12 }\nz := p.dist()",
        ),
        (
            "shapes.zz",
            "pub struct Point { x: int, y: int }\nimpl Point {\n    func dist(self) -> int { return self.x + self.y }\n}",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    let msgs = diag_texts(&result);
    assert!(
        msgs.iter()
            .any(|m| m.contains("`dist` on struct `s.Point` is private")),
        "expected private-method error, got: {msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.contains("add `pub`")),
        "expected `add pub` hint, got: {msgs:?}"
    );
}

#[test]
fn impl_method_with_pub_is_public_cross_module() {
    // A method marked `pub` inside the impl is callable cross-module.
    use zz_runtime::{Interp, Value};

    let dir = temp_project(&[
        (
            "main.zz",
            "import shapes as s\np := s.Point{ x: 5, y: 12 }\nz := p.dist()",
        ),
        (
            "shapes.zz",
            "pub struct Point { x: int, y: int }\nimpl Point {\n    pub func dist(self) -> int { return self.x + self.y }\n}",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(17));
}

#[test]
fn import_alias_struct() {
    // `import shapes as s` → use `s.Point` etc.
    use zz_runtime::{Interp, Value};

    let dir = temp_project(&[
        (
            "main.zz",
            "import shapes as s\np := s.Point { x: 5, y: 12 }\nz := p.dist()",
        ),
        (
            "shapes.zz",
            "pub struct Point { x: int, y: int }\npub func dist(p: Point) -> int { return p.x + p.y }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(17));
}

#[test]
fn func_call_before_for_loop_no_corruption() {
    // Regression: CallPath had wrong stack effect (-argc instead of 1-argc),
    // causing stack underflow when a function call preceded a for loop.
    use zz_runtime::{Interp, Value};

    let dir = temp_project(&[(
        "main.zz",
        "func id(x: int) -> int { return x }\nx := id(42)\nfor i in 0..3 { println(i) }\nx",
    )]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(42));
}

#[test]
fn struct_method_then_recursion_in_for_loop() {
    // Regression: struct method call + recursive fib inside for loop
    // triggered the CallPath stack effect bug.
    use zz_runtime::{Interp, Value};

    let dir = temp_project(&[(
        "main.zz",
        "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { return p.x + p.y }\nfunc fib(n: int) -> int { return if n <= 1 { n } else { fib(n - 1) + fib(n - 2) } }\np := Point { x: 3, y: 4 }\nd := dist(p)\nfor i in 0..5 { println(fib(i)) }\nd",
    )]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);

    let mut interp = Interp::with_natives(result.natives.clone());
    let mut last = Value::Unit;
    for p in &result.programs {
        last = interp.run(p).unwrap();
    }
    assert_eq!(last, Value::Int(7));
}

// ---- Phase 2: Visibility & Modules tests ----

#[test]
fn pub_access_works() {
    // Public items from imported modules are accessible.
    let dir = temp_project(&[
        (
            "main.zz",
            "import shapes\nz := shapes.dist(shapes.Point{x:1, y:2})",
        ),
        (
            "shapes.zz",
            "pub struct Point { x: int, y: int }\npub func dist(p: Point) -> int { return p.x + p.y }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);
}

#[test]
fn private_access_error() {
    // Private items from imported modules produce an error.
    let dir = temp_project(&[
        ("main.zz", "import shapes\nz := shapes.hidden()"),
        (
            "shapes.zz",
            "pub struct Point { x: int, y: int }\nfunc hidden() -> int { return 42 }",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(
        result.errors.iter().any(|e| e
            .diags
            .iter()
            .any(|d| d.message.contains("undefined variable") || d.message.contains("private"))),
        "expected undefined/private error, got: {:?}",
        result.errors
    );
}

#[test]
fn private_struct_access_error() {
    // Private struct from imported module produces an error when used.
    let dir = temp_project(&[
        ("main.zz", "import shapes\np := shapes.Secret{ x: 1 }"),
        ("shapes.zz", "struct Secret { x: int }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.diags.iter().any(|d| d.message.contains("undefined")
                || d.message.contains("private")
                || d.message.contains("unknown struct"))),
        "expected undefined/private error, got: {:?}",
        result.errors
    );
}

#[test]
fn pub_reexport_works() {
    // `pub import` re-exports a namespace through the parent module.
    // Note: re-exported functions/bindings work directly. Re-exported structs
    // require type aliasing (future work) since struct types are identity-based.
    let dir = temp_project(&[
        ("main.zz", "import lib\nz := lib.math.double(21)"),
        ("lib.zz", "pub import math"),
        ("math.zz", "pub func double(n: int) -> int { return n * 2 }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);
}

#[test]
fn import_alias_reexport() {
    // `pub import math as m` re-exports as `lib.m.*`.
    let dir = temp_project(&[
        ("main.zz", "import lib\nz := lib.m.double(21)"),
        ("lib.zz", "pub import math as m"),
        ("math.zz", "pub func double(n: int) -> int { return n * 2 }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);
}

#[test]
fn plugin_import_merges_dotted_names() {
    // `import toy` resolves a path dependency shipping plugin.zzi and
    // merges its dotted signatures (no bare aliases).
    let dir = temp_project(&[
        (
            "zz.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies.toy]\npath = \"toy\"\n",
        ),
        (
            "toy/plugin.zzi",
            "// Version: 1\n// Rustc: 1.85.0\n// Plugin-version: 0.1.0\n\nextern \"C\" {\n    func toy.add(a: int, b: int) -> int\n}\n",
        ),
        ("src/main.zz", "import toy\nx := toy.add(1, 2)"),
    ]);
    let mut lock = zz_pm::lock::Lockfile::new();
    lock.upsert(zz_pm::lock::LockedDep {
        name: "toy".to_string(),
        version: "*".to_string(),
        source: "path".to_string(),
        hash: "test".to_string(),
        commit: None,
        native: None,
    });
    lock.save(&dir.join("zz.lock")).unwrap();
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert!(result.funcs.contains_key("toy.add"));
    assert!(!result.funcs.contains_key("add"));
}

#[test]
fn plugin_import_alias_copies_namespace() {
    // `import toy as t` exposes `t.*` alongside `toy.*`, never bare names.
    let dir = temp_project(&[
        (
            "zz.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies.toy]\npath = \"toy\"\n",
        ),
        (
            "toy/plugin.zzi",
            "// Version: 1\n// Rustc: 1.85.0\n// Plugin-version: 0.1.0\n\nextern \"C\" {\n    func toy.add(a: int, b: int) -> int\n}\n",
        ),
        ("src/main.zz", "import toy as t\nx := t.add(1, 2)"),
    ]);
    let mut lock = zz_pm::lock::Lockfile::new();
    lock.upsert(zz_pm::lock::LockedDep {
        name: "toy".to_string(),
        version: "*".to_string(),
        source: "path".to_string(),
        hash: "test".to_string(),
        commit: None,
        native: None,
    });
    lock.save(&dir.join("zz.lock")).unwrap();
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert!(result.funcs.contains_key("t.add"));
    assert!(result.funcs.contains_key("toy.add"));
    assert!(!result.funcs.contains_key("add"));
}

#[test]
fn method_call_not_rewritten_when_free_fn_collides() {
    // `b.resize()` must stay a method call even though the CALLER's
    // module defines its own free `resize` — the rewriter must not
    // qualify the method name when the receiver is a local value.
    // (Method and free function live in different modules here, so
    // their namespaced keys do not collide.)
    let dir = temp_project(&[
        (
            "main.zz",
            "import shapes\nimport ops\nw := shapes.wrap(40)\nx := ops.go(w)\nprintln(x)",
        ),
        (
            "shapes.zz",
            "pub struct Box { v: int }\nimpl Box {\n    pub func resize(self, n: int) -> int {\n        return self.v + n\n    }\n}\npub func wrap(v: int) -> Box {\n    return Box{v: v}\n}\n",
        ),
        (
            "ops.zz",
            "import shapes\npub func resize(a: int, b: int) -> int {\n    return a * b\n}\npub func go(b: shapes.Box) -> int {\n    return b.resize(2)\n}\n",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
}

// --- S1 check-cache tests -------------------------------------------------

fn load_check(path: &std::path::Path) -> LoadResult {
    super::load_program_check(path).expect("load_program_check")
}

fn cache_key_lines(result: &LoadResult) -> Vec<String> {
    // Canonical outcome fingerprint: sorted errors + sorted bindings.
    let mut errs: Vec<String> = diag_texts(result);
    errs.sort();
    let mut binds: Vec<String> = result
        .bindings
        .iter()
        .map(|(k, v)| format!("{k}={v:?}"))
        .collect();
    binds.sort();
    errs.extend(binds);
    errs
}

#[test]
fn check_cache_hit_matches_miss() {
    super::cache::with_isolated_cache(|| {
        // First load populates (miss), second serves from cache (hit):
        // outcomes must be identical, including bindings and funcs.
        let dir = temp_project(&[
            ("main.zz", "import math.utils\nx := utils.double(21)"),
            (
                "math/utils.zz",
                "pub func double(n: int) -> int { return n * 2 }",
            ),
        ]);
        let main = dir.join("main.zz");
        // Isolated dir starts empty: growth proves the cold load stored.
        let before = super::cache::entry_count();
        let cold = load_check(&main);
        assert!(no_errors(&cold), "cold errors: {:?}", cold.errors);
        assert!(
            super::cache::entry_count() > before,
            "cold load stored nothing"
        );
        let warm = load_check(&main);
        assert!(no_errors(&warm), "warm errors: {:?}", warm.errors);
        assert_eq!(cache_key_lines(&cold), cache_key_lines(&warm));
        assert_eq!(cold.bindings, warm.bindings);
        let mut cold_keys: Vec<_> = cold.funcs.keys().collect();
        let mut warm_keys: Vec<_> = warm.funcs.keys().collect();
        cold_keys.sort();
        warm_keys.sort();
        assert_eq!(cold_keys, warm_keys);
    });
}

#[test]
fn check_cache_invalidates_on_edit() {
    super::cache::with_isolated_cache(|| {
        // Touching one module re-checks it; the error must appear on the
        // very next load (no stale clean hit).
        let dir = temp_project(&[
            ("main.zz", "import config\nx := config.base + 1"),
            ("config.zz", "pub base := 41"),
        ]);
        let main = dir.join("main.zz");
        let ok = load_check(&main);
        assert!(no_errors(&ok));
        std::fs::write(dir.join("config.zz"), "pub base := \"not an int\"\n").unwrap();
        let broken = load_check(&main);
        assert!(
            !no_errors(&broken),
            "expected type error after edit, got clean"
        );
        // And reverting restores the clean outcome (old key still valid).
        std::fs::write(dir.join("config.zz"), "pub base := 41").unwrap();
        let fixed = load_check(&main);
        assert!(no_errors(&fixed), "errors: {:?}", fixed.errors);
    });
}

#[test]
fn check_cache_replays_errors() {
    super::cache::with_isolated_cache(|| {
        // Error outcomes replay identically from cache (no error loss).
        let dir = temp_project(&[
            ("main.zz", "import config\nx := config.base + 1"),
            ("config.zz", "pub base := \"not an int\"\n"),
        ]);
        let main = dir.join("main.zz");
        let first = load_check(&main);
        assert!(!no_errors(&first));
        let second = load_check(&main);
        assert_eq!(cache_key_lines(&first), cache_key_lines(&second));
    });
}

#[test]
fn check_cache_rechecks_only_dependents() {
    super::cache::with_isolated_cache(|| {
        // Dep-aware keys: touching ANY single leaf rechecks exactly the
        // leaf + main (2 modules), regardless of load position. A
        // load-order chain would recheck 21 for mod00 and 2 for mod19.
        let mut owned: Vec<(String, String)> = vec![(
            "main.zz".to_string(),
            (0..4)
                .map(|m| format!("import mod{m:02}"))
                .collect::<Vec<_>>()
                .join("\n")
                + "\nx := mod00.mod00_f0(1) + mod03.mod03_f0(2)\n",
        )];
        for m in 0..4 {
            owned.push((
                format!("mod{m:02}.zz"),
                format!("pub func mod{m:02}_f0(x: int) -> int {{ return x + {m} }}"),
            ));
        }
        let refs: Vec<(&str, &str)> = owned
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let dir = temp_project(&refs);
        let main = dir.join("main.zz");
        let cold = load_check(&main);
        assert!(no_errors(&cold), "cold errors: {:?}", cold.errors);
        let base = super::cache::entry_count();
        // Touch the FIRST module: only it + main recheck (2 new entries).
        std::fs::write(
            dir.join("mod00.zz"),
            "pub func mod00_f0(x: int) -> int { return x + 100 }\n",
        )
        .unwrap();
        let edited = load_check(&main);
        assert!(no_errors(&edited), "errors: {:?}", edited.errors);
        assert_eq!(
            super::cache::entry_count(),
            base + 2,
            "editing mod00 must recheck exactly mod00 + main"
        );
    });
}

#[test]
fn check_cache_disabled_via_env() {
    super::cache::with_isolated_cache(|| {
        // ZZ_CHECK_CACHE=0 forces fresh checks (thread-scoped: parallel tests
        // keep their own cache).
        let _off = zz_pm::paths::env_scope(&[("ZZ_CHECK_CACHE", Some("0".into()))]);
        let dir = temp_project(&[
            ("main.zz", "import config\nx := config.base + 1"),
            ("config.zz", "pub base := 41"),
        ]);
        let result = load_check(&dir.join("main.zz"));
        assert!(no_errors(&result), "errors: {:?}", result.errors);
    });
}

#[test]
fn check_cache_namespaces_do_not_poison() {
    // #290: the same module imported under two namespaces across entries
    // (`import area as ar` vs plain `import area`) must not share one
    // cache entry — cached pubs are stored already-namespaced, so the
    // second entry would restore the winner's qualifier set and report
    // spurious `undefined variable` errors.
    super::cache::with_isolated_cache(|| {
        let dir = temp_project(&[
            ("area.zz", "pub func doc_new(t: str) -> str { return t }\n"),
            ("a.zz", "import area as ar\nx := ar.doc_new(\"hi\")\n"),
            ("b.zz", "import area\ny := area.doc_new(\"hi\")\n"),
        ]);
        let first = load_check(&dir.join("a.zz"));
        assert!(no_errors(&first), "aliased errors: {:?}", first.errors);
        let second = load_check(&dir.join("b.zz"));
        assert!(no_errors(&second), "plain errors: {:?}", second.errors);
        // Reverse order poisons nothing either (warm cache, other winner).
        let third = load_check(&dir.join("a.zz"));
        assert!(no_errors(&third), "re-aliased errors: {:?}", third.errors);
    });
}

#[test]
fn failed_dependency_collapses_cascade() {
    // #274: a broken dependency reports once at its source; each importer
    // gets one targeted error instead of N `undefined variable` /
    // `unknown function` follow-ons.
    let dir = temp_project(&[
        ("cache.zz", "pub func save(root: str) -> int { return root + 1 }\n"),
        (
            "main.zz",
            "import cache\nfunc main() {\n    a := cache.save(\"x\")\n    b := cache.save(\"y\")\n    println(a)\n    println(b)\n}\n",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    // Root cause still reported once at the dependency itself.
    let dep_errs: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.name.ends_with("cache.zz"))
        .collect();
    assert_eq!(dep_errs.len(), 1, "dep must fail once: {:?}", result.errors);
    // Importer fails once with the collapsed error, not the cascade.
    let imp_errs: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.name.ends_with("main.zz"))
        .collect();
    assert_eq!(
        imp_errs.len(),
        1,
        "importer must fail once: {:?}",
        result.errors
    );
    let texts: Vec<String> = imp_errs[0]
        .diags
        .iter()
        .map(|d| d.message.clone())
        .collect();
    assert!(
        texts
            .iter()
            .any(|m| m.contains("failed to check") && m.contains("cache")),
        "expected collapsed import-failed error: {texts:?}"
    );
    assert!(
        !texts.iter().any(|m| m.contains("`cache.")),
        "cascade not collapsed: {texts:?}"
    );
}

#[test]
fn selective_std_call_marks_import_used() {
    // Regression: the loader rewrites `pow(2, 3)` to the canonical
    // `std.math.pow(2, 3)`, while the census only knew the short
    // `math.pow` form — every selectively-imported std call warned
    // `unused import` falsely.
    let dir = temp_project(&[(
        "main.zz",
        "import std.math(pow)\nfunc main() {\n    println(pow(2, 3))\n}\n",
    )]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert!(
        !diag_texts(&result)
            .iter()
            .any(|m| m.contains("unused import")),
        "selective call must not warn: {:?}",
        diag_texts(&result)
    );
}

#[test]
fn selective_std_unused_still_warns() {
    let dir = temp_project(&[(
        "main.zz",
        "import std.math(pow)\nfunc main() {\n    println(1)\n}\n",
    )]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(
        diag_texts(&result)
            .iter()
            .any(|m| m.contains("unused import `pow`")),
        "genuinely-unused selective must warn: {:?}",
        diag_texts(&result)
    );
}

#[test]
fn later_module_selective_reaches_all_funcs() {
    // Regression (batched parity): std-selective seeds of later modules
    // never reached `all_funcs` (only the first-success snapshot did),
    // so the native HIR re-check reported `undefined variable` for them
    // while the VM stayed clean.
    let dir = temp_project(&[
        ("cA.zz", "import std.math(PI)\nprintln(PI)\n"),
        ("cB.zz", "import std.math(E as ee)\nprintln(ee)\n"),
        (
            "drv.zz",
            "import cA as c0\nimport cB as c1\nfunc main() {\n    println(\"ok\")\n}\n",
        ),
    ]);
    let result = load_program(&dir.join("drv.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert!(
        result.funcs.contains_key("ee"),
        "aliased selective const must survive into all_funcs"
    );
    assert!(
        result.funcs.contains_key("PI"),
        "plain selective const must survive into all_funcs"
    );
}

/// Manifest helper for package-mapping tests (#288).
fn pkg_manifest(name: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n")
}

#[test]
fn package_import_resolves_from_tests_dir() {
    // `tests/` file imports `src/` module via the package name: the
    // core acceptance case of #288 (no fragile `../src/...` path).
    let manifest = pkg_manifest("app");
    let dir = temp_project(&[
        ("zz.toml", manifest.as_str()),
        (
            "src/math.zz",
            "pub func add(a: int, b: int) -> int { return a + b }",
        ),
        ("tests/check.zz", "import app.math\nx := math.add(1, 2)"),
    ]);
    let result = load_program(&dir.join("tests/check.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.programs.len(), 2);
    assert_eq!(result.bindings["check.x"], Type::Int);
}

#[test]
fn package_selective_and_alias_imports() {
    let manifest = pkg_manifest("app");
    let dir = temp_project(&[
        ("zz.toml", manifest.as_str()),
        (
            "src/math.zz",
            "pub func add(a: int, b: int) -> int { return a + b }",
        ),
        (
            "src/other.zz",
            "pub func mul(a: int, b: int) -> int { return a * b }",
        ),
        (
            "tests/sel.zz",
            "import app.math(add as f)\nimport app.other as m\nx := f(1, 2)\ny := m.mul(3, 4)",
        ),
    ]);
    let result = load_program(&dir.join("tests/sel.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["sel.x"], Type::Int);
    assert_eq!(result.bindings["sel.y"], Type::Int);
}

#[test]
fn package_wildcard_import() {
    let manifest = pkg_manifest("app");
    let dir = temp_project(&[
        ("zz.toml", manifest.as_str()),
        (
            "src/math.zz",
            "pub func add(a: int, b: int) -> int { return a + b }",
        ),
        ("tests/wc.zz", "import app.math(*)\nx := add(1, 2)"),
    ]);
    let result = load_program(&dir.join("tests/wc.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["wc.x"], Type::Int);
}

#[test]
fn package_bare_import_binds_package_name() {
    // Bare `import app` loads `src/main.zz` under the `app` namespace
    // (not the `main` file stem) so `app.x()` and `import app(x)`
    // agree with each other.
    let manifest = pkg_manifest("app");
    let dir = temp_project(&[
        ("zz.toml", manifest.as_str()),
        ("src/main.zz", "pub base := 41"),
        ("main.zz", "import app\nx := app.base + 1"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.x"], Type::Int);
}

#[test]
fn package_bare_selective_uses_package_ns() {
    let manifest = pkg_manifest("app");
    let dir = temp_project(&[
        ("zz.toml", manifest.as_str()),
        ("src/main.zz", "pub func who() -> int { return 7 }"),
        ("main.zz", "import app(who as w)\nx := w()"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.x"], Type::Int);
}

#[test]
fn package_missing_file_diagnostic() {
    let manifest = pkg_manifest("app");
    let dir = temp_project(&[
        ("zz.toml", manifest.as_str()),
        (
            "src/math.zz",
            "pub func add(a: int, b: int) -> int { return a + b }",
        ),
        ("tests/bad.zz", "import app.nope\n1"),
    ]);
    let result = load_program(&dir.join("tests/bad.zz")).unwrap();
    let texts = diag_texts(&result);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("resolved to") && t.contains("src/nope.zz")),
        "expected mapping diagnostic, got: {texts:?}",
    );
}

#[test]
fn package_bare_missing_main_diagnostic() {
    let manifest = pkg_manifest("app");
    let dir = temp_project(&[("zz.toml", manifest.as_str()), ("main.zz", "import app\n1")]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    let texts = diag_texts(&result);
    assert!(
        texts.iter().any(|t| t.contains("src/main.zz")),
        "expected src/main.zz hint, got: {texts:?}",
    );
}

#[test]
fn package_hyphen_name_matches_underscore_import() {
    // `package.name = "my-app"` is importable as `my_app.*`.
    let manifest = pkg_manifest("my-app");
    let dir = temp_project(&[
        ("zz.toml", manifest.as_str()),
        (
            "src/math.zz",
            "pub func add(a: int, b: int) -> int { return a + b }",
        ),
        ("tests/check.zz", "import my_app.math\nx := math.add(1, 2)"),
    ]);
    let result = load_program(&dir.join("tests/check.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["check.x"], Type::Int);
}

#[test]
fn package_mapping_beats_relative_lookalike() {
    // A relative `./app.zz` must not shadow the package mapping: bare
    // `import app` resolves to `src/main.zz` (precedence from #288).
    let manifest = pkg_manifest("app");
    let dir = temp_project(&[
        ("zz.toml", manifest.as_str()),
        ("app.zz", "pub marker := 1"),
        ("src/main.zz", "pub base := 41"),
        ("main.zz", "import app\nx := app.base + 1"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert!(
        result.files.iter().any(|(n, _)| {
            let comps: Vec<_> = std::path::Path::new(n).components().collect();
            comps.len() >= 2
                && comps[comps.len() - 2].as_os_str() == "src"
                && comps[comps.len() - 1].as_os_str() == "main.zz"
        }),
        "expected src/main.zz in load set, got: {:?}",
        result.files.iter().map(|(n, _)| n).collect::<Vec<_>>(),
    );
    assert!(
        !result.files.iter().any(|(n, _)| n.ends_with("/app.zz")),
        "relative ./app.zz must not load, got: {:?}",
        result.files.iter().map(|(n, _)| n).collect::<Vec<_>>(),
    );
}

#[test]
fn package_cycle_reported_at_import_site() {
    let manifest = pkg_manifest("app");
    let dir = temp_project(&[
        ("zz.toml", manifest.as_str()),
        (
            "src/a.zz",
            "import app.b\npub func fa() -> int { return 1 }",
        ),
        (
            "src/b.zz",
            "import app.a\npub func fb() -> int { return 2 }",
        ),
        ("tests/e.zz", "import app.a\n1"),
    ]);
    let result = load_program(&dir.join("tests/e.zz")).unwrap();
    assert!(
        result.errors.iter().any(|e| e
            .diags
            .iter()
            .any(|d| d.message.contains("circular import"))),
        "expected circular import error, got: {:?}",
        result.errors
    );
}

#[test]
fn non_matching_import_still_resolves_relatively() {
    // A manifest in scope must not reroute imports whose first segment
    // is not the package name (regression guard for #288 fallback).
    let manifest = pkg_manifest("app");
    let dir = temp_project(&[
        ("zz.toml", manifest.as_str()),
        ("helper.zz", "pub base := 41"),
        ("main.zz", "import helper\nx := helper.base + 1"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.x"], Type::Int);
}

#[test]
fn package_import_resolves_from_nested_tests_dir() {
    // Deeper nesting on both sides: importer at `tests/unit/` reaches
    // `src/nested/` through the root manifest alone (no nested
    // manifests, no symlinks, no `tests/src/` — regression for #288).
    let manifest = pkg_manifest("app");
    let dir = temp_project(&[
        ("zz.toml", manifest.as_str()),
        ("src/math.zz", "pub func add(a: int, b: int) -> int { return a + b }"),
        (
            "src/nested/deep.zz",
            "import app.math\npub func deep_add(a: int, b: int) -> int { return math.add(a, b) + 100 }",
        ),
        (
            "tests/unit/check.zz",
            "import app.nested.deep\nimport app.math(add as f)\nx := deep.deep_add(1, 2)\ny := f(20, 22)",
        ),
    ]);
    let result = load_program(&dir.join("tests/unit/check.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.programs.len(), 3);
    assert_eq!(result.bindings["check.x"], Type::Int);
    assert_eq!(result.bindings["check.y"], Type::Int);
}

#[test]
fn stdlib_import_kinds_recorded_for_scoped_runs() {
    // Full + selective + wildcard + aliased imports across two modules:
    // scoped run-cache hits must replay exactly this set.
    let dir = temp_project(&[
        (
            "main.zz",
            "import std.math\nimport std.str(length)\nimport dep.help\nfunc main() {\n    println(math.abs(0 - 1))\n    println(length(\"hey\"))\n    println(help.run())\n}\n",
        ),
        (
            "dep/help.zz",
            "import std.vec as v\nimport std.map(*)\npub func run() -> int {\n    return v.len([1]) + 0\n}\n",
        ),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.stdlib_modules, vec!["map", "math", "str", "vec"]);
    assert_eq!(
        result.stdlib_selectives,
        vec![("str".to_string(), vec![("length".to_string(), None)])]
    );
    assert_eq!(result.stdlib_wildcards, vec!["map".to_string()]);
    // Aliased full import keeps its (module, alias) pair for mirrors.
    assert!(
        result
            .stdlib_aliases
            .contains(&("vec".to_string(), "v".to_string())),
        "aliases: {:?}",
        result.stdlib_aliases
    );

    // #503: a bare `ns.func` spelling of a *pure-ZZ* helper pulls the module
    // into the closure the VM and both codegen backends compile. Without it
    // the call checked clean and then failed at run time with `undefined
    // method` (the HIR backend merged everything and silently succeeded).
    // Bare *natives* are always registered and must not drag anything in —
    // `vec.push` here must add neither `vec` nor `str`.
    let dir = temp_project(&[(
        "bare.zz",
        "func main() {\n    println(str.repeat(\"ab\", 2))\n    println(math.min(1, 2))\n    println(vec.push([1], 2))\n    println(str.length(\"abc\"))\n    println(\"bare_ok\")\n}\n",
    )]);
    let result = load_program(&dir.join("bare.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.stdlib_modules, vec!["math", "str"]);

    // …and a program that only uses bare *natives* stays closure-free, so
    // the scan cannot turn an import-free program into a stdlib compile.
    let dir = temp_project(&[(
        "native.zz",
        "func main() {\n    println(str.length(\"abc\"))\n    println(vec.push([1], 2))\n}\n",
    )]);
    let result = load_program(&dir.join("native.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert!(
        result.stdlib_modules.is_empty(),
        "bare natives must not pull stdlib modules: {:?}",
        result.stdlib_modules
    );

    // Qualified paths (`helper.str.repeat`) never match: the scan only
    // accepts the two-segment `ns.func` spelling with a std namespace head,
    // so a dotted path whose *middle* segment is a std namespace is safe.
    // (The program below need not type-check — the closure decision is
    // deliberately syntactic, which is why an over-match would only cost a
    // little startup work rather than miscompile.)
    let dir = temp_project(&[
        (
            "dotted.zz",
            "import helper\nfunc main() {\n    println(helper.str.repeat(\"ab\", 2))\n}\n",
        ),
        ("helper.zz", "pub x := 1"),
    ]);
    let dotted = load_program(&dir.join("dotted.zz")).unwrap();
    assert!(
        dotted.stdlib_modules.is_empty(),
        "dotted spellings must not pull stdlib modules: {:?}",
        dotted.stdlib_modules
    );
}

#[test]
fn dep_style_warnings_collapse_to_one_note() {
    // #387: a registry/path dep's style lints are true positives the
    // consumer can neither fix nor silence — they must not print per
    // site. Errors from deps still propagate. Path deps stand in for
    // registry deps (same resolver path).
    let dir = temp_project(&[
        (
            "app/zz.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nask = { path = \"../askpkg\" }\n",
        ),
        (
            "app/src/main.zz",
            "import ask.editor\nprintln(editor.edit())",
        ),
        ("askpkg/zz.toml", "[package]\nname = \"ask\"\nversion = \"0.1.0\"\n"),
        (
            "askpkg/src/helper.zz",
            "pub func get() -> int { return 1 }",
        ),
        (
            // Two style lints inside the dep (an unused import and an
            // unused local). Neither may reach the consumer.
            "askpkg/src/editor.zz",
            "import ask.helper\npub func edit() -> int {\n    unused := 1\n    return 2\n}",
        ),
    ]);
    let mut lock = zz_pm::lock::Lockfile::new();
    lock.deps.push(zz_pm::lock::LockedDep {
        name: "ask".to_string(),
        version: "*".to_string(),
        source: "path".to_string(),
        hash: String::new(),
        commit: None,
        native: None,
    });
    lock.save(&dir.join("app/zz.lock")).expect("write lock");
    let result = load_program(&dir.join("app/src/main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    // Messages only (notes echo the first hidden warning for triage).
    let msgs: Vec<&str> = result
        .errors
        .iter()
        .flat_map(|e| e.diags.iter().map(|d| d.message.as_str()))
        .collect();
    // The raw per-site warnings must not reach the consumer ...
    assert!(
        !msgs
            .iter()
            .any(|m| m.contains("unused variable") || m.contains("unused import")),
        "dep style warnings must collapse, got: {:?}",
        msgs
    );
    // ... but the collapse note names the dependency.
    assert!(
        msgs.iter()
            .any(|m| m.contains("inside dependency") && m.contains("hidden")),
        "expected collapse note, got: {:?}",
        msgs
    );
    assert!(
        msgs.iter().any(|m| m.contains("inside dependency")
            && m.contains("`editor`")
            && m.contains("hidden")),
        "collapse note must name the dependency, got: {:?}",
        msgs
    );
}

#[test]
fn first_party_style_warning_still_fires() {
    // #387 companion: first-party files keep the raw per-site warning.
    let dir = temp_project(&[
        (
            "zz.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        ),
        ("src/main.zz", "unused := 1"),
    ]);
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert!(
        diag_texts(&result)
            .iter()
            .any(|m| m.contains("unused variable")),
        "first-party style warning must fire, got: {:?}",
        diag_texts(&result)
    );
}

#[test]
fn dep_errors_still_propagate() {
    // #387 companion: only warnings collapse — dep type errors still
    // break the consumer build.
    let dir = temp_project(&[
        (
            "app/zz.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nask = { path = \"../askpkg\" }\n",
        ),
        (
            "app/src/main.zz",
            "import ask.broken\nx := broken.f()",
        ),
        ("askpkg/zz.toml", "[package]\nname = \"ask\"\nversion = \"0.1.0\"\n"),
        ("askpkg/src/broken.zz", "pub func f() -> int { \"nope\" }"),
    ]);
    let mut lock = zz_pm::lock::Lockfile::new();
    lock.deps.push(zz_pm::lock::LockedDep {
        name: "ask".to_string(),
        version: "*".to_string(),
        source: "path".to_string(),
        hash: String::new(),
        commit: None,
        native: None,
    });
    lock.save(&dir.join("app/zz.lock")).expect("write lock");
    let result = load_program(&dir.join("app/src/main.zz")).unwrap();
    assert!(
        !no_errors(&result),
        "dep type error must propagate, got: {:?}",
        result.errors
    );
}

#[test]
fn missing_bare_import_hints_transitive_provider() {
    // #420: `rand` is declared by direct dep `faker` but not installed
    // for direct use — the error must name the provider and the fix.
    let dir = temp_project(&[
        (
            "zz.toml",
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n[dependencies]\nfaker = { path = \"./fakerlib\" }\n",
        ),
        (
            "fakerlib/zz.toml",
            "[package]\nname = \"faker\"\nversion = \"0.1.0\"\n[dependencies]\nrand = \"^0.4.1\"\n",
        ),
        (
            "src/main.zz",
            "import faker\nimport rand\nfunc main() {\n    println(1)\n}\n",
        ),
    ]);
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    let texts = diag_texts(&result);
    let joined = texts.join("\n");
    assert!(
        joined.contains("transitively via `faker`") && joined.contains("zz add rand"),
        "expected transitive hint, got: {joined}"
    );
}

#[test]
fn transitive_path_import_resolves_when_locked() {
    // #420 fixed behavior: `leaf` is only declared by direct dep `mid`,
    // but once locked (full-tree `zz install`) `import leaf` resolves —
    // no `zz add leaf`, no transitive hint.
    let dir = temp_project(&[
        (
            "zz.toml",
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n[dependencies]\nmid = { path = \"./midlib\" }\n",
        ),
        (
            "midlib/zz.toml",
            "[package]\nname = \"mid\"\nversion = \"0.1.0\"\n[dependencies]\nleaf = { path = \"../leaflib\" }\n",
        ),
        (
            "leaflib/zz.toml",
            "[package]\nname = \"leaf\"\nversion = \"0.1.0\"\n",
        ),
        ("midlib/src/main.zz", "import leaf\npub func midval() -> int { return leaf.leafval() }"),
        ("leaflib/src/main.zz", "pub func leafval() -> int { return 41 }"),
        (
            "src/main.zz",
            "import mid\nimport leaf\nx := mid.midval() + leaf.leafval()",
        ),
    ]);
    let mut lock = zz_pm::lock::Lockfile::new();
    for name in ["mid", "leaf"] {
        lock.deps.push(zz_pm::lock::LockedDep {
            name: name.to_string(),
            version: "*".to_string(),
            source: "path".to_string(),
            hash: String::new(),
            commit: None,
            native: None,
        });
    }
    lock.save(&dir.join("zz.lock")).expect("write lock");
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.x"], Type::Int);
}

#[test]
fn hyphen_package_import_resolves_both_spellings() {
    // #486: the package dir and lock name use `-`, but `-` never parsed
    // in import paths and `_` never resolved. Both spellings must load
    // with no `zz add` rename workaround.
    let dir = temp_project(&[
        (
            "zz.toml",
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n[dependencies]\nmy-lib = { path = \"./mylib\" }\n",
        ),
        (
            "mylib/zz.toml",
            "[package]\nname = \"my-lib\"\nversion = \"0.1.0\"\n",
        ),
        ("mylib/src/main.zz", "pub func pkgval() -> int { return 41 }"),
        ("mylib/src/util.zz", "pub func uval() -> int { return 1 }"),
        (
            "src/main.zz",
            "import my-lib as lib\nimport my_lib.util as u\nx := lib.pkgval() + u.uval()",
        ),
    ]);
    let mut lock = zz_pm::lock::Lockfile::new();
    lock.deps.push(zz_pm::lock::LockedDep {
        name: "my-lib".to_string(),
        version: "*".to_string(),
        source: "path".to_string(),
        hash: String::new(),
        commit: None,
        native: None,
    });
    lock.save(&dir.join("zz.lock")).expect("write lock");
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.x"], Type::Int);
}

#[test]
fn missing_bare_import_hints_uninstalled_declaration() {
    // #420: declared in zz.toml but never installed — point at install.
    let dir = temp_project(&[
            (
                "zz.toml",
                "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n[dependencies]\nrand = \"^0.4.1\"\n",
            ),
            (
                "src/main.zz",
                "import rand\nfunc main() {\n    println(1)\n}\n",
            ),
        ]);
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    let texts = diag_texts(&result);
    let joined = texts.join("\n");
    assert!(
        joined.contains("declared in zz.toml but not installed") && joined.contains("zz install"),
        "expected install hint, got: {joined}"
    );
}

// ── Directory collections ([collect.*]) ─────────────────────────────

const COLLECTION_MANIFEST: &str = "[package]\n\
name = \"demo\"\n\
version = \"0.1.0\"\n\
[collect.commands]\n\
dir = \"commands\"\n\
sig = \"func([str]) -> int\"\n";

fn collection_project(files: &[(&str, &str)]) -> PathBuf {
    let mut all = vec![("zz.toml", COLLECTION_MANIFEST)];
    all.extend_from_slice(files);
    all.push(("commands/.keep", ""));
    temp_project(&all)
}

fn collection_source(result: &LoadResult) -> String {
    result
        .files
        .iter()
        .find(|(_, src)| src.contains("Generated from [collect."))
        .map(|(_, src)| src.clone())
        .expect("synthesized collection source present")
}

#[test]
fn invalid_declared_collection_fails_even_when_unused() {
    let dir = temp_project(&[
        (
            "zz.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\
             [collect.commands]\ndir = \"missing\"\nsig = \"func([str]) -> int\"\n",
        ),
        ("src/main.zz", "x := 1\n"),
    ]);
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    let joined = diag_texts(&result).join("\n");
    assert!(
        joined.contains("collection `commands` in zz.toml is invalid"),
        "{joined}"
    );
    assert!(joined.contains("does not exist"), "{joined}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn collection_custom_export_name() {
    let dir = temp_project(&[
        (
            "zz.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\
             [collect.commands]\ndir = \"commands\"\nsig = \"func([str]) -> int\"\nexport = \"main\"\n",
        ),
        ("src/main.zz", "import collect.commands\ncommands.all()\n"),
        (
            "commands/greet.zz",
            "pub func main(args: [str]) -> int { return args.len() }\n",
        ),
    ]);
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    let src = collection_source(&result);
    assert!(src.contains("    main: func([str]) -> int\n"), "{src}");
    assert!(src.contains("main: __collect_commands_greet.main"), "{src}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn collection_loads_and_exposes_get_and_all() {
    let dir = collection_project(&[
        (
            "src/main.zz",
            "import collect.commands\n\
             entries := commands.all()\n\
             hit := commands.get(\"greet\")\n",
        ),
        (
            "commands/greet.zz",
            "pub func run(args: [str]) -> int { return args.len() }\n",
        ),
        (
            "commands/build.zz",
            "pub func run(args: [str]) -> int { return args.len() + 1 }\n",
        ),
    ]);
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert!(matches!(result.bindings["main.entries"], Type::Array(_)));
    assert!(matches!(result.bindings["main.hit"], Type::Option(_)));
    assert!(result.funcs.contains_key("commands.all"));
    assert!(result.funcs.contains_key("commands.get"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn collection_members_are_deterministically_ordered() {
    let dir = collection_project(&[
        ("src/main.zz", "import collect.commands\ncommands.all()\n"),
        (
            "commands/zeta.zz",
            "pub func run(args: [str]) -> int { return 0 }\n",
        ),
        (
            "commands/alpha.zz",
            "pub func run(args: [str]) -> int { return 0 }\n",
        ),
        (
            "commands/mid.zz",
            "pub func run(args: [str]) -> int { return 0 }\n",
        ),
    ]);
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    let src = collection_source(&result);
    let alpha = src.find("name: \"alpha\"").unwrap();
    let mid = src.find("name: \"mid\"").unwrap();
    let zeta = src.find("name: \"zeta\"").unwrap();
    assert!(alpha < mid && mid < zeta, "got:\n{src}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn empty_collection_is_valid() {
    let dir = collection_project(&[(
        "src/main.zz",
        "import collect.commands\nn := commands.all().len()\n",
    )]);
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.n"], Type::Int);
    assert!(collection_source(&result).contains("pub const ALL: [Entry] = [\n]"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn collection_alias_import_works() {
    let dir = collection_project(&[
        (
            "src/main.zz",
            "import collect.commands as cmds\nn := cmds.all().len()\n",
        ),
        (
            "commands/greet.zz",
            "pub func run(args: [str]) -> int { return 1 }\n",
        ),
    ]);
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.n"], Type::Int);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn collection_signature_mismatch_reports_expected_and_found() {
    let dir = collection_project(&[
        ("src/main.zz", "import collect.commands\ncommands.all()\n"),
        (
            "commands/bad.zz",
            "pub func run(args: [str]) -> str { return \"nope\" }\n",
        ),
        (
            "commands/good.zz",
            "pub func run(args: [str]) -> int { return 1 }\n",
        ),
    ]);
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    let joined = diag_texts(&result).join("\n");
    assert!(
        joined.contains("does not match collection `commands`"),
        "{joined}"
    );
    assert!(
        joined.contains("expected: func run(args: [str]) -> int"),
        "{joined}"
    );
    assert!(
        joined.contains("found:    func run(args: [str]) -> str"),
        "{joined}"
    );
    let src = collection_source(&result);
    assert!(src.contains("name: \"good\""), "{src}");
    assert!(
        !src.contains("name: \"bad\""),
        "invalid member kept:\n{src}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn collection_missing_export_is_reported_at_member() {
    let dir = collection_project(&[
        ("src/main.zz", "import collect.commands\ncommands.all()\n"),
        (
            "commands/other.zz",
            "pub func other(args: [str]) -> int { return 0 }\n",
        ),
    ]);
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    let joined = diag_texts(&result).join("\n");
    assert!(joined.contains("is missing"), "{joined}");
    assert!(joined.contains("hint: add `pub run("), "{joined}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn collection_name_collision_is_loud() {
    let dir = collection_project(&[
        ("src/main.zz", "import collect.commands\ncommands.all()\n"),
        (
            "commands/foo-bar.zz",
            "pub func run(args: [str]) -> int { return 0 }\n",
        ),
        (
            "commands/foo_bar.zz",
            "pub func run(args: [str]) -> int { return 0 }\n",
        ),
    ]);
    let result = load_program(&dir.join("src/main.zz")).unwrap();
    let joined = diag_texts(&result).join("\n");
    assert!(joined.contains("both map to member `foo_bar`"), "{joined}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn collection_sees_files_added_and_removed_between_loads() {
    let dir = collection_project(&[
        ("src/main.zz", "import collect.commands\ncommands.all()\n"),
        (
            "commands/greet.zz",
            "pub func run(args: [str]) -> int { return 0 }\n",
        ),
    ]);
    let first = load_program(&dir.join("src/main.zz")).unwrap();
    assert!(no_errors(&first), "errors: {:?}", first.errors);
    let src = collection_source(&first);
    assert!(src.contains("name: \"greet\""));
    assert!(!src.contains("name: \"build\""));

    fs::write(
        dir.join("commands/build.zz"),
        "pub func run(args: [str]) -> int { return 0 }\n",
    )
    .unwrap();
    let second = load_program(&dir.join("src/main.zz")).unwrap();
    assert!(no_errors(&second), "errors: {:?}", second.errors);
    let src = collection_source(&second);
    assert!(src.contains("name: \"build\""));
    assert!(src.contains("name: \"greet\""));

    fs::remove_file(dir.join("commands/greet.zz")).unwrap();
    let third = load_program(&dir.join("src/main.zz")).unwrap();
    assert!(no_errors(&third), "errors: {:?}", third.errors);
    let src = collection_source(&third);
    assert!(src.contains("name: \"build\""));
    assert!(!src.contains("name: \"greet\""));
    let _ = fs::remove_dir_all(&dir);
}

/// Whether `chunk`, or any function/closure chunk nested inside it, holds
/// an `Op::CallPath`.
fn has_callpath(chunk: &zz_runtime::vm::Chunk) -> bool {
    chunk.code.iter().any(|op| match op {
        zz_runtime::vm::Op::CallPath { .. } => true,
        zz_runtime::vm::Op::MakeFunc { chunk, .. }
        | zz_runtime::vm::Op::MakeClosure { chunk, .. }
        | zz_runtime::vm::Op::SpawnClosure { chunk, .. } => has_callpath(chunk),
        _ => false,
    })
}

#[test]
fn module_aware_compile_emits_callpath_for_function_calls() {
    // The VM's direct-call fast path looks up `Op::CallPath`'s precomputed
    // `joined` key instead of building a `Value::Func` (which clones the
    // function per call). That only holds while the module-aware compiler —
    // the one `zz run` uses, which namespaces top-level functions into
    // `module.fn` paths — emits a path op for a whole-program call. If it
    // ever emits `Op::Call`, the optimization silently stops applying.
    let dir = temp_project(&[(
        "main.zz",
        "func fib(n: int) -> int { return if n <= 1 { n } else { fib(n - 1) + fib(n - 2) } }\nfib(3)",
    )]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    let native_names: std::sync::Arc<std::collections::HashSet<String>> =
        std::sync::Arc::new(result.natives.keys().cloned().collect());
    let chunk = zz_runtime::vm::Compiler::compile_program_typed(
        &result.programs[0],
        std::sync::Arc::new(std::collections::HashMap::new()),
        result.structs.clone(),
        result.enums.clone(),
        native_names,
    );
    assert!(
        has_callpath(&chunk),
        "expected a CallPath for a module-qualified function call, got {:?}",
        chunk.code
    );
}
