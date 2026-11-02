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
- [String](stdlib/str.md) — `std.str` manipulation, byte offsets.
- [Vector](stdlib/vec.md) — `std.vec` array ops.
- [Bytes](stdlib/bytes.md) — `std.bytes` buffers.
- [Map](stdlib/map.md) / [Set](stdlib/set.md) / [Decimal](stdlib/dec.md) — data helpers.
- [JSON](stdlib/json.md) — parse/stringify/get/as_*.
- [CSV](stdlib/csv.md) — RFC4180 subset.
- [Property](stdlib/property.md) — seeded generators + shrinking.
- [Colors](stdlib/colors.md) — ANSI styling.
- [Crypto](stdlib/crypto.md) — digests, HMAC, passwords, signatures, JWT.
- [Encoding](stdlib/encoding.md) — base64, hex, URL codecs.
- [UUID](stdlib/uuid.md) — v4/v7 generation, parse, validate.
- [Log](stdlib/log.md) / [Sys](stdlib/sys.md) / [Term](stdlib/term.md) — OS/terminal.
- [Args](stdlib/args.md) / [Env](stdlib/env.md) / [Process](stdlib/process.md) — env/CLI/process.
- [FS](stdlib/fs.md) / [Path](stdlib/path.md) — filesystem + lexical paths.
- [HTTP](stdlib/http.md) — server, routing, middleware, static, TLS, client.
- [Net](stdlib/net.md) — TCP, timeouts, binary-safe reads, TLS, Unix.
- [Cell](stdlib/cell.md) — shared-mutable slots.
- [Regexp](stdlib/regexp.md) — compiled patterns.
- [SQL](stdlib/sqlz.md) — SQLite, postgres, mysql, transactions.
- [Chan](stdlib/chan.md) / [Task](stdlib/task.md) — concurrency.
- [Test](stdlib/test.md) — asserts, temp dirs, snapshots.
- [Math](stdlib/math.md) / [Time](stdlib/time.md) — numerics, clocks, dates.
- [Built-ins](stdlib/builtins.md) — no import: `println`, `input`, `len`, `range`, `map`, `filter`, `int`, `float`.
- [Stdlib reference](stdlib/reference.md) — generated entries with signatures.
- Exhaustive API tables: [stdlib.md](stdlib.md).
- [Stdlib reference](stdlib/reference.md) — generated 691 entries with signatures across 42 groups.
- Exhaustive API tables: [stdlib.md](stdlib.md).

## Tooling

- [CLI](tooling/cli.md) — `run`, `build`, `eval`, `watch`, `clean`, `cache`, `toolchain`.
- [CLI reference](tooling/cli-reference.md) — every command and flag in one table.
- [Check, fix, format](tooling/check-fmt.md) — diagnostics, auto-fix, formatting.
- [Test and bench](tooling/test-bench.md) — `zz test`, doctests, snapshots, `zz bench`, coverage.
- [Packages](tooling/packages.md) — `zz.toml`, registry/git/path deps, `vendor/`, publish.
- [Plugins](tooling/plugins.md) — C ABI, `plugin.zzi`, native build hooks.
- [Editors and debugging](tooling/editors-debug.md) — LSP, DAP, VS Code, Neovim.
- [Offline doc lookup](tooling/doc-spec.md) — `zz doc` spec over local files.
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
- [Examples index](guides/examples-index.md) — curated map (examples/ is scratch).
- [VM vs native parity](guides/parity.md) — what is guaranteed identical.
- [Docker and production AOT](guides/docker.md) — builder/runtime images, no app-level Clang.
- [FAQ and glossary](guides/faq-glossary.md) — quick answers + terms.

## Conventions used in these docs

- `zz` fences are complete programs. Run with `zz run file.zz`.
- `//` output comments show what `println` prints.
- Tables list canonical paths (`std.vec.push`). Bare aliases (`push`) work only where noted.
- VM (`zz run`) and AOT (`zz build` / `zz run --native`) agree unless a page says otherwise.

## Deep references (complete, historical)

New pages above are canonical. Flat files below are deep references kept for
history and code links — each carries a banner to its canonical replacement.
On conflict, the new page + `funcs.rs` win:

- [syntax.md](syntax.md), [types.md](types.md), [stdlib.md](stdlib.md),
  [cli.md](cli.md), [testing.md](testing.md), [threading.md](threading.md),
  [debugging.md](debugging.md), [lsp-setup.md](lsp-setup.md),
  [bench.md](bench.md), [coverage.md](coverage.md).
