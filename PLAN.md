# Plan — Issue #652: checker unreachable!() crashes

## Goal
Replace `unreachable!()` panic sites in zz_checker with graceful `RawDiag` errors so the compiler never panics on valid/malformed input. Production-ready: fmt + clippy clean, tests green, PR to `dev`, CI green, merge.

## Issue body (summary)
- `crates/zz_checker/src/checker/funcs.rs:19` — `collect_func`: `_ => unreachable!()` for non-Func statements
- `crates/zz_checker/src/checker/funcs.rs:162` — `check_func_body`: `_ => unreachable!()` for non-Func statements
- `crates/zz_checker/src/checker/structs.rs:29` — `collect_struct`: `_ => unreachable!()` for non-Struct statements
- `crates/zz_checker/src/checker/mod.rs:1341` — `func_name`: `_ => unreachable!()` for non-Func statements
- `crates/zz_checker/src/checker/type_check.rs:412` — `Stmt::For` with `vars.len() == 0`: `unreachable!()`

## Expected behavior
- Return proper `RawDiag` errors instead of `unreachable!()`.
- Compiler should never panic on valid or malformed input.

## Acceptance
- `cargo fmt --check` clean in `crates/zz_checker`
- `cargo clippy -p zz_checker --all-targets -- -D warnings` zero warnings
- `cargo test -p zz_checker` green
- Commit on branch `fix/issue-652-checker-unreachable`, push, open PR to `dev`
- `gh pr checks --watch` until green; fix until green; merge to `dev`

## Worktree
`/home/zaid/Projects/zz-lang-wt-652` (branch `fix/issue-652-checker-unreachable`, off `dev`)
