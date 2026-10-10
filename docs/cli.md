# CLI Reference

Complete guide to the `zz` command-line tool.

## Installation

```bash
cargo install --path crates/zz_cli
```

## Commands

### `zz` (REPL)

Start the interactive REPL:

```bash
zz
```

Output:
```
ZZ 0.1.0 — type-based language
Type expressions to evaluate. :help for commands.
zz>
```

#### REPL Features

- Multi-line continuation on trailing operators, open parens, or incomplete statements
- Bindings persist across snippets
- Each snippet is type-checked before execution
- Continuation prompt: `  | `

```
zz> x := 42
42
zz> x + 1
43
zz> func add(a: int, b: int) -> int { a + b }
<func add>
zz> add(3, 4)
7
```

### `zz run <file.zz>`

Type-check and run a ZZ source file:

```bash
zz run examples/demo.zz
```

### `zz eval <source>`

Evaluate a source string and print the result:

```bash
zz eval "1 + 2"
3

zz eval "println('hello')"
hello
()
```

### `zz build [FLAGS] [<file.zz>]`

Single Clang backend. Every `zz build` produces a real native binary and
requires `clang` (18+) or `zig` on PATH. (`zz run` without `--native` is
the only VM path, and it leaves no build artifacts.)

```bash
# Static (default): self-contained -O3 -flto=thin, stripped, DCE.
# System libs link conditionally: programs that never fetch skip
# libcurl, programs that never query skip libsqlite3 (no phantom
# DT_NEEDED). A program needing neither links fully static with no
# static syslibs required; fetch/query programs need libcurl.a /
# libsqlite3.a for explicit `--static`, otherwise the default build
# downgrades to dynamic with a note.
zz build main.zz                # standalone: produces ./main
zz build                        # inside a project: builds src/main.zz
                                # (then main.zz) to <root>/bin/<pkg-name>

# Release: native Clang -O3 -flto=thin, stripped, dynamic, cached under ~/.zz/cache
zz build -p main.zz
# Max optimization: full LTO (-O3, DCE, stripped); with `-- <args>` adds a PGO training run
zz build -p --full main.zz
zz build -p --full main.zz -- fast representative workload
zz build --static main.zz        # self-contained, explicit (not on macOS)
zz build --dynamic main.zz       # fast dynamic debug build -O0 -g
zz build -o server main.zz       # name the output binary (project bin/server or ./server)
zz build --pgo main.zz           # profile-guided, native host only
zz build --target aarch64-unknown-linux-gnu main.zz   # cross (implies -p)
zz build -p --cc zig main.zz     # use `zig cc` as the provider
zz build -p --verbose main.zz    # print the exact clang command line
zz build --embed ./assets main.zz            # bake static files into the binary
zz run --embed ./assets main.zz              # serve them in the VM instead
zz run --native --embed ./assets main.zz     # bake + run natively
```

Static assets (`--embed <dir>`) are readable at runtime through
`fs.embedfs()` (same `*_at` operations as every provider; read-only):

```zz
import std.fs

match fs.embedfs() {
    .ok(e) => match fs.read_to_string_at(e, "index.txt") {
        .ok(c)  => println(c),
        .err(x) => println(x),
    },
    .err(x) => println(x),
}
```

Asset edits invalidate the build cache (content-hashed), and `app.c`
manual builds include the same tables.

Output paths (3-line rule): standalone file → `./<stem>` in CWD;
project file → `<project-root>/bin/<name>`; `--target` adds
`-<triple>` suffix. `-o <name>` renames (bare name stays in the
resolved directory, a path is used as-is).

<details>
<summary>Output details (entry, discovery, sidecars, atomicity)</summary>

Project builds (any source under a project, or `zz build` with no
argument inside one) publish to `<project-root>/bin/`, named after
`[package] name` for the entry point or the file stem for explicit
non-entry files — never `src/bin/`. Byte-identical `src/bin/` twins
left by previous versions are reaped on publish (logged to stderr);
anything else there is left alone. Sidecars (`app.c`, `build.sh`,
`build.bat`: reproducible manual build with a single clang line) sit
next to the binary.

Entry point: `src/main.zz`, then `main.zz` — or `[package] entry` to
override (e.g. `entry = "src/cli.zz"`; must stay inside the package
and exist, else the build fails loudly). The entry also decides the
default binary name (`[package] name`).

Project discovery: `zz build` with no argument discovers the project
from the current working directory. With an explicit source path, the
file's owning project (nearest ancestor holding `zz.toml`) decides the
output directory — even when invoked from another directory. A file
outside any project builds standalone into the current directory.
`zz clean` operates on the discovered project root too. Builds into a
project root ensure `bin/` is gitignored (append-only, silent), so
pre-redesign checkouts stay clean.

Concurrent builds to one destination publish atomically (last writer
wins, never a half-written binary); `zz run --native` executes a
private staged copy, so a parallel publish can never swap the binary
mid-exec.
</details>

Cross-compilation rules:

