# `std.args` — command-line argument parser

Opaque parser handles for building CLIs with `--flag value`, `--bool`,
subcommands, positional args, and `--help` text. Create a parser, declare
flags, parse, then read values.

```zz
import std.args

p := std.args.parser()
p.str_flag("out", "out.txt")
p.int_flag("jobs", 4)
p.bool_flag("verbose")

if p.parse(["--jobs", "8", "--verbose"]) {
    println("out=", p.get_str("out"))
    println("jobs=", p.get_int("jobs"))
    println("verbose=", p.get_bool("verbose"))
} else {
    println("parse failed: ", p.error())
}
```

## Types

| Type | Notes |
|---|---|
| `args` | Opaque parser handle. Created with `parser()`; passed as the first argument to every `std.args.*` function. Not copyable by value — pass it through. |

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `parser()` | `parser() -> args` | Create a new, empty argument parser. |
| `parse(p, argv)` | `parse(p: args, argv: [str]) -> bool` | Parse `argv` (e.g. process args minus the program name). Returns `true` on success. |
| `str_flag(p, name, default)` | `str_flag(p: args, name: str, default: str) -> unit` | Declare a string flag `--name value` with `default`. |
| `int_flag(p, name, default)` | `int_flag(p: args, name: str, default: int) -> unit` | Declare an int flag `--name N` with `default`. |
| `bool_flag(p, name)` | `bool_flag(p: args, name: str) -> unit` | Declare a boolean flag `--name` (present = true). |
| `subcommand(p)` | `subcommand(p: args) -> str` | The matched subcommand name after a successful parse. |
| `get_str(p, name)` | `get_str(p: args, name: str) -> str` | Value of string flag `name`. Panics/errors if absent or wrong type. |
| `get_int(p, name)` | `get_int(p: args, name: str) -> int` | Value of int flag `name`. |
| `get_bool(p, name)` | `get_bool(p: args, name: str) -> bool` | Value of bool flag `name`. |
| `positional(p, i)` | `positional(p: args, i: int) -> Option<str>` | The `i`-th positional argument, `.none` if absent. |
| `get_raw()` | `get_raw() -> [str]` | Raw remaining arguments after parsing. |
| `error(p)` | `error(p: args) -> str` | Human-readable parse error message (empty on success). |
| `was_help(p)` | `was_help(p: args) -> bool` | `true` if `--help` was requested. |
| `help(p, prog)` | `help(p: args, prog: str) -> str` | Rendered help text for `prog`. |
