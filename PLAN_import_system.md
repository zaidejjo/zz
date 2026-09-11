# Plan: Implement Enhanced Import System for zz

## Summary

The basic import system (`import std.math`, `import std.math as m`, `pub import`) already works. This plan adds **selective imports**, **wildcard imports**, and **symbol aliases** — the 4 new syntaxes from the spec.

## Current State

| Feature | Status |
|---------|--------|
| `import std.io` | ✅ works |
| `import std.io as console` | ✅ works |
| `pub import` | ✅ works |
| `import local_file` | ✅ works |
| Circular import detection | ✅ works |
| `import std.math(PI, sin)` | ❌ NOT implemented |
| `import std.math(*)` | ❌ NOT implemented |
| `import std.math(PI as pi)` | ❌ NOT implemented |

---

## Implementation Steps

### Step 1: Extend AST — `crates/zz_frontend/src/ast/stmt.rs`

Add `ImportItem` struct and extend `Stmt::Import`:

```rust
/// A single item in a selective import list: `PI` or `PI as pi` or `*`.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportItem {
    /// Wildcard: `*`
    Wildcard { span: Span },
    /// Named import: `name` or `name as alias`
    Named {
        name: String,
        alias: Option<String>,
        span: Span,
    },
}
```

Extend `Stmt::Import`:
```rust
Import {
    path: Vec<String>,
    alias: Option<String>,
    items: Vec<ImportItem>,  // NEW: empty = full import, wildcard = *, named = selective
    span: Span,
    pub_: bool,
},
```

### Step 2: Update Parser — `crates/zz_frontend/src/parser/stmt.rs`

Update `parse_import()` (line 338-361) to handle optional `(items)` after the path:

Grammar:
```
import_stmt := 'import' path ('as' IDENT)? ('(' import_items ')')?
import_items := '*' (',' import_item)*
              | import_item (',' import_item)*
import_item  := IDENT ('as' IDENT)?
```

After parsing path and optional module alias, check for `LParen`:
- If `LParen` found: parse import items list
- `*` → `ImportItem::Wildcard`
- `IDENT` → `ImportItem::Named { name, alias: None }`
- `IDENT as IDENT` → `ImportItem::Named { name, alias: Some(...) }`

### Step 3: Add Stdlib Selective Copy — `crates/zz_stdlib/src/lib.rs`

Add new public function:
```rust
pub fn register_selective_namespace(
    module: &str,
    items: &[(String, Option<String>)],  // (original_name, alias)
    funcs: &mut HashMap<String, FuncSig>,
    natives: &mut HashMap<String, NativeEntry>,
) -> Result<Vec<String>, String>  // returns list of symbols not found
```

This iterates `stdlib_funcs()` and `stdlib_natives()`, copying only entries matching `std.<module>.<name>` to `<alias_or_name>` in both registries.

### Step 4: Update Loader — `crates/zz_cli/src/loader/mod.rs`

**4a. Store selective import metadata** — add field to `Loader`:
```rust
selective_imports: Vec<(PathBuf, Vec<String>, Vec<ImportItem>)>,
// (importing_file_path, module_path, items)
```

**4b. In `load_file()` (line 207-215)** — extract `items` from `Stmt::Import`:
```rust
let imports: Vec<(Vec<String>, Option<String>, Vec<ImportItem>)> = parsed
    .program.stmts.iter()
    .filter_map(|s| match s {
        Stmt::Import { path, alias, items, .. } => Some((path.clone(), alias.clone(), items.clone())),
        _ => None,
    })
    .collect();
```

For stdlib with items: validate module, store in `selective_imports`, skip full namespace registration.
For stdlib without items: existing behavior (full namespace).
For local with items: load file normally, store in `selective_imports`.
For local without items: existing behavior.

**4c. In `finish()` (after line 535)** — process selective imports:
```rust
// For each module in order:
for (imp_path, module_path, items) in &self.selective_imports {
    if module_path.first().map(String::as_str) == Some("std") {
        // Stdlib selective import
        let module = &module_path[1];
        let name_aliases: Vec<(String, Option<String>)> = items.iter().map(|item| match item {
            ImportItem::Named { name, alias, .. } => (name.clone(), alias.clone()),
            ImportItem::Wildcard { .. } => unreachable!(), // handled separately
        }).collect();
        match register_selective_namespace(module, &name_aliases, &mut self.funcs, &mut self.natives) {
            Ok(missing) => { /* emit errors for missing symbols */ }
            Err(e) => { /* emit error */ }
        }
    } else {
        // Local file selective import — items already in seed as ns.name
        let ns = module_path.last().map(String::as_str).unwrap_or("");
        let prefix = format!("{ns}.");
        for item in items {
            match item {
                ImportItem::Named { name, alias, .. } => {
                    let target = alias.as_ref().unwrap_or(name);
                    // Copy from seed ns.name → target
                    let full = format!("{prefix}{name}");
                    if let Some(sig) = self.funcs.get(&full).cloned() {
                        self.funcs.insert(target.clone(), sig);
                    }
                    if let Some(entry) = self.natives.get(&full).cloned() {
                        self.natives.insert(target.clone(), entry);
                    }
                    if let Some(ty) = self.bindings.get(&full).cloned() {
                        self.bindings.insert(target.clone(), ty);
                    }
                    if let Some(sig) = self.structs.get(&full).cloned() {
                        self.structs.insert(target.clone(), sig);
                    }
                }
                ImportItem::Wildcard { .. } => {
                    // Copy ALL pub items from ns.* → bare names
                    let keys: Vec<String> = self.funcs.keys()
                        .filter(|k| k.starts_with(&prefix))
                        .cloned()
                        .collect();
                    for key in keys {
                        let bare = key[prefix.len()..].to_string();
                        if let Some(sig) = self.funcs.get(&key).cloned() {
                            self.funcs.insert(bare.clone(), sig);
                        }
                        if let Some(entry) = self.natives.get(&key).cloned() {
                            self.natives.insert(bare.clone(), entry);
                        }
                    }
                    // Also bindings and structs...
                }
            }
        }
    }
}
```

