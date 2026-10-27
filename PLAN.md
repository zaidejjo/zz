# Plan — Issue #649: VM Box::leak memory leak

## Goal
Eliminate `Box::leak` memory leak in `Value::namespace()` / display logic. Use interning, static string pool, or owned `String` returns instead of `Box::leak`. Must be production-ready: fmt + clippy clean, tests green, PR to `dev`, CI green, merge.

## Issue (#649)
VM leaks memory permanently via `Box::leak`:
- `crates/zz_runtime/src/value.rs:1487` — `Value::Opaque(h) => Some(Box::leak(h.tag.clone().into_boxed_str()) as &str)` — leaks one string per distinct opaque tag
- `crates/zz_runtime/src/value.rs:1496` — `o.name.rsplit_once('.').map(|(ns, _)| Box::leak(ns.to_string().into_boxed_str()) as &str)` — leaks one string per distinct object namespace

## Expected behavior
- No permanent memory leaks. Use interning, static string pool, or owned `String` returns instead of `Box::leak`.
- Long-running programs should not grow memory unboundedly from type tags or namespace names.

## Acceptance criteria
- `cargo fmt --check` clean in `crates/zz_runtime`
- `cargo clippy -p zz_runtime --all-targets -- -D warnings` zero warnings
- `cargo test -p zz_runtime` green
- Commit on branch `fix/issue-649-vm-leak`, push, open PR to `dev`
- `gh pr checks --watch <PR_URL>` until CI green; fix until green; merge to `dev`

## Worktree
`/home/zaid/Projects/zz-wt-649` (branch `fix/issue-649-vm-leak`, off `dev`)
