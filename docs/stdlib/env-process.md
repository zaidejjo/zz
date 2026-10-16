# Env and processes — `std.env`, `std.args`, `std.process`, `std.sys`, `std.log`

## `std.env`

```zz
import std.env

match env.get("HOME") {
    .some(home) => println("Home: {home}"),
    .none       => println("HOME not set"),
}
match env.set("APP_MODE", "demo") {
    .ok(_)  => println("set"),
    .err(e) => println(e),
}
env.remove("APP_MODE")  // total no-op when absent

match env.cwd() {
    .ok(c)  => println(c),
    .err(e) => println(e),
}
println(env.os())        // linux / macos / windows
println(env.temp_dir())  // always succeeds
for arg in env.args() { println(arg) }
```

| Function | Notes |
|---|---|
| `get(key)` | `str?` — `.none` when absent |
| `get_var` | legacy alias shape |
| `var(name)` | value or `.err` |
| `set/remove` | process-wide, POSIX-like; bad key is `.err` |
| `vars()` | all, sorted by key |
| `cwd` / `set_cwd` | `.err` on failure |
| `exe_path` | absolute binary path |
| `home_dir`, `user` | `Option` |
| `temp_dir` | total |
| `os` / `arch` | platform strings |
| `args()` | script args |

## `std.args` — flag parser

Opaque parser handles; declare flags, parse, then read:

```zz
import std.args
// parser() -> str_flag/int_flag/bool_flag -> parse -> get_str/get_int/get_bool
// + positional, subcommand, help, error, was_help, get_raw
```

Use for CLIs with `--flag value`, `--bool`, subcommands, and `--help` text.
See [stdlib.md](../stdlib.md) for exact arities.

## `std.process`

Results cross as `[status, stdout, stderr]` inside `Result`; spawned children
are opaque `"process"` handles.

```zz
import std.process

match process.run("ls", ["-la"]) {
    .ok(out)  => println("status {out[0]}"),
    .err(e)   => println(e),
}
match process.run_with_env("make", ["test"], ["RUST_BACKTRACE=1"]) {
    .ok(out)  => println(out[1]),
    .err(e)   => println(e),
}
```

`spawn(cmd, argv)` → handle (non-blocking), `wait(handle)` → triple,
`pid()` → int, `exit(code)` terminates. Env entries must be `K=V` or check-time
errors name the entry.

## `std.sys`

Infallible queries: `os`, `arch`, `cpu_count`, `hostname`, `total_mem`, `avail_mem`
(bytes for mem). Use for worker sizing and diagnostics headers.

## `std.log`

Levels, sinks, spans. Span handles are opaque `"span"`.

```zz
import std.log

log.set_level("info")
log.info("server ready")
log.warn("retrying")
println(log.get_level())  // "info"
log.to_stderr()
log.to_file("/tmp/app.log")
span := log.span_begin("request")
span.end()
```

`set_format`, `trace/debug/info/warn/error`. Keep hot-path logs at `debug`
and gate verbose fields behind level checks.

Next: [Packages](../tooling/packages.md), [Cookbook](../guides/cookbook.md).
