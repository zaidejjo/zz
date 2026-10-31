# `std.env` — environment variables and process info

Read and set environment variables, query the process working directory,
executable path, and platform identity.

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
env.remove("APP_MODE")  // no-op when absent

match env.cwd() {
    .ok(c)  => println("cwd: {c}"),
    .err(e) => println(e),
}
println(env.os())        // linux / macos / windows
println(env.arch())      // x86_64 / aarch64
println(env.temp_dir())  // always succeeds
for arg in env.args() { println(arg) }
```

## Types

None.

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `get(key)` | `get(key: str) -> Option<str>` | Value of env var `key`; `.none` when absent. |
| `var(name)` | `var(name: str) -> Result<str, str>` | Value or `.err` when absent. |
| `set(key, val)` | `set(key: str, val: str) -> Result<unit, str>` | Set process-wide; bad key is `.err`. |
| `remove(key)` | `remove(key: str) -> unit` | Remove a var (no-op when absent). |
| `vars()` | `vars() -> {str: str}` | All vars, sorted by key. |
| `cwd()` | `cwd() -> Result<str, str>` | Current working directory. |
| `set_cwd(path)` | `set_cwd(path: str) -> Result<unit, str>` | Change working directory. |
| `exe_path()` | `exe_path() -> Result<str, str>` | Absolute path of the running binary. |
| `home_dir()` | `home_dir() -> Option<str>` | User's home directory. |
| `user()` | `user() -> Option<str>` | Current username. |
| `temp_dir()` | `temp_dir() -> str` | System temp directory (always succeeds). |
| `os()` | `os() -> str` | Platform: `linux`, `macos`, or `windows`. |
| `arch()` | `arch() -> str` | CPU architecture. |
| `args()` | `args() -> [str]` | Command-line args (program name first). |
