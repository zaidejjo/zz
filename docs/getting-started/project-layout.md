# Project layout

Minimal project:

```
my-app/
  zz.toml
  src/main.zz
  tests/
  bench/
```

## Create it

```bash
# Create a new project in a fresh folder (templates: cli | lib | web | plugin)
zz new my-app --template cli

# Or initialize zz.toml + src/main.zz in the current folder
zz init --template cli
```

## zz.toml

```toml
[package]
name = "my-app"
version = "0.1.0"

[test]
jobs = 4

# Add dependencies here, or with `zz add <pkg>[@ver]` then `zz install`.
[dependencies]
# my-dep = "1.2.3"
```

`zz add <pkg>[@ver]` writes the entry into `zz.toml`; `zz install` resolves
dependencies into `vendor/` and writes the exact pins to `zz.lock`. Commit
`zz.toml` and `zz.lock`; `vendor/` is typically git-ignored (it can be
regenerated with `zz install`).

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

## Useful commands

```bash
zz check src/        # fast, cached type-check
zz fmt src/          # format in place
zz test              # run @test functions
zz bench             # run @bench functions
zz clean             # bin/, build/, src/bin/
zz clean --deps      # + vendor/ + zz.lock
zz upgrade           # self-update zz + zz-lsp from GitHub releases
```

Next: [REPL tour](repl-tour.md), [Modules](../language/modules.md).