| Rule | Behavior |
|------|----------|
| `--target` | Clang uses the triple's safe baseline CPU (no host-specific flags anywhere) |
| `--pgo --target <foreign>` | Rejected: `Error: --pgo requires a native build target` |
| `--static` on `apple-darwin` | Rejected: `Error: Static binaries are not supported on macOS targets` |
| Cross link | Adds `-fuse-ld=lld`; Windows triples also link `-lws2_32` |

All builds compile with `-fwrapv` (defined wrapping integer arithmetic)
and `-fno-strict-aliasing`, and without `-ffast-math` or
`-march=native`: VM and native binaries must agree bit-for-bit on
integers and floats (see the IR spec). If no Clang provider is
installed,
the build (debug or release) fails after still emitting `app.c` +
build scripts next to the planned destination so the program can be
built manually on a machine with Clang.

### `zz run [--native] [<file.zz>]`

`zz run` executes through the VM and leaves no build artifacts.
`zz run --native` builds through the cached Clang pipeline (publishing
to the same authoritative destination as `zz build`, `-o` overrides
like `build`) and executes the binary. The file argument defaults to
the project entry inside a project.

### `zz clean` / `zz cache`

```bash
zz clean                # remove project outputs (bin/, build/, src/bin/)
zz clean --deps         # ... plus vendor/ + zz.lock (re-fetch with zz install)
zz cache clean          # wipe the global build cache (~/.zz/cache)
zz cache gc             # garbage-collect unused package downloads
```

`zz clean` operates on the discovered project root from any subdir and
always reports the global build cache with a reclaim hint when it is
non-empty. The cache holds native binaries, precompiled runtime
archives, and per-module objects — everything rebuildable, keyed by
content, honoring `ZZ_HOME`. When disk runs low, `zz cache clean` is
the reclaim command (next builds just take a little longer).

### `zz toolchain <install|use|uninstall|status>`

Manage the Zig C backend. `zz build` needs a C backend (system `clang`
or `zig cc`); `toolchain install` downloads a pinned Zig release into
`~/.zz/toolchain` so builds work with no system toolchain and stay
reproducible across machines:

```bash
zz toolchain install                  # latest stable Zig, verified + pinned
zz toolchain install --version 0.17.0 # pin an exact release instead
zz toolchain status                   # installed versions, pin, active backend
zz toolchain use 0.17.0               # switch the pin among installed versions
zz toolchain uninstall 0.17.0         # remove an installed version
```

Details:

- Sources are official `ziglang.org` releases via `download/index.json`
  (URLs are never constructed — asset naming changed between eras);
  every download is sha256-checked against the index.
- A pin is explicit opt-in: once present, provider probing prefers the
  managed `zig` over everything on PATH (`--cc=clang` / `--cc=zig`
  still force their provider), and the precompiled-runtime cache key
  includes the pin so switching toolchains rebuilds instead of reusing.
- Set `ZZ_TOOLCHAIN_ROOT` to relocate the toolchain dir (hermetic CI).

### `zz check [FLAGS] [PATH]`

Scan files for errors and warnings:

```bash
# Check current directory
zz check

# Check specific file
zz check src/main.zz

# Check directory recursively
zz check src/
```

#### Check Flags

| Flag | Description |
|------|-------------|
| `--fix` / `-f` | Apply safe auto-fixes |
| `--hard` | Apply all fixes including ambiguous (no prompts) |
| `--interactive` / `-i` | Prompt for ambiguous fixes |
| `--stats` | Print per-file and total check stats (modules, cache hits/misses, seed size, wall time) |

> Note: `--check` / `-c` is a `zz fmt` flag only. `zz check --check`
> is rejected — `zz check` already checks without writing; use
> `zz fmt --check` for a formatting dry-run.

#### Check Performance Budget

`zz check` stays fast by construction (per-module seed key-set restore
instead of O(seed) clones, plus the S1 content-addressed check cache):

- Cold `zz check` on a single file: well under 0.5s (measured ~40ms).
- Warm (no edits): served from cache — `0 checked` in `--stats` output.
- Budget: p50 ≤ 0.5s on a 10k-line project cold, ≤ 50ms warm per file.
  Verify with `zz check --stats <path>` (run twice: the second run
  should show `cached` instead of `checked`).

```bash
$ zz check --stats src/main.zz
stats: src/main.zz: 1 modules (0 cached, 1 checked), seed 878 funcs, 38.1ms
stats: 1 files, 1 modules (0 cached, 1 checked), seed 878 funcs, 39.0ms total
```

### `zz fix [FLAGS] [PATH]`

Shortcut for `zz check --fix`:

```bash
zz fix src/

# Apply all fixes without prompting
zz fix --hard src/
```

### `zz fmt [FLAGS] [PATH]`

Format ZZ source files in-place:

```bash
# Format current directory
zz fmt

# Format specific file
zz fmt src/main.zz

# Check formatting without writing
zz fmt -c src/

# Format directory recursively
zz fmt src/
```

## Flags

