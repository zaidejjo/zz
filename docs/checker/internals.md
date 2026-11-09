# ZZ Type Checker Internals

This document describes the architecture and design of the ZZ type checker,
covering the pipeline, key data structures, and how to extend it.

## Overview

The type checker lives in `crates/zz_type_check` and is orchestrated by
`crates/zz_checker`. The pipeline is:

```
Source text → zz_frontend (lexer/parser) → zz_type_check → zz_hir → zz_ir → codegen/runtime
```

The checker itself is a tree-walking unification-based type checker with
inference, implemented as the `TypeCheckOps` trait on `Checker`.

## Crate Layout

| Crate | Responsibility |
|-------|---------------|
| `zz_span_ast` | AST node definitions (`Expr`, `Stmt`, `Ty`, `Pattern`, etc.) and the shared `AstVisitor` trait |
| `zz_types` | `Type` enum and related type representations |
| `zz_unify` | Unification algorithm (`Unifier`, `unify`, `resolve`, `fresh_var`) |
| `zz_checker_core` | `Checker` state, `FuncSig`, `TypeCheckOps` trait declarations |
| `zz_checker` | Public API: `check_program`, `check_src`, integration with loader |
| `zz_type_check` | Core checking logic: statements, expressions, patterns, calls |
| `zz_scope` | Scope management, `ScopeOps` trait (variable binding, lookup) |
| `zz_structs` | Struct field access, method resolution, `StructOps` trait |
| `zz_sigs` | Enum/signature operations, `EnumOps` trait |
| `zz_infer` | Type inference, `InferOps` trait (merge_types, instantiate, validate_bounds) |
| `zz_lints` | Lint passes including `std_gate` (import hygiene) |

## Key Data Structures

### Checker (`zz_checker_core`)

The `Checker` struct holds all mutable state during type checking:

- **Environments** (`env: Vec<Scope>`): Stack of symbol scopes for variable resolution
- **Functions** (`funcs: HashMap<String, FuncSig>`): Known function signatures
- **Structs** (`structs`: ...): Struct definitions and fields
- **Enums** (`enums`: ...): Enum definitions and variants
- **Unifier** (`unifier: Unifier`): Unification state for type inference
- **Errors** (`errors: Vec<RawDiag>`): Accumulated diagnostics
- **Loop depth** (`loop_depth: u32`): Tracks nesting for break/continue validation

### TypeCheckOps Trait

Defined in `zz_checker_core`, this trait declares all checking operations:

- `check_expr(&mut self, e: &Expr) -> Type` — expression type checking
- `check_stmt(&mut self, s: &Stmt)` — statement type checking
- `check_call(...)` — function/method call resolution
- `check_pattern(...)` — pattern matching against types
- `check_block(...)` — block statement checking

Implemented by `Checker` in `zz_type_check/src/lib.rs`.

## Module Structure (`zz_type_check/src/lib.rs`)

