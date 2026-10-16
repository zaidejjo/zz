# ZZ Documentation

Fast systems language: native AOT, green threads, zero GC pauses.
This is the developer-friendly entry point. Pick a path below.

## Start here

- New to ZZ? Start with [Quick start](getting-started/quick-start.md), then
  [Project layout](getting-started/project-layout.md) and [REPL tour](getting-started/repl-tour.md).
- Installing or updating? See [Install](getting-started/install.md).
- One page? See [Cheatsheet](getting-started/cheatsheet.md).
- Building something now? Jump to [Cookbook](guides/cookbook.md).

## Learn the language

- [Language overview](language/overview.md) — mental model in 5 minutes.
- [Variables and functions](language/variables-functions.md) — `:=`, `: T =`, defaults, closures, generics.
- [Control flow](language/control-flow.md) — `if` / `if let`, `for`, `while`, `defer`, `break`.
- [Strings](language/strings.md) — interpolation, `{{ }}`, raw backticks, format specs.
- [Collections](language/collections.md) — arrays, dicts, tuples, slicing, comprehensions, pipelines.
- [Structs and enums](language/structs-enums.md) — embedding, `impl`, generics, recursion.
- [Pattern matching](language/pattern-matching.md) — `match`, guards, exhaustiveness.
- [Operators](language/operators.md) — precedence, associativity, type rules.
- [Lexicon](language/lexicon.md) — keywords, literals, comments, terminators.
- [Modules](language/modules.md) — imports, package vs relative, cycles.
- [Value semantics](language/value-semantics.md) — copies, no aliasing, what the optimizer may elide.
- Deep references (complete, exhaustive):
  [Syntax](syntax.md), [Types](types.md).

## Standard library

- [Stdlib overview](stdlib/overview.md) — module index and namespace rule.
- [Built-ins](stdlib/builtins.md) — no import: `println`, `input`, `len`, `range`, `map`, `filter`, `int`, `float`.
- [Strings and arrays](stdlib/str-vec.md) — `std.str`, `std.vec`, `std.bytes` builders.
- [Data helpers](stdlib/data.md) — `std.map`, `std.set`, `std.dec`, `std.csv`, `std.property`.
- [Filesystem and bytes](stdlib/fs-bytes.md) — `std.fs`, providers, `bytes` buffers.
- [HTTP](stdlib/http.md) — server, routing, middleware, static, TLS, client, in-process tests.
- [Networking](stdlib/net.md) — TCP, timeouts, binary-safe reads.
- [Env and processes](stdlib/env-process.md) — `std.env`, `std.args`, `std.process`, `std.sys`, `std.log`.
- [Math and time](stdlib/math-time.md) — `std.math`, `std.time` + calendar dates.
- [Terminal](stdlib/terminal.md) — `std.term`, `std.colors`.
- [Encoding and crypto](stdlib/encoding-crypto.md) — base64/hex/URL, sha/hmac, argon2/bcrypt, JWT, ed25519/RSA.
- [Regexp](stdlib/regexp.md) — `std.regexp` handles, match/find/replace/captures.
- [Databases](stdlib/sql-db.md) — `std.sqlz` (sqlite), postgres and mysql drivers.
- [Concurrency](stdlib/concurrency.md) — `std.chan`, `std.task`, snapshot isolation.
- [System slots](stdlib/cell-path-sys.md) — `cell`, `path`, `sys`, `args`, `log`, TLS/Unix.
- [Test helpers](stdlib/testing-lib.md) — asserts, `std.test`, snapshots.
- [Stdlib reference](stdlib/reference.md) — generated 465 fns across 34 modules.
- Exhaustive API tables: [stdlib.md](stdlib.md).

## Tooling

- [CLI](tooling/cli.md) — `run`, `build`, `eval`, `watch`, `clean`, `cache`, `toolchain`.
- [CLI reference](tooling/cli-reference.md) — every command and flag in one table.
- [Check, fix, format](tooling/check-fmt.md) — diagnostics, auto-fix, formatting.
- [Test and bench](tooling/test-bench.md) — `zz test`, doctests, snapshots, `zz bench`, coverage.
- [Packages](tooling/packages.md) — `zz.toml`, registry/git/path deps, `vendor/`, publish.
- [Plugins](tooling/plugins.md) — C ABI, `plugin.zzi`, native build hooks.
- [Editors and debugging](tooling/editors-debug.md) — LSP, DAP, VS Code, Neovim.
- [Performance](tooling/perf.md) — budgets, PGO, profiling, `black_box`.
- [Performance deep dive](tooling/perf-deep.md) — profiling recipes, memory table.
- Command details: [CLI](cli.md), [Testing](testing.md), [Bench](bench.md).

## Compiler internals

- [Architecture](compiler/architecture.md) — frontend → checker → HIR → IR → VM / AOT.
- [Checker](compiler/checker.md) — unification, inference, errors, suggestions.
- [Runtime and codegen](compiler/runtime-codegen.md) — tree-walker, bytecode VM, HIR→C, CoW, DCE.
- Specs: [IR draft](ir-spec-draft.md), [VM CoW](./design/vm-cow.md).

## Guides

- [Cookbook](guides/cookbook.md) — file server, JSON API, CLI, CSV→JSON, worker pool.
- [Error catalog](guides/errors.md) — most common errors and exact fixes.
- [Error index](guides/error-index.md) — verbatim messages with fixes.
- [VM vs native parity](guides/parity.md) — what is guaranteed identical.
- [FAQ and glossary](guides/faq-glossary.md) — quick answers + terms.

## Conventions used in these docs

- `zz` fences are complete programs. Run with `zz run file.zz`.
- `//` output comments show what `println` prints.
- Tables list canonical paths (`std.vec.push`). Bare aliases (`push`) work only where noted.
- VM (`zz run`) and AOT (`zz build` / `zz run --native`) agree unless a page says otherwise.

## Legacy flat references

The complete references still live at the top level and are linked everywhere above.
New pages are the learning path; old pages are the source of truth:

- [syntax.md](syntax.md), [types.md](types.md), [stdlib.md](stdlib.md),
  [cli.md](cli.md), [testing.md](testing.md), [threading.md](threading.md),
  [debugging.md](debugging.md), [lsp-setup.md](lsp-setup.md),
  [bench.md](bench.md), [coverage.md](coverage.md).
