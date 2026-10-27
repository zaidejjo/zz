# Plan — Issue #639: import fixit fires when std module root partially resolves

## Goal
Make the import fixit fire when a std module root partially resolves (so `use std::...` suggestions work for partially-resolved std paths). Production-ready: fmt + clippy clean, tests green, PR to `dev`, CI green, merge.

## Issue body
- `crates/zz_checker/src/checker/` — import fixit doesn't fire when std module root partially resolves.

## Acceptance
- `cargo fmt --check` clean in `crates/zz_checker`
- `cargo clippy -p zz_checker --all-targets -- -D warnings` zero warnings
- `cargo test -p zz_checker` green
- Commit on branch `fix/issue-639`, push, open PR to `dev`
- `gh pr checks --watch` until green; fix until green; merge to `dev`

## Worktree
`/home/zaid/Projects/zz-lang-wt-639` (branch `fix/issue-639`, off `dev`)
