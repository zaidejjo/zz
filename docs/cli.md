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

### `zz build [FLAGS] <file.zz>`

Single Clang backend. Every `zz build` produces a real native binary and
requires `clang` (18+) or `zig` on PATH. (`zz run` without `--native` is
the only VM path.)

```bash
# Static (default): self-contained -O3 -flto=thin, stripped, DCE.
# System libs link conditionally: programs that never fetch skip
# libcurl, programs that never query skip libsqlite3 (no phantom
# DT_NEEDED). A program needing neither links fully static with no
# static syslibs required; fetch/query programs need libcurl.a /
# libsqlite3.a for explicit `--static`, otherwise the default build
# downgrades to dynamic with a note.
zz build main.zz

# Release: native Clang -O3 -flto=thin, stripped, dynamic, cached under ~/.zz/cache
zz build -p main.zz
# Max optimization: full LTO (-O3, DCE, stripped); with `-- <args>` adds a PGO training run
zz build -p --full main.zz
zz build -p --full main.zz -- fast representative workload
zz build --static main.zz        # self-contained, explicit (not on macOS)
zz build --dynamic main.zz       # fast dynamic debug build -O0 -g
zz build -o server main.zz       # name the output binary (bin/server)
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

Asset edits invalidate the build cache (content-hashed), and `bin/app.c`
manual builds include the same tables.

All artifacts live in `bin/` next to the source: `bin/app`,
`bin/app.exe` (Windows), or `bin/app-<triple>[.exe]` for `--target`
builds, plus `bin/app.c`, `bin/build.sh`, `bin/build.bat` (reproducible
manual build with a single clang line). `-o <name>` renames the binary
(a bare name stays in `bin/`, a path is used as-is, go-like).

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
the build (debug or release) fails after still emitting `bin/app.c` +
build scripts so the program can be built manually on a machine with
Clang.

### `zz run --native <file.zz>`

Transient release compile → execute → cleanup (uses the same Clang
release pipeline as `zz build -p`).

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
| `--check` / `-c` | Check formatting without writing |

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

| Flag | Short | Description |
|------|-------|-------------|
| `--check` | `-c` | Check formatting without writing (exit 1 if changed) |
| `--fix` | `-f` | Apply safe auto-fixes |
| `--hard` | | Apply all fixes including ambiguous |
| `--interactive` | `-i` | Prompt for ambiguous fixes |
| `--help` | `-h` | Show usage |
| `--version` | `-V` | Show version |

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

### Circular imports

Circular imports are rejected (`a` imports `b`, `b` imports `a`):

```text
circular import: `a.zz` imports `b.zz`, which (transitively) imports `a.zz`
hint: circular imports are not allowed; consider restructuring your code to break the cycle
```

Restructure by extracting shared types into `c.zz`, then both `a` and `b`
import `c` with no cycle. Tracking full cycle support in #232.

### One file, one namespace

One file cannot be imported under two different aliases in the same
program (`import a as x` in one file, `import a as y` in another is an
error). Struct identity is namespace-qualified (`x.T` vs `y.T` would
diverge), so the loader keeps a single canonical namespace per file.
Workaround: have both importers use the same name (or no alias), and
import the shared file directly. Tracking per-importer alias copies
in #228.

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
