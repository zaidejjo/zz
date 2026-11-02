# Env — `std.env`

Cross-platform environment + OS identity. `get`/`home_dir`/`user` return `Option`; `temp_dir` is total; `var`/`set`/`cwd`/`set_cwd`/`exe_path` are fallible (`Result`).

```zz
import std.env
```

## Purpose

`std.env` reads/sets environment variables, queries the working directory, executable path, home/temp dirs, current user, OS/arch strings, and script args. Process-wide like POSIX `getenv`/`setenv`.

## Quickstart

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
env.remove("APP_MODE")     // total no-op when absent

match env.cwd() {
    .ok(c)  => println(c),
    .err(e) => println(e),
}
println(env.os())          // linux / macos / windows
println(env.temp_dir())    // always succeeds

for arg in env.args() { println(arg) }
```

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `env.get` | `env.get(key: str) -> Option<str>` | Value or `.none` when absent | `env.get("HOME")` |
| `env.var` | `env.var(name: str) -> Result<str, str>` | Value or `.err` | |
| `env.set` | `env.set(key: str, val: str) -> Result<unit, str>` | Set (`.err` on bad key) | `env.set("APP_MODE","demo")` |
| `env.remove` | `env.remove(key: str) -> unit` | Remove (total no-op when absent) | `env.remove("APP_MODE")` |
| `env.vars` | `env.vars() -> {str: str}` | All variables, sorted by key | |
| `env.cwd` | `env.cwd() -> Result<str, str>` | Working directory (`.err` on failure) | |
| `env.set_cwd` | `env.set_cwd(path: str) -> Result<unit, str>` | Change directory (`.err` on failure) | |
| `env.exe_path` | `env.exe_path() -> Result<str, str>` | Absolute path of this binary | |
| `env.home_dir` | `env.home_dir() -> Option<str>` | `HOME` / `USERPROFILE` | |
| `env.temp_dir` | `env.temp_dir() -> str` | System temp dir (total) | |
| `env.user` | `env.user() -> Option<str>` | `USER`/`LOGNAME` / `USERNAME` | |
| `env.os` | `env.os() -> str` | `"linux"` / `"macos"` / `"windows"` | |
| `env.arch` | `env.arch() -> str` | Architecture string | |
| `env.args` | `env.args() -> [str]` | Script arguments | |

## Error model

- **`get`/`home_dir`/`user`** return `Option` (`.none` when absent) — total.
- **`temp_dir`/`os`/`arch`/`args`** — total.
- **`var`/`set`/`cwd`/`set_cwd`/`exe_path`** return `Result<_, str>`.

## Gotchas

- **`env.set`/`env.remove` are process-wide** (POSIX-like) — affect the whole program.
- **`env.remove` is a total no-op** when the key is absent.
- **`env.args()`** returns script args; use [`std.args`](args.md) for flag parsing.

## Production patterns

```zz
import std.env

func require_env(key: str) -> Result<str, str> {
    return match env.get(key) {
        .some(v) => .ok(v),
        .none    => .err("env: {key} not set"),
    }
}

func main() -> Result<int, str> {
    home := require_env("HOME")?
    println(home)
    return .ok(0)
}
```

