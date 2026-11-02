# Args — `std.args`

Raw argv + flag parser. Opaque parser handles (`Opaque("args")`); declare flags, parse, then read. Supports `str`/`int`/`bool` flags, positionals, subcommands, and `--help` text.

```zz
import std.args
```

## Purpose

`std.args` parses command-line flags: `str_flag`, `int_flag`, `bool_flag`, positional args, subcommands, and `--help` text. Declare before `parse`; read after. `get_raw()` returns raw argv without parsing. Ideal for CLIs with `--flag value`, `--bool`, subcommands, and `--help` text.

## Quickstart

```zz
import std.args

p := ArgsParser.new()
p.str_flag("out", "out.txt")
p.int_flag("jobs", 4)
p.bool_flag("verbose")
if p.parse(["--jobs", "8"]) {
    println(p.get_str("out"))
    println(p.get_int("jobs"))
    println(p.get_bool("verbose"))
} else {
    println(p.error())
}

println(p.positional(0) ?? "-")   // first leftover or "-"
println(p.subcommand() ?? "none")
println(p.help("myapp"))          // --help text
if p.was_help() { println("help shown") }
```

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `std.args.get_raw` | `std.args.get_raw() -> [str]` | Raw argv without parsing | |
| `std.args.parser` | `std.args.parser() -> args` | Create parser handle | `p := std.args.parser()` |
| `std.args.str_flag` | `std.args.str_flag(p, name, default) -> unit` | Declare string flag | |
| `std.args.int_flag` | `std.args.int_flag(p, name, default) -> unit` | Declare int flag | |
| `std.args.bool_flag` | `std.args.bool_flag(p, name) -> unit` | Declare bool flag | |
| `std.args.parse` | `std.args.parse(p, argv: [str]) -> bool` | Parse argv; `false` on error | |
| `std.args.get_str` | `std.args.get_str(p, name) -> str` | Read string flag | |
| `std.args.get_int` | `std.args.get_int(p, name) -> int` | Read int flag | |
| `std.args.get_bool` | `std.args.get_bool(p, name) -> bool` | Read bool flag | |
| `std.args.positional` | `std.args.positional(p, i: int) -> Option<str>` | Positional arg by index | |
| `std.args.subcommand` | `std.args.subcommand(p) -> str` | Subcommand name | |
| `std.args.help` | `std.args.help(p, prog: str) -> str` | Help text | |
| `std.args.error` | `std.args.error(p) -> str` | Last parse error | |
| `std.args.was_help` | `std.args.was_help(p) -> bool` | Was `--help` requested | |
| `ArgsParser.new` | `ArgsParser.new() -> args` | Constructor namespace spelling | `p := ArgsParser.new()` |

## Method syntax

Parser handles dispatch method calls:

```zz
import std.args

func main() -> Result<int, str> {
    p := ArgsParser.new()
    p.str_flag("out", "out.txt")
    if !p.parse(["--out", "a.txt"]) {
        return .err(p.error())
    }
    println(p.get_str("out"))   // "a.txt"
    return .ok(0)
}
```

## Error model

- `parse` returns `bool` — check it before reading flags; `error()` gives the message on failure.
- `get_str`/`get_int`/`get_bool` assume the flag was declared; undeclared names are errors.

## Gotchas

- **Declare all flags before `parse`; read values after.**
- **`help` text derives from flag declarations** — keep descriptions short verbs.
- **`get_raw()`** bypasses parsing — use for passthrough to subcommands.
- **Positionals** are leftovers after flag consumption.

## Production patterns

```zz
import std.args

func cli(argv: [str]) -> Result<int, str> {
    p := ArgsParser.new()
    p.str_flag("config", "config.toml")
    p.int_flag("port", 8080)
    p.bool_flag("verbose")
    if !p.parse(argv) {
        return .err(p.error())
    }
    if p.was_help() {
        println(p.help("myapp"))
        return .ok(0)
    }
    println(p.get_str("config"))
    return .ok(0)
}
```

