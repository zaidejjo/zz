# Project layout

Minimal project:

```
my-app/
  zz.toml
  src/main.zz
  tests/
  bench/
```

`zz.toml`:

```toml
[package]
name = "my-app"
version = "0.1.0"

[test]
jobs = 4
```

## Entry point

`zz build` / `zz run` with no file uses `src/main.zz` (then `main.zz`).
Override with `[package] entry = "src/cli.zz"` (must stay inside the package).

Outputs go to `<root>/bin/<name>` for projects, `./<stem>` for standalone files.
Sidecars (`app.c`, `build.sh`, `build.bat`) sit next to the binary for manual rebuilds.

## Imports resolve by tree, not CWD

```zz
import std.math
import std.str
import app.math            // <root>/src/math.zz as math.*
import app.utils.helpers as h
```

Precedence: `std` > package (`app.*`) > registry dep > relative file.
`-` and `_` spellings agree (`my-app` is `my_app`).

## Tests, benches, assets

- Tests: `zz test` uses `./tests/` when it holds `.zz` files, else `.`.
- Benches: `zz bench` uses `./bench/` when present, else `.`.
- Static assets: `zz build --embed ./assets` + `fs.embedfs()` at runtime.
- Deps: `vendor/` + `zz.lock`, managed by `zz install`. See [Packages](../tooling/packages.md).

## Useful commands

```bash
zz check src/        # fast, cached
zz fmt src/
zz test
zz bench
zz clean             # bin/, build/, src/bin/
zz clean --deps      # + vendor/ + zz.lock
```

Next: [REPL tour](repl-tour.md), [Modules](../language/modules.md).
