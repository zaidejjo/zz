# Examples index — curated map (examples/ is gitignored scratch)

`examples/` is local scratch (`/.gitignore` → `/examples`) holding 79 files:
curated recipes, benchmarks, and legacy regression snapshots (`bk_*`,
`thing.zz`, `play.zz`, historical spellings `packges/`, `performace_check/`).
`e2e.rs` / `dual_engine_parity.rs` reference those paths, so renames need test
updates. Use this map; do not browse blind.

## Start here (curated, referenced from docs)

- `server.zz`, `users-web.zz`, `v2-users-web.zz`, `http_static.zz`, `static_serve.zz` → [HTTP](../stdlib/http.md)
- `parallel_worker_pool.zz` → [Concurrency](../stdlib/concurrency.md)
- `regexp_email_demo/extract_emails.zz` → [Regexp](../stdlib/regexp.md)
- `sqlz_test.zz`, `sqlz_universal.zz` → [Databases](../stdlib/sql-db.md)
- `test_framework_showcase/` → [Test helpers](../stdlib/testing-lib.md)
- `structs.zz`, `family.zz`, `impl-test.zz` → [Structs](../language/structs-enums.md)
- `str_advanced_example.zz`, `test-fstrings.zz`, `multi_string.zz` → [Strings](../language/strings.md)
- `projects/todo-app/main.zz` → full app (routes + JSON + static)
- `zimg_demo/`, `cli/`, `mgr/` → project layouts (`zz.toml` + `src/main.zz`)

## Scratch / history (do not copy blindly)

- `play.zz`, `thing.zz`, `demo.zz`, `test-*.zz`, `bk_*.zz` — experiments and snapshots.
- `packges/`, `performace_check/` — historical spellings; tests reference the paths.
- `errors_diag/diag-*.zz` — one error shape each.

## Rule

New tracked examples must be referenced from `docs/` (cookbook or module guide)
or from `crates/zz_cli/tests/`. Unreferenced scratch stays local-only.
See [Conventions](../CONVENTIONS.md).
