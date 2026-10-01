# Contributing to ZZ

Thanks for helping. Keep it simple.

## Setup

```bash
git clone https://github.com/zaidejjo/zz
cd zz
cargo install --path crates/zz_cli
```

Requires stable Rust.

## Workflow

```bash
cargo test -p zz_frontend     # single crate, fast loop
./scripts/test-fast.sh        # fast suite (skips native legs + plugin e2e)
cargo test --all              # full suite (slow, CI equivalent)
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

CI enforces zero clippy warnings and clean `cargo fmt`.

## PRs

1. Fork, branch from `main`, keep the diff focused.
2. Add a `.zz` fixture under `tests/fixtures/` for user-facing changes and register it in `crates/zz_cli/tests/e2e.rs`.
3. Update docs on the website repo when behavior changes.
4. Make sure tests, clippy, and fmt pass before requesting review.

## Project layout

```
zz_frontend → zz_checker → zz_stdlib → zz_cli
                   |                       |
                   v                       v
             zz_runtime ──────────────→ zz_lsp
```

- `zz_frontend`: lexer, parser, AST, formatter.
- `zz_checker`: type checker with inference.
- `zz_runtime`: interpreter + VM.
- `zz_stdlib`: native functions (keep `funcs.rs` type signatures and `natives.rs` implementations in sync).
- `zz_cli`: `zz` binary. `zz_lsp`: `zz-lsp` binary.

## License

By contributing you agree your changes are Apache-2.0 licensed.