| Flag | Short | Scope | Description |
|------|-------|-------|-------------|
| `--check` | `-c` | `fmt` only | Check formatting without writing (exit 1 if changed) |
| `--dry-run` | | `setup`, `upgrade` | Report status without changing anything (`--check` deprecated alias) |
| `--fix` | `-f` | `check`, `fix` | Apply safe auto-fixes |
| `--hard` | | `check`, `fix` | Apply all fixes including ambiguous |
| `--interactive` | `-i` | `check`, `fix` | Prompt for ambiguous fixes |
| `--help` | `-h` | all | Show usage |
| `--version` | `-V` | all | Show version |

## Diagnostics

ZZ provides rich diagnostic output with context:

### Error Output Format

```
error[E001]: type mismatch
  --> src/main.zz:5:12
   |
 5 |     x: int = "hello"
   |     ----   ^^^^^^^^ expected int, found str
   |
   = help: cast string to int with `int()` function
```

### Diagnostic Features

- **Source context**: Shows the offending line with underline
- **Location**: File path, line number, column
- **Error code**: Categorized error (E001, E002, etc.)
- **Help text**: Suggestions for fixing the error
- **Multiple errors**: Reports all errors in a pass, not just the first

### Auto-Fix

Some diagnostics offer automatic fixes:

```
warning[W001]: unused variable
  --> src/main.zz:3:5
   |
 3 |     x := 42
   |     ^^^^^^^
   |
   = help: prefix with `_` to suppress: `_x := 42`
   = fix: `_x := 42`
```

Run `zz fix` to apply fixes:

```bash
zz fix src/main.zz
```

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | Errors found, or formatting check failed |

## File Loading

The CLI resolves imports relative to the source file's directory:

```
project/
├── main.zz          // println("hi")
├── utils/
│   ├── mod.zz       // import std.math
│   └── helper.zz    // import .utils as util
```

- `import std.*` loads from the built-in standard library
- `import .name` loads from the current directory
- `import utils.helper` loads `utils/helper.zz` relative to the source file
- `import <package>.path` (package name from the nearest `zz.toml`)
  loads `<project-root>/src/path.zz`, from anywhere in the project —
  including `tests/` files importing `src/` modules. A bare
  `import <package>` loads `<project-root>/src/main.zz` under the
  package name. Resolution precedence: `std` > package > registry
  dependency > relative file. `-` and `_` spellings of the package
  name both match (`my-app` ⇔ `my_app`).

### Circular imports

Circular imports are rejected (`a` imports `b`, `b` imports `a`):

```text
circular import: `a.zz` imports `b.zz`, which (transitively) imports `a.zz`
hint: circular imports are not allowed; consider restructuring your code to break the cycle
```

Restructure by extracting shared types into `c.zz`, then both `a` and `b`
import `c` with no cycle. Cyclic imports remain unsupported by design
(use a shared leaf module to break the cycle).

### One file, one namespace

One file cannot be imported under two different aliases in the same
program (`import a as x` in one file, `import a as y` in another is an
error). Struct identity is namespace-qualified (`x.T` vs `y.T` would
diverge), so the loader keeps a single canonical namespace per file.
Workaround: have both importers use the same name (or no alias), and
import the shared file directly. Per-importer alias copies remain
unsupported — migrate all importers to one canonical import path.

### Dependency sources

A dependency name has exactly one source: whatever `zz.toml` declares
(`"1.2.3"` → registry, `{ git = …, rev = … }` → git, `{ path = … }` →
path). The manifest always wins — a lock pin from another source (left
over from before the manifest flipped, e.g. registry → path during
local development) is stale by definition: the resolver ignores it,
re-resolves from the manifest, and prints
`note: <name>: manifest declares <source>, ignoring stale <source> pin`.
`zz install` rewrites the lock and recreates `vendor/` links, so
flipping a dep between path and registry needs no manual cleanup.
`zz install` also verifies materialization: on a fresh clone (lock
matches, `vendor/` missing) it fetches and links instead of reporting
"up to date". Same-name declarations across workspace members stay
unsupported until `[workspace]` ships — keep dependency names unique
per workspace until then.

## REPL Commands

In the REPL, these special commands are available:

| Command | Description |
|---------|-------------|
| `:help` | Show help |
| `:quit` | Exit REPL |
| `:type expr` | Show the type of an expression |
| `:clear` | Clear all bindings |

## Examples

### Run a Complete Program

```bash
zz run examples/demo.zz
```

### Quick Evaluation

```bash
zz eval "range(1, 6) |> map(|x| x * 2) |> str"
[2, 4, 6, 8, 10]
```

### Check for Errors

```bash
$ zz check src/
error[E001]: type mismatch
  --> src/main.zz:5:12
   |
 5 |     x: int = "hello"
   |     ----   ^^^^^^^^ expected int, found str
```

### Format Code

```bash
$ zz fmt src/main.zz
Formatted 1 file
```

### Auto-Fix Errors

```bash
$ zz fix src/main.zz
Fixed 2 issues in src/main.zz
```
