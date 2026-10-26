# Packages — `zz.toml`, deps, publish

## Manifest

```toml
[package]
name = "my-app"
version = "0.1.0"
entry = "src/main.zz"   # optional override, must stay inside package

[dependencies]
toml = "1.2.3"                    # registry
utils = { git = "https://...", rev = "abc123" }
local = { path = "../local" }

[test]
jobs = 4

[collect.commands]        # directory-based typed registry
dir = "commands"
sig = "func(args: [str]) -> int"
```

See [Collections](collections.md) for member discovery, ordering, and
diagnostics.

Same-name deps across workspace members stay unsupported until `[workspace]` —
keep names unique per workspace.

## Commands

```bash
zz init [name]        # scaffold
zz new <name>        # new project
zz add <dep>         # add dep
zz install           # fetch + link vendor/, rewrite zz.lock
zz install --frozen  # CI: fail if lock stale (use in CI)
zz remove <dep>
zz update
zz outdated
zz deps
zz audit
zz search <q>
zz info <pkg>
zz registry / login / publish
zz generate-wrapper
zz clean --deps      # drop vendor/ + lock, re-fetch with install
```

A dep name has exactly one source: manifest wins. Stale lock pins from another
source are ignored with `note: <name>: manifest declares <source>, ignoring stale
<source> pin`; `install` rewrites lock + `vendor/` links, so registry↔path flips
need no manual cleanup. Fresh clones (lock matches, `vendor/` missing) fetch+link
instead of `up to date`. Long-form docs bundle `DOCS.md` else `docs/**/*.md`
(max 200KB, sorted `#`-level) for `publish`/`info`.

## Layout

- `vendor/` holds links/copies; `zz.lock` pins exact versions.
- Flipping registry↔path during local dev is safe — just `zz install`.
- `bin/` is gitignored append-only by builds; `src/bin/` twins from old versions
  are reaped on publish (logged), anything else left alone.

Next: [Project layout](../getting-started/project-layout.md),
[Plugins](plugins.md).
