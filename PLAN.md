# Plan — Issue #650: pure-ZZ stdlib compilation failure panics

## Goal
Replace `panic!` in pure-ZZ stdlib compilation with graceful error propagation so a single stdlib compile error doesn't crash the entire `zz` process. Production-ready: fmt + clippy clean, tests green, PR to `dev`, CI green, merge.

## Issue body
- `crates/zz_stdlib/src/zz_std.rs:90-101` — `compile_one` result handled with `Err(e) => panic!("pure-ZZ stdlib compilation failed: {e}")` inside `SLOTS[idx].get_or_init(|| { ... })`.
- Called during stdlib init; a single stdlib compile error crashes the entire process instead of surfacing a check-time diagnostic.

## Expected behavior
- Return a proper `Result`/diagnostic instead of panicking.
- Caller handles the error gracefully and reports it as a compiler diagnostic.

## Acceptance
- `cargo fmt --check` clean in `crates/zz_stdlib`
- `cargo clippy -p zz_stdlib --all-targets -- -D warnings` zero warnings
- `cargo test -p zz_stdlib` green
- Commit on branch `fix/issue-650`, push, open PR to `dev`
- `gh pr checks --watch` until green; fix until green; merge to `dev`

## Worktree
`/home/zaid/Projects/zz-lang-wt-650` (branch `fix/issue-650`, off `dev`)
