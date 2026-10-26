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
faker = { version = "1.0", optional = true }

[features]
# List form: enables other features and/or optional deps.
default = ["std", "locales-ar"]
std = []
# Table form: gates module files and pulls in optional deps.
locales-ar = { modules = ["locales/ar"], deps = ["faker"] }

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

## Features

`[features]` makes optional dependencies and module files opt-in. Without the
section nothing changes: every dependency installs and every module imports
exactly as before.

- A **list** entry (`default = ["std"]`) enables other features. It may also
  name an `optional = true` dependency directly.
- A **table** entry (`locales-ar = { modules = [...], deps = [...] }`) gates
  module files and enables optional dependencies.
- `modules` paths are relative to the package source root (`src/`), without
  `.zz`: `"locales/ar"` gates `src/locales/ar.zz`. Absolute paths and `..`
  are rejected.
- `deps` must name a declared `optional = true` dependency, and every optional
  dependency must be reachable from at least one feature (otherwise it can
  never be enabled).
- The `default` feature is always active. `--features a,b` (repeatable) adds
  more; `--all-features` enables every declared feature.

```bash
zz install                          # only the default feature set
zz install --features locales-ar    # + locales-ar (and its optional dep)
zz run --features locales-ar src/main.zz
zz build --all-features
zz check --features locales-ar,extras src
```

Disabled optional dependencies stay out of resolution: no registry metadata,
lock entry, `vendor/` link, or compile. Importing a gated module or dependency
names the exact feature:

```
cannot resolve import `locales.ar`: module `locales/ar` is provided by feature `locales-ar`, which is not enabled
hint: enable it with --features locales-ar (or add it to `default`)
```

`zz install` records the feature selection in `zz.lock`; switching features
re-resolves and relinks. Published packages carry their `[features]` tables in
the registry metadata, and `zz info <pkg>` prints them.

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
