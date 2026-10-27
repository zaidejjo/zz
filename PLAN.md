# Plan — Issue #635: build: per-phase build timing for zz build

## Goal
Add per-phase build timing (parse, check, lower, codegen, link) to `zz build` so users can see where time is spent. Production-ready: fmt + clippy clean, tests green, PR to `dev`, CI green, merge.

## Issue body
- `crates/zz_cli/src/build.rs` — `zz build` has no per-phase timing; only overall.
- Enhancement, compiler, ux.

## Expected behavior
- `zz build` prints time spent in each phase (parse, check, lower, codegen, link).
- Lightweight, always-on (or behind a `--time` flag).

## Acceptance
- `cargo fmt --check` clean in `crates/zz_cli`
- `cargo clippy -p zz_cli --all-targets -- -D warnings` zero warnings
- `cargo test -p zz_cli` green
- Commit on branch `fix/issue-635`, push, open PR to `dev`
- `gh pr checks --watch` until green; fix until green; merge to `dev`

## Worktree
`/home/zaid/Projects/zz-lang-wt-635` (branch `fix/issue-635`, off `dev`)
