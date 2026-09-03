# AGENTS.md — ZZ Language Repository

## Build & Test Commands

```bash
# Install the CLI
cargo install --path crates/zz_cli

# Run all tests (CI equivalent)
cargo test --all

# Run tests for a single crate
cargo test -p zz_frontend
cargo test -p zz_checker
cargo test -p zz_cli

# Lint (CI enforces zero warnings)
cargo clippy --all-targets -- -D warnings

# Format check (CI enforces)
cargo fmt --check
```

## Architecture

6 crates in a linear pipeline:

```
zz_frontend → zz_checker → zz_stdlib → zz_cli
                 |                       |
                 v                       v
           zz_runtime ──────────────→ zz_lsp
```

- **zz_frontend**: Lexer, parser, AST, lossless formatter, diagnostics
- **zz_checker**: Unification-based type checker with inference
- **zz_runtime**: Tree-walker interpreter + bytecode VM
- **zz_stdlib**: 70+ native functions (two registries: `stdlib_funcs` for checker, `stdlib_natives` for runtime)
- **zz_cli**: Binary `zz` — REPL, run, check, fix, fmt
- **zz_lsp**: Language server binary `zz-lsp`

Binary entrypoints:
- `crates/zz_cli/src/main.rs` → `zz` binary
- `crates/zz_lsp/src/main.rs` → `zz-lsp` binary

## Testing Conventions

### Unit tests
- `zz_frontend/src/tests/` — parser, lexer, AST tests
- `zz_checker/tests/type_check_tests.rs` — 900+ type-check tests using `check_src()` helper

### End-to-end tests (`crates/zz_cli/tests/e2e.rs`)
- Discovers `.zz` fixture files under `tests/fixtures/`
- **Success fixtures** (`syntax/`, `types/`, `stdlib/`): must exit 0, last stdout line contains success marker
- **Error fixtures** (`errors/`): must exit 1, stderr contains "error"
- Each fixture must print a final line with a success marker (e.g., `declarations_ok`)
- Tests use `e2e_success_test!` and `e2e_error_test!` macros — register new fixtures in `e2e.rs`

### Fixture structure
```
tests/fixtures/
├── syntax/     # Parser/syntax features (23 fixtures)
├── types/      # Type system features (4 fixtures)
├── stdlib/     # Standard library tests (19 fixtures)
└── errors/     # Expected compile/runtime errors
```

## Key Patterns

### Stdlib registration (lockstep)
Two registries must be kept in sync when adding stdlib functions:
1. `zz_stdlib/src/funcs.rs` — type signatures for the checker
2. `zz_stdlib/src/natives.rs` — Rust implementations for the runtime

Module namespaces: `import std.io` copies `std.io.*` entries to `io.*` via `register_module_namespace()`.

### Known stdlib modules
`io`, `str`, `vec`, `json`, `http`, `fs`, `env`, `math`, `time`, `encoding`, `net`

### Diagnostics
- Spans everywhere — all errors are spanned
- Levenshtein suggestions for typos
- Fix-it hints with safety levels (Safe / Ambiguous)
- `codespan-reporting` for rendering

## CI - ITS NOW IN TODO NOT NOW

`.github/workflows/ci.yml` runs on push to main and all PRs:
1. `cargo fmt --check`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo test --all`

Matrix: ubuntu-latest, macos-latest, windows-latest.

## Gotchas

- `examples/` and `plans/` are gitignored
- `--fix` mode has three safety levels: auto (safe only), `--hard` (all), `-i` (interactive)
- The `zz` binary name collides with nothing in the workspace — safe to install globally
