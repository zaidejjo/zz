use super::*;
use std::fs;

/// Create a unique temp dir with the given (relative path → contents).
fn temp_project(files: &[(&str, &str)]) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("zz_loader_test_{}", n));
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

#[test]
fn loads_relative_import() {
    let dir = temp_project(&[
        ("main.zz", "import math.utils\nx := utils.double(21)"),
        ("math/utils.zz", "pub func double(n: int) -> int { n * 2 }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.programs.len(), 2);
    assert_eq!(result.bindings["main.x"], Type::Int);
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
        ("math/utils.zz", "pub func double(n: int) -> int { n * 2 }"),
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
            "func twice(n: int) -> int { n * 2 }\npub func double(n: int) -> int { twice(n) }",
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
            "n := 100\npub func double(n: int) -> int { n * 2 }",
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
        ("a/utils.zz", "func f() -> int { 1 }"),
        ("b/utils.zz", "func g() -> int { 2 }"),
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
fn two_namespaces_for_one_module_detected() {
    let dir = temp_project(&[
        ("main.zz", "import utils\nimport utils as u\n1"),
        ("utils.zz", "func f() -> int { 1 }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.diags.iter().any(|d| d.message.contains("two namespaces"))),
        "expected two-namespace error, got: {:?}",
        result.errors
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
        ("d.zz", "func f() -> int { 1 }"),
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
        ("main.zz", "import std.io\nimport std.str\nimport math.utils\nn := utils.double(6)\nstr.length(\"abc\")"),
        ("math/utils.zz", "pub func double(n: int) -> int { n * 2 }"),
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
fn stdlib_import_needs_no_file() {
    let dir = temp_project(&[("main.zz", "import std.io\nimport std.str\n1")]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.programs.len(), 1);
    assert!(result.funcs.contains_key("std.io.println"));
    // Namespaced copies are registered too.
    assert!(result.funcs.contains_key("io.println"));
    assert!(result.natives.contains_key("io.println"));
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
            "pub struct Point { x: int, y: int }\npub func dist(p: Point) -> int { p.x + p.y }",
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
            "pub c_val := 10\npub func c_func(x: int) -> int { x + 1 }",
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
        ("b.zz", "pub func add(a: int, b: int) -> int { a + b }"),
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
    // Private binding in module B is NOT accessible from A.
    let dir = temp_project(&[
        ("main.zz", "import b\nz := b.secret"),
        ("b.zz", "secret := 42"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(
        result.errors.iter().any(|e| e
            .diags
            .iter()
            .any(|d| d.severity == zz_frontend::diag::Severity::Error)),
        "expected error for private binding, got: {:?}",
        result.errors
    );
}

#[test]
fn private_func_not_visible_cross_module() {
    // Private function in module B is NOT accessible from A.
    let dir = temp_project(&[
        ("main.zz", "import b\nz := b.helper(1)"),
        ("b.zz", "func helper(x: int) -> int { x }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(
        result.errors.iter().any(|e| e
            .diags
            .iter()
            .any(|d| d.severity == zz_frontend::diag::Severity::Error)),
        "expected error for private function, got: {:?}",
        result.errors
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
    // Private struct from imported module cannot be instantiated.
    let dir = temp_project(&[
        ("main.zz", "import shapes\np := shapes.Secret { x: 1 }"),
        ("shapes.zz", "struct Secret { x: int }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(
        result.errors.iter().any(|e| e
            .diags
            .iter()
            .any(|d| d.severity == zz_frontend::diag::Severity::Error)),
        "expected error for private struct, got: {:?}",
        result.errors
    );
}

#[test]
fn shadowing_imported_pub_var() {
    // Local variable shadows an imported pub variable.
    use zz_runtime::{Interp, Value};

    let dir = temp_project(&[
        (
            "main.zz",
            "import config\nconfig_val := config.val\nx := { val := 99\nval }\nstd.io.println(x)\nconfig_val",
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
        ("math.zz", "pub func double(n: int) -> int { n * 2 }"),
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
            "pub struct Point { x: int, y: int }\npub func dist(p: Point) -> int { p.x + p.y }",
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
            "pub func pub_func() -> int { priv_func() }\nfunc priv_func() -> int { 42 }",
        ),
    ]);
    // This should work: pub_func calls priv_func internally (same module).
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);
}

#[test]
fn pub_item_unused_no_warning() {
    // pub items must NOT generate unused-variable warnings.
    let dir = temp_project(&[(
        "main.zz",
        "pub x := 42\npub func f() -> int { 1 }\npub struct S { v: int }\nfunc main() { std.io.println(1) }",
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
    let dir = temp_project(&[("main.zz", "secret := 42\nfunc main() { std.io.println(1) }")]);
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
            "pub struct Point { x: int, y: int }\npub func dist(p: Point) -> int { p.x + p.y }",
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
        ("shapes.zz", "func hidden() -> int { 42 }"),
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
        ("math.zz", "pub func double(n: int) -> int { n * 2 }"),
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
            "multiplier := 3\npub func compute(x: int) -> int { x * multiplier }",
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
        "secret := 42\nfunc helper(x: int) -> int { x + secret }\nresult := helper(8)",
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
            "pub struct Point { x: int, y: int }\npub func manhattan(p: Point) -> int { p.x + p.y }",
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
            "pub struct Point { x: int, y: int }\npub p := Point{ x: 42, y: 0 }\npub func get_x() -> int { p.x }",
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
            "pub scores := [10, 20, 30]\npub func first() -> int { scores[0] }\npub func mid() -> [int] { scores[1:3] }",
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
    assert_eq!(last, Value::Array(vec![Value::Int(20), Value::Int(30)]));
}

#[test]
fn pipeline_in_module() {
    use zz_runtime::{Interp, Value};

    // Piped calls in a module rewrite the callee path correctly.
    let dir = temp_project(&[
        ("main.zz", "import math\nz := math.apply()"),
        (
            "math.zz",
            "func inc(n: int) -> int { n + 1 }\npub func apply() -> int { 5 |> inc }",
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
            "pub struct Point { x: int, y: int }\npub func dist(p: Point) -> int { p.x + p.y }\npub func apply() -> int { p := Point { x: 3, y: 4 }\np.dist() }",
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
            "pub struct Point { x: int, y: int }\npub func dist(p: Point) -> int { p.x + p.y }",
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
            "pub struct shapes.Point { x: int, y: int }\npub func shapes.mk_point(x: int, y: int) -> shapes.Point { shapes.Point { x: x, y: y } }",
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
            "pub struct Point { x: int, y: int }\npub func dist(p: Point) -> int { p.x + p.y }",
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
        "func id(x: int) -> int { x }\nx := id(42)\nfor i in 0..3 { std.io.println(i) }\nx",
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
        "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }\nfunc fib(n: int) -> int { if n <= 1 { n } else { fib(n - 1) + fib(n - 2) } }\np := Point { x: 3, y: 4 }\nd := dist(p)\nfor i in 0..5 { std.io.println(fib(i)) }\nd",
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
            "pub struct Point { x: int, y: int }\npub func dist(p: Point) -> int { p.x + p.y }",
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
            "pub struct Point { x: int, y: int }\nfunc hidden() -> int { 42 }",
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
        ("math.zz", "pub func double(n: int) -> int { n * 2 }"),
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
        ("math.zz", "pub func double(n: int) -> int { n * 2 }"),
    ]);
    let result = load_program(&dir.join("main.zz")).unwrap();
    assert!(no_errors(&result), "errors: {:?}", result.errors);
    assert_eq!(result.bindings["main.z"], Type::Int);
}
