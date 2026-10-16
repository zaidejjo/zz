# Compiler architecture

Linear pipeline:

```
zz_frontend → zz_checker → zz_hir → zz_ir → { zz_runtime ↔ zz_codegen } → zz_cli
                               zz_stdlib ─┘                ↓
              zz_arena, zz_fmt, zz_native_rt, zz_plugin, zz_pm, zz_lsp
```

| Crate | Role |
|---|---|
| `zz_frontend` | lexer, parser, AST, lossless formatter, diagnostics (spans, Levenshtein suggestions, fix-its) |
| `zz_checker` | unification type checker with inference |
| `zz_hir` / `zz_ir` | typed HIR, portable IR (codec + disassembler), callgraph + DCE |
| `zz_runtime` | tree-walker interpreter + bytecode VM + C-ABI bridge (`c_abi.rs`) |
| `zz_codegen` | AOT: HIR→C (`lower/`) + chunk→C (`chunk.rs`), Clang driver, PGO, cross targets |
| `zz_stdlib` | 900+ natives: VM registry + C twins + Rust staticlib (`ffi.rs`) |
| `zz_cli` (`zz`) | run/build/test/check/fix/fmt, REPL, loader + module cache, doctor, upgrade |
| `zz_lsp` / `zz-dap` | editor server, debug adapter |
| `zz_pm` | registry/git/path deps, publish |
| `zz_plugin`, `zz_fmt`, `zz_arena`, `zz_native_rt` | plugin ABI, formatter, arenas, shared native impls |

## Adding a stdlib native — five locksteps

Miss one and CI fails:

1. `zz_stdlib/src/funcs.rs` — checker signature (+ count assertion).
2. `zz_stdlib/src/natives/` — Rust VM impl.
3. `zz_codegen/src/runtime/*.c` + `.h` — C twin, byte-identical semantics.
4. `zz_codegen/src/lower/mod.rs` (`native_impl`) — AOT name mapping.
5. Fixture `tests/fixtures/stdlib/<name>_test.zz` + `e2e.rs` + `dual_engine_parity.rs`
   (+ `batch_lists.rs` or `EXCLUDED` with reason).

Contracts for search/byte natives: byte offsets O(1), backend-identical,
matching Rust `str::find` edge semantics.

New compiler work needs `cargo install --path crates/zz_cli` before dogfooding.
Branches off `dev` (`feat/...`, `fix/...`), `cargo fmt` + zero clippy + green tests,
PR to `dev` (never push to `dev`/`main` directly).

Specs: [IR draft](../ir-spec-draft.md), [VM CoW](../design/vm-cow.md).
Checker detail: [Checker](checker.md). Backends: [Runtime and codegen](runtime-codegen.md).
