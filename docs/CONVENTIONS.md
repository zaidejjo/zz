# Docs conventions — how we write and verify

Read this before adding or editing any page under `docs/`.

## One topic per file

- Filename is the slug: `language/operators.md`, `stdlib/http.md`.
- Title is `# Name — scope`. First paragraph states who it is for and what it unlocks.
- End every learning page with `Next:` links. End every reference with `Full tables:` link.

## Examples must run

- `zz` fences are complete programs. Prefer `zz run file.zz` shape.
- `//` comments show `println` output. Keep one interpolated string per line for diffs.
- Complete bodies only: no stubs, no ellipsis bodies. If behavior differs VM vs AOT, say so
  explicitly and link [Parity](guides/parity.md).
- New stdlib names must exist in `crates/zz_stdlib/src/funcs.rs`.
  New CLI flags must exist in `crates/zz_cli/src/main.rs` dispatch.

## Links

- Relative only. From `docs/<folder>/page.md`, legacy refs are `../syntax.md`,
  sibling folders are `../tooling/cli.md`, same folder is `page.md`.
- From `docs/README.md`, everything is `folder/page.md` or `syntax.md` (no `../`).
- Run `python3 scripts/check-docs.py` before pushing: missing links fail,
  `../../` escapes fail, uncovered `std.*` modules warn.

## Tables

- API tables: `| Function | Description |` with canonical `std.mod.fn` names.
- CLI tables: `| Flag | Meaning |` with defaults in parens.
- Operator tables: precedence tightest-first with associativity.

## What goes where

- New pages are canonical: `getting-started/`, `language/`, `stdlib/`, `tooling/`,
  `compiler/`, `guides/` (+ generated `stdlib/reference.md` for signatures).
- Legacy flat files (`syntax.md`, `types.md`, `stdlib.md`, `cli.md`, `testing.md`,
  `bench.md`, `threading.md`) are deep references kept for history and code links;
  each carries a banner pointing at its canonical replacement. On conflict, the
  new page + `funcs.rs` win — file a fix, don't fork the docs.
- Internals: `compiler/`, `design/`, `perf/`. Never document an optimization as a
  semantic promise — see [Value semantics](language/value-semantics.md).

## Verification

```bash
python3 scripts/check-docs.py        # links + stdlib coverage + fence sanity
zz check --stats src/main.zz         # example sanity for checker claims
cargo fmt --check                    # Rust still clean (docs-only = no-op)
```
