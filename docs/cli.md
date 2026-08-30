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
├── main.zz          // import std.io
├── utils/
│   ├── mod.zz       // import std.math
│   └── helper.zz    // import .utils as util
```

- `import std.*` loads from the built-in standard library
- `import .name` loads from the current directory
- `import utils.helper` loads `utils/helper.zz` relative to the source file

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