### Step 5: Update Checker Import Handling — `crates/zz_checker/src/checker/type_check.rs`

Update `Stmt::Import` match (line 76-89) to handle new `items` field:
- Full import: same as before (push namespace to `self.imports`)
- Selective import: push each imported name to `self.imports`
- Wildcard import: push namespace with wildcard marker

### Step 6: Update Formatter — `crates/zz_fmt/src/ir.rs`

Update the `Stmt::Import` arm (line 297-318) to emit items:
```rust
if !items.is_empty() {
    self.text("(");
    for (i, item) in items.iter().enumerate() {
        if i > 0 { self.text(","); self.space(); }
        match item {
            ImportItem::Wildcard { .. } => self.text("*"),
            ImportItem::Named { name, alias, .. } => {
                self.text(name);
                if let Some(a) = alias {
                    self.space(); self.text("as"); self.space(); self.text(a);
                }
            }
        }
    }
    self.text(")");
}
```

Also update `crates/zz_fmt/src/verify.rs` to handle `items` field.

### Step 7: E2E Test Fixtures

Create test files under `tests/fixtures/modules/`:

**`tests/fixtures/stdlib/selective_import.zz`** — Test 2: selective import
```zz
import std.math(PI, sin)
assert(PI() > 3.14)
assert(sin(0) == 0)
println("selective_import_ok")
```

**`tests/fixtures/stdlib/wildcard_import.zz`** — Test 3: wildcard import
```zz
import std.math(*)
assert(PI() > 3.14)
assert(cos(0) == 1)
println("wildcard_import_ok")
```

**`tests/fixtures/stdlib/symbol_alias.zz`** — Test 5: symbol alias
```zz
import std.math(PI as pi)
assert(pi() > 3.14)
println("symbol_alias_ok")
```

**`tests/fixtures/stdlib/multi_selective.zz`** — Test 6: multiple imports
```zz
import std.math(PI, sqrt)
import std.json(parse)
assert(PI() > 3.14)
println("multi_selective_ok")
```

Register new tests in `crates/zz_cli/tests/e2e.rs`.

### Step 8: Error Cases

Add error test fixtures:

**`tests/fixtures/errors/import_private.zz`** — Try importing private symbol
**`tests/fixtures/errors/import_not_found.zz`** — Try importing nonexistent symbol

---

## Key Design Decisions

1. **Selective imports work at the loader level** — not in the checker. The loader copies specific symbols from the dependency's seed into the importing module's seed as bare names.

2. **Wildcard imports copy ALL pub items** — iterates all seed entries matching the module prefix and copies them without prefix.

3. **`as` aliases** — the target name is used as the key in the importing module's seed, not the original name.

4. **Conflict detection** — if a name already exists in the seed when copying, emit an error suggesting `as` to resolve.

5. **`pub` enforcement** — for local files, only `pub` items are in the cross-module seed, so private items are naturally unavailable. For stdlib, all items are effectively pub.

6. **No changes to `namespace_program()`** — the rewriting runs as before. Selective imports only affect what's in the importing module's seed.

---

## Files Modified

| File | Change |
|------|--------|
| `crates/zz_frontend/src/ast/stmt.rs` | Add `ImportItem`, extend `Stmt::Import` |
| `crates/zz_frontend/src/parser/stmt.rs` | Update `parse_import()` |
| `crates/zz_stdlib/src/lib.rs` | Add `register_selective_namespace()` |
| `crates/zz_cli/src/loader/mod.rs` | Handle items in load/finish |
| `crates/zz_checker/src/checker/type_check.rs` | Update import match arm |
| `crates/zz_fmt/src/ir.rs` | Format new import syntax |
| `crates/zz_fmt/src/verify.rs` | Update import verification |
| `crates/zz_cli/tests/e2e.rs` | Register new test fixtures |
| `tests/fixtures/stdlib/` | New test fixture files |
| `tests/fixtures/errors/` | New error test fixtures |

---

## Verification

1. `cargo fmt --check` — formatting passes
2. `cargo clippy --all-targets -- -D warnings` — no warnings
3. `cargo test --all` — all existing + new tests pass
4. Manual: `echo 'import std.math(PI, sin); println(PI())' | cargo run -- eval -` — selective import works
5. Manual: `echo 'import std.math(*); println(sin(0))' | cargo run -- eval -` — wildcard works
