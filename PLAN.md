# Plan — Issue #653: codegen unreachable!() crashes

## Goal
Replace `unreachable!()` panic sites in zz_codegen with graceful error propagation so the compiler never panics on valid/malformed IR. Production-ready: fmt + clippy clean, tests green, PR to `dev`, CI green, merge.

## Issue body (summary)
- `crates/zz_codegen/src/chunk.rs:3756` — `_ => unreachable!()` in BinOp fast-path match
- `crates/zz_codegen/src/chunk.rs:3778` — `UnOp::Pos => unreachable!()` (defensive panic-risk)
- `crates/zz_codegen/src/lower/expr.rs:729` — `_ => unreachable!()` when both scalar operands are `Some` but pattern doesn't match
- `crates/zz_codegen/src/lower/expr.rs:768` — same for `double` operands

## Expected behavior
- Replace `unreachable!()` with proper error propagation — return a diagnostic/fallback instead of panicking.
- Compiler should never panic on valid IR.

## Acceptance
- `cargo fmt --check` clean in `crates/zz_codegen`
- `cargo clippy -p zz_codegen --all-targets -- -D warnings` zero warnings
- `cargo test -p zz_codegen` green
- Commit on branch `fix/issue-653-codegen-unreachable`, push, open PR to `dev`
- `gh pr checks --watch` until green; fix until green; merge to `dev`

## Worktree
`/home/zaid/Projects/zz-lang-wt-653` (branch `fix/issue-653-codegen-unreachable`, off `dev`)
