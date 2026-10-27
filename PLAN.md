# Plan — Issue #633: checker built-in noop / discard arm body for match .none and .err

## Goal
Allow an empty/noop match arm body (`.none => `, `.err => `) instead of requiring a statement/expression, so exhaustive matches with an intentional no-op arm don't need a throwaway `noop()`. Production-ready: fmt + clippy clean, tests green, PR to `dev`, CI green, merge.

## Issue body
- Match `.none`/`.err` arms require a payload/body; no built-in noop/discard statement exists.
- `crates/zz_checker/src/checker/type_check.rs` — `bind_pattern` (`.none`/`.err` payload rules), `warn_if_discarded`.
- `crates/zz_frontend/src/parser/expr.rs` — `parse_match` arm body uses `parse_stmt()` (empty body is a parse error).

## Expected behavior
- `.none => ` / `.err => ` (empty body) parses and checks as a noop (unit) arm.
- No false "unused/discarded" warning for an intentional empty arm.

## Acceptance
- `cargo fmt --check` clean in `crates/zz_frontend`, `crates/zz_checker`
- `cargo clippy -p zz_frontend -p zz_checker --all-targets -- -D warnings` zero warnings
- `cargo test -p zz_frontend -p zz_checker` green
- Add fixture/test proving `match x { .none => , .some(_) => ... }` compiles
- Commit on branch `fix/issue-633`, push, open PR to `dev`
- `gh pr checks --watch` until green; fix until green; merge to `dev`

## Worktree
`/home/zaid/Projects/zz-lang-wt-633` (branch `fix/issue-633`, off `dev`)
