//! Seed checker signatures from a document's own imports (#256, #257).
//!
//! The real loader (`zz_cli::loader`) copies `std.*` signatures into the
//! check seed before type-checking: full imports under their namespace
//! (`import std.math` → `math.*`), selective imports as bare names
//! (`import std.math(PI)` → `PI`), wildcards likewise. The LSP used to
//! skip that step and check with raw `stdlib_funcs()` (`std.*` keys only),
//! so valid selective files reported both `undefined variable 'PI'` and
//! `unused import 'PI'`, and `math.` member completion found zero keys.
//!
//! This module mirrors the loader's std-import seeding for the funcs table
//! only (the LSP has no runtime natives to populate). Unknown modules are
//! skipped — the checker/loader surface those errors through their own
//! paths.

use std::collections::HashMap;

use zz_checker::FuncSig;
use zz_frontend::ast::{ImportItem, Program, Stmt};
use zz_stdlib::{
    register_module_namespace, register_selective_namespace, register_wildcard_namespace,
};

/// Copy import-visible signatures into a checker seed.
///
/// `base` is the global seed (stdlib `std.*` keys plus accumulated workspace
/// defs).
///
/// Returns the seed extended with this program's own imports:
///
/// - `import std.math` (or `as m`) → `math.*` (or `m.*`) copies.
/// - `import std.math(PI, sin as s)` → bare `PI`, `s`.
/// - `import std.math(*)` → all bare members.
///
/// Non-`std` imports are left alone (workspace defs arrive via the seed).
pub fn seeded_funcs_for_program(
    program: &Program,
    base: &HashMap<String, FuncSig>,
) -> HashMap<String, FuncSig> {
    let mut funcs = base.clone();
    // The register_* helpers also populate a natives table the LSP never
    // reads; feed them a throwaway map so funcs stay loader-identical.
    let mut dummy_natives: HashMap<String, zz_runtime::NativeEntry> = HashMap::new();
    for stmt in &program.stmts {
        let Stmt::Import {
            path, alias, items, ..
        } = stmt
        else {
            continue;
        };
        if path.first().map(String::as_str) != Some("std") || path.len() < 2 {
            continue;
        }
        let module = path[1..].join(".");
        if items.is_empty() {
            let ns = alias
                .clone()
                .unwrap_or_else(|| path.last().cloned().unwrap_or_else(|| module.clone()));
            let _ = register_module_namespace(&module, &ns, &mut funcs, &mut dummy_natives);
        } else if items
            .iter()
            .any(|i| matches!(i, ImportItem::Wildcard { .. }))
        {
            let _ = register_wildcard_namespace(&module, &mut funcs, &mut dummy_natives);
        } else {
            let name_aliases: Vec<(String, Option<String>)> = items
                .iter()
                .filter_map(|i| match i {
                    ImportItem::Named { name, alias, .. } => Some((name.clone(), alias.clone())),
                    ImportItem::Wildcard { .. } => None,
                })
                .collect();
            let _ = register_selective_namespace(
                &module,
                &name_aliases,
                &mut funcs,
                &mut dummy_natives,
            );
        }
    }
    funcs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> HashMap<String, FuncSig> {
        zz_stdlib::stdlib_funcs()
    }

    fn parse(src: &str) -> Program {
        zz_frontend::parse(src).program
    }

    #[test]
    fn full_import_seeds_namespace() {
        let program = parse("import std.math\n");
        let funcs = seeded_funcs_for_program(&program, &base());
        assert!(funcs.contains_key("math.abs"), "math.* should be seeded");
        assert!(funcs.contains_key("std.math.abs"), "std.* keys stay");
    }

    #[test]
    fn aliased_namespace_seeds_under_alias() {
        let program = parse("import std.math as m\n");
        let funcs = seeded_funcs_for_program(&program, &base());
        assert!(funcs.contains_key("m.abs"), "alias ns should be seeded");
    }

    #[test]
    fn selective_import_seeds_bare_names() {
        let program = parse("import std.math(PI)\n");
        let funcs = seeded_funcs_for_program(&program, &base());
        assert!(
            funcs.contains_key("PI"),
            "selective const should seed bare name"
        );
    }

    #[test]
    fn selective_alias_seeds_target() {
        let program = parse("import std.math(PI as pi)\n");
        let funcs = seeded_funcs_for_program(&program, &base());
        assert!(funcs.contains_key("pi"), "aliased const seeds target");
        assert!(
            !funcs.contains_key("PI"),
            "original spelling stays unseeded"
        );
    }

    #[test]
    fn wildcard_seeds_bare_members() {
        let program = parse("import std.math(*)\n");
        let funcs = seeded_funcs_for_program(&program, &base());
        assert!(funcs.contains_key("abs"), "wildcard seeds bare members");
        assert!(funcs.contains_key("PI"), "wildcard seeds bare consts");
    }

    #[test]
    fn unknown_module_is_skipped() {
        let program = parse("import std.nope\n");
        let funcs = seeded_funcs_for_program(&program, &base());
        assert_eq!(funcs.len(), base().len(), "unknown modules change nothing");
    }
}
