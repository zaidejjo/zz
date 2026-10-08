# AGENTS.md — ZZ Language Repository

The ZZ programming language toolchain: a compiled language with a
tree-walker interpreter, bytecode VM, and AOT backends (direct C + chunk→C),
a growing async stdlib, LSP, package manager, and plugin system.

## Quick Start

```bash
cargo install --path crates/zz_cli   # install the `zz` binary
zz run main.zz                        # run (VM)
zz run --native main.zz               # run (AOT-compiled)
zz test                               # Zhunit-style tests in .zz files
```

## Build, Test, Lint (CI gates all three)

```bash
cargo test --all                                   # full CI equivalent (slow: native legs + plugins)
./scripts/test-fast.sh                             # fast loop (VM legs only, skips natives + plugin_e2e)
ZZ_PARITY_VM_ONLY=1 cargo test -p zz_cli --test dual_engine_parity
cargo test -p zz_frontend | -p zz_checker | -p zz_cli | -p zz_runtime | -p zz_stdlib | -p zz_codegen
cargo clippy --all-targets -- -D warnings          # zero warnings enforced
cargo fmt --check                                  # rustfmt enforced (run `cargo fmt` first)
```

Long commands (>2 min: full parity, `--all`): run in background and
continue working; never block polling. Never prefix `timeout`.

## Architecture (linear pipeline)

```
zz_frontend → zz_checker → zz_hir → zz_ir → { zz_runtime ↔ zz_codegen } → zz_cli
                                  zz_stdlib ─┘                ↓
                          zz_arena, zz_fmt, zz_native_rt, zz_plugin, zz_pm, zz_lsp
```

- **zz_frontend**: lexer, parser, AST, lossless formatter, diagnostics
- **zz_checker**: unification type checker with inference (spans everywhere,
  Levenshtein typo suggestions, fix-it hints with safety levels)
- **zz_hir / zz_ir**: typed HIR, portable IR (codec + disassembler), callgraph + DCE
- **zz_runtime**: tree-walker interpreter + bytecode VM + C-ABI bridge (`c_abi.rs`)
- **zz_codegen**: AOT backends — HIR→C (`lower/`) and chunk→C (`chunk.rs`) + Clang driver, PGO, cross targets
- **zz_stdlib**: 900+ natives (VM registry + C twins + Rust staticlib via `ffi.rs`)
- **zz_cli** (`zz`): run/build/test/check/fix/fmt, REPL, loader with module cache, doctor, upgrade
- **zz_lsp** (`zz-lsp`), **zz_pm** (registry), **zz_plugin**, **zz_fmt**, **zz_arena**

## How Work Lands (no exceptions)

1. New branch off `dev` (`feat/...`, `fix/...`, `chore/...`).
2. Small reviewable commits, present tense (`fix(codegen): ...`).
3. `cargo fmt`, zero clippy warnings, relevant tests green **locally**.
4. PR **to `dev`** → green CI → merge. Never push to `dev`/`main`
   directly. `main` advances via `dev`→`main` PRs only (releases cut from `main`).

## Adding a Stdlib Native (five locksteps — miss one and CI fails)

1. `zz_stdlib/src/funcs.rs` — checker signature (+ count assertion).
2. `zz_stdlib/src/natives/` — Rust VM implementation.
3. `zz_codegen/src/runtime/*.c` + `.h` — C twin (byte-identical semantics).
4. `zz_codegen/src/lower/mod.rs` (`native_impl`) — AOT name mapping.
5. Fixture `tests/fixtures/stdlib/<name>_test.zz` + registration in
   `e2e.rs` + `dual_engine_parity.rs` (+ `batch_lists.rs`, or `EXCLUDED`
   with reason).

Contracts for search/byte natives: byte offsets (O(1)), backend-identical,
matching Rust `str::find` edge semantics. New compiler work needs
`cargo install --path crates/zz_cli` before dogfooding.

## Testing Conventions

- Unit: `zz_frontend/src/tests/`, `zz_checker/tests/type_check_tests.rs`
  (`check_src()` helper), per-crate `#[test]` modules.
- E2E (`zz_cli/tests/e2e.rs`): fixture must exit 0 with a non-empty last
  line (success fixtures) or exit 1 with "error" on stderr (errors/).
- Parity (`dual_engine_parity.rs`): VM-vs-native strict equality;
  `parity_strict!(...)` per fixture; the sweep (`parity_discover_all_fixtures`)
  fails on any unexpected result — fix the code or, for genuine gaps, file
  an issue and list in `known_native_failures()` (the sweep also shouts
  `FIXED!` when a listing goes stale — remove it the same day).
- Batched parity (`batched_parity.rs` + `batch_lists.rs`): every strict
  fixture must be batched or `EXCLUDED` with reason (inventory test enforces).
- Tests must be order-independent: libtest runs in parallel — never rely
  on another test's global registration; re-register idempotently.

## Shell Gotchas (this repo bites here)

- zsh: never `echo ===`, never bare `==` in commands (glob/parse errors).
- `zz test <file>` resolves imports relative to the importing file only.
- `zz check <dir>` module blind spot: N errors usually share one root cause.
- Arrays/structs are values (mutating a param mutates a copy).
- `zz` binary name is globally safe; AUR/prebuilt zips need no sources,
  but native builds outside a checkout need `ZZ_NATIVE_RT_DIR`.

## Work Preservation (absolute)

1. Commit every completed unit immediately; never end work dirty.
2. NEVER `reset --hard` / `checkout -- .` / `clean -fd` on a non-clean
   tree (`git status --porcelain` first). Stash (named) to switch context.
3. User branches are read-only: never commit to, reset, or delete them.
4. End of session: tree clean, everything committed and pushed.

## Releases

Version lives in workspace `Cargo.toml`; releases cut from `main` via
`.github/workflows/release.yml` (per-arch zips incl. `lib/` native
runtime). Downstream (`zcc` pin in `packaging/zz.version`, AUR) follows
the published tag — never point at moving branches.
