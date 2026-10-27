# Plan — Issue #632: no unused-var warning for unused for-loop vars

## Goal
Suppress the "unused variable" warning for for-loop index variables (they're often intentional no-op loop counters), and rely on the existing `_`-prefix exemption so `for _ in ...` is silent. Production-ready: fmt + clippy clean, tests green, PR to `dev`, CI green, merge.

## Issue body
- `crates/zz_checker/src/checker/type_check.rs:403-443` — `Stmt::For` binds loop vars via `define_at(&vars[0].name, elem, vars[0].span)` with `warn_unused: true`.
- `crates/zz_checker/src/checker/scope.rs:93-119` `emit_unused_binding_warnings` fires for any binding never read.
- `scope.rs:105` already skips `_`-prefixed names.

## Expected behavior
- Unused for-loop index variables should not be flagged (loop counters are commonly unused).
- With #636, `for _ in ...` is explicitly silent.

## Acceptance
- `cargo fmt --check` clean in `crates/zz_checker`
- `cargo clippy -p zz_checker --all-targets -- -D warnings` zero warnings
- `cargo test -p zz_checker` green
- Commit on branch `fix/issue-632`, push, open PR to `dev`
- `gh pr checks --watch` until green; fix until green; merge to `dev`

## Worktree
`/home/zaid/Projects/zz-lang-wt-632` (branch `fix/issue-632`, off `dev`)