The checker is split into focused modules following the
[Phase 5 item 1](https://github.com/zaid/zz_lang) effort to keep functions
under ~150 lines:

| Module | Contents |
|--------|----------|
| `check_call` | Call expression handling: direct names, field callees, sqlz, print builtin, input/range, unknown suggestions, path method dispatch |
| `check_call_args` | Argument checking: count, positional slots, named args, filled slots |
| `check_expr` | Per-expression-kind type checking (Int, Str, Ident, Call, Closure, If, Match, etc.) |
| `check_stmt` | Per-statement-kind type checking (Decl, Func, If, While, For, etc.) |
| `definite_assignment` | Definite assignment analysis for variables |

Each module declares free functions taking `&mut Checker`, following the pattern
established by `definite_assignment.rs`.

## Expression Checking

`check_expr_impl` dispatches on `Expr` variants to per-kind helper functions
in `check_expr.rs`. Key patterns:

- **Literals** (`Int`, `Float`, `Str`, `Bool`): Return the corresponding `Type`
- **Identifiers**: Look up in scope via `ScopeOps::lookup`
- **Paths**: Resolve qualified names, handle enum construction (`Token.Variant`)
- **Field access** (`obj.method`): Dispatch via `check_call_field_callee`
- **Calls**: Resolve callee, check args via `check_args_against`
- **Closures**: Push scope, bind params, check body, pop scope
- **Conditionals/Loops**: Ensure boolean condition, check body in new scope
- **Match**: Check scrutinee and arms, unify arm result types

## Statement Checking

`check_stmt` dispatches on `Stmt` variants to helpers in `check_stmt.rs`:

- **Decl** (`x := expr`): Check initializer, bind in scope, check for shadowing
- **Func**: Check params, return type, body in new scope
- **If/While/For**: Check condition (bool), check body in new scope
- **Assign/CompoundAssign**: Check lvalue and rvalue types unify
- **Return**: Check return type against function signature

## Call Checking

`check_call` is the most complex function (~330 lines after extraction). It
handles:

1. **Selective-import alias resolution**: Bare `squared` from `import m(squared)` becomes `m.squared`
2. **Direct-name calls**: Qualified names like `std.math.sin`, `pg.query`
3. **Removed API table**: Renamed stdlib functions with hints
4. **Math constants**: `math.PI()` is an error (use bare `math.PI`)
5. **SQL calls**: `sqlz.query`, `sqlz.exec`, `sqlz.transaction`, `pg.*`, `mysql.*`
6. **HTTP lint**: Route/param usage checking
7. **Print builtin**: Variadic, warns about bare function values
8. **Input/range builtins**: Argument count and type checks
9. **Unknown function suggestion**: Levenshtein-based fixit hints
10. **Path method dispatch**: `obj.method()`, enum construction, variant methods
11. **Fallback callee type**: `Func`, `Named`, `Var`, `Error` types

## Unification

The `Unifier` (in `zz_unify`) implements Hindley-Milner-style unification:

- `fresh_var()`: Create a new type variable
- `unify(&mut self, a: &Type, b: &Type) -> Result<(), UnifyError>`: Unify two types
- `resolve(&self, t: &Type) -> Type`: Resolve a type variable to its binding
- `bind_var(&mut self, t: &Type, ty: Type)`: Bind a type variable

Type variables default to concrete types when unconstrained (e.g., `[]` → `Array<Var>` → `Array<Int>` when used with ints).

## Adding a New Stdlib Native

Five locksteps (all required):

1. `zz_stdlib/src/funcs.rs`: Add checker signature
2. `zz_stdlib/src/natives/`: Rust VM implementation
3. `zz_codegen_runtime/src/runtime/*.c` + `.h`: C twin (byte-identical semantics)
4. `zz_codegen_ffi/src/natives.rs`: AOT name mapping
5. Fixture + registration in `e2e.rs`, `dual_engine_parity.rs`, `batch_lists.rs`

## Lints

The `zz_lints` crate contains lint passes:

- **`std_gate`**: Import hygiene — flags `std.*` uses without matching `import`
- **`value_lint`**: Value usage checks (unused variables, etc.)

Both use the shared `AstVisitor` trait from `zz_span_ast` for AST traversal.

## Testing

- Unit tests: `zz_type_check/src/tests/`, per-crate `#[test]` modules
- Type check tests: `crates/zz_checker/tests/type_check_tests.rs` (387 tests)
- E2E tests: `crates/zz_cli/tests/e2e.rs` (snapshot fixtures)
- Parity tests: VM vs native backend strict equality
- Proptest: `crates/zz_checker/tests/proptest.rs` (parser stability, type-checker no-panic, unifier reflexivity)
- Fuzz: `fuzz/` directory with libFuzzer target

Run with:

```bash
cargo test -p zz_type_check
cargo test -p zz_checker --test type_check_tests
cargo test -p zz_cli --test e2e snapshot_error_diagnostics
cargo test -p zz_checker --test proptest
```

## Coverage

Generate a coverage report:

```bash
cargo llvm-cov --html --open
```

Requires `cargo-llvm-cov` installed (`cargo install cargo-llvm-cov`).
