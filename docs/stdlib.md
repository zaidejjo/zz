# Standard Library

Exhaustive reference of all built-in functions and modules.

## Built-in Functions

Available without imports:

| Function | Signature | Description |
|----------|-----------|-------------|
| `print` | `print(v: T) -> unit` | Print value without newline |
| `println` | `println(v: T) -> unit` | Print value with newline |
| `input` | `input(prompt: str) -> str` | Read line from stdin (optional prompt) |
| `typeof` | `typeof(v: T) -> str` | Runtime type name |
| `str` | `str(v: T) -> str` | Convert to string |
| `int` | `int(v: T)` | Parse/convert to int (`.none` on failure) |
| `float` | `float(v: T)` | Parse/convert to float (`.none` on failure) |
| `len` | `len(v: T) -> int` | Length of array, bytes, string, dict, or range |
| `range` | `range(start: int, stop: int, step: int)` | Create integer range |
| `map` | `map(arr: [T] \| T.., f: func(T) -> U) -> [U]` | Apply function to each element |
| `filter` | `filter(arr: [T] \| T.., f: func(T) -> bool) -> [T]` | Keep elements where predicate is true |
| `enumerate` | `enumerate(arr: [T] \| T..)` | Index + value pairs |
| `zip` | `zip(a: [T] \| T.., b: [U] \| U..)` | Pair elements from two iterables |

## Module Index

| Module | Purpose |
|--------|---------|
| `std.str` | String manipulation |
| `std.vec` | Array operations |
| `std.json` | JSON parsing/serialization |
| `std.http` | HTTP server |
| `std.fs` | Filesystem operations |
| `std.env` | Environment variables, CLI args |
| `std.math` | Math functions |
| `std.time` | Time and sleep |

---

## Console I/O (built-ins, no import)

```zz
println("hello")
print("Enter name: ")
name := input("Enter name: ")
println("Hello, {name}")
```

---

## `std.str` -- String Operations

```zz
import std.str
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `str.length` | `str.length(s: str) -> int` | String length |
| `str.split` | `str.split(s: str, sep: str) -> [str]` | Split by separator |
| `str.contains` | `str.contains(s: str, sub: str) -> bool` | Check substring |
| `str.trim` | `str.trim(s: str) -> str` | Trim whitespace |
| `str.to_upper` | `str.to_upper(s: str) -> str` | Uppercase |
| `str.to_lower` | `str.to_lower(s: str) -> str` | Lowercase |
| `str.replace` | `str.replace(s: str, old: str, new: str) -> str` | Replace substring |
| `str.starts_with` | `str.starts_with(s: str, prefix: str) -> bool` | Check prefix |
| `str.ends_with` | `str.ends_with(s: str, suffix: str) -> bool` | Check suffix |

```zz
import std.str

s := "  Hello World  "
s.trim()                // "Hello World"
str.to_upper(s)         // "  HELLO WORLD  "
str.contains(s, "World")  // true
str.split("a,b,c", ",")   // ["a", "b", "c"]
str.replace("foo bar", "bar", "baz")  // "foo baz"
str.starts_with("hello", "he")  // true
str.ends_with("hello", "lo")    // true
```

### Method Call Syntax

String functions also support method syntax on a string value:

```zz
import std.str

"  hello  ".trim()          // "hello"
"hello".to_upper()          // "HELLO"
"hello world".to_lower()    // "hello world"
"hello world".contains("world")  // true
"hello world".split(" ")    // ["hello", "world"]
"hello world".replace("world", "zz")  // "hello zz"
"hello".starts_with("he")  // true
"hello".ends_with("lo")    // true
```

---

## `std.vec` -- Array Operations

```zz
import std.vec
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `vec.len` | `vec.len(v: [T]) -> int` | Array length |
| `vec.push` | `vec.push(v: [T], x: T) -> [T]` | New array with `x` appended |
| `vec.pop` | `vec.pop(v: [T]) -> [T]` | New array with last element removed |
| `vec.reverse` | `vec.reverse(v: [T]) -> [T]` | Reversed copy |
| `vec.join` | `vec.join(v: [T], sep: str) -> str` | Join as string |
| `vec.contains` | `vec.contains(v: [T], x: T) -> bool` | Check if element exists |
| `vec.sort` | `vec.sort(v: [T]) -> [T]` | Sorted copy |
| `vec.insert` | `vec.insert(v: [T], idx: int, x: T) -> [T]` | Insert at index |
| `vec.remove` | `vec.remove(v: [T], idx: int) -> [T]` | Remove at index |

```zz
import std.vec

nums := [3, 1, 2]
vec.push(nums, 4)         // [3, 1, 2, 4]
vec.pop(nums)             // [3, 1]
vec.sort(nums)            // [1, 2, 3]
vec.reverse(nums)         // [2, 1, 3]
vec.join(nums, ", ")      // "3, 1, 2"
vec.contains(nums, 1)     // true
vec.insert(nums, 0, 99)   // [99, 3, 1, 2]
vec.remove(nums, 1)       // [3, 2]
```

### Method Call Syntax

```zz
import std.vec

[3, 1, 2].sort()        // [1, 2, 3]
[3, 1, 2].reverse()     // [2, 1, 3]
[3, 1, 2].push(4)       // [3, 1, 2, 4]
[3, 1, 2].contains(1)   // true
["a", "b"].join(", ")   // "a, b"
```

---

## `std.json` -- JSON Parsing

```zz
import std.json
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `json.parse` | `json.parse(s: str) -> json` | Parse JSON string |
| `json.stringify` | `json.stringify(v: T) -> str` | Serialize to JSON |
| `json.get` | `json.get(j: json, key: str) -> json` | Get object field |
| `json.as_str` | `json.as_str(j: json) -> str` | Extract string |
| `json.as_int` | `json.as_int(j: json) -> int` | Extract integer |
| `json.as_float` | `json.as_float(j: json) -> float` | Extract float |
| `json.as_bool` | `json.as_bool(j: json) -> bool` | Extract boolean |

```zz
import std.json

data := json.parse({"name": "Alice", "age": 30})
name := json.as_str(json.get(data, "name"))   // "Alice"
age := json.as_int(json.get(data, "age"))     // 30

// Serialize
person := {"name": "Bob", "age": 25}
json.stringify(person)  // {"age":25,"name":"Bob"}
```

---

## `std.http` -- HTTP Server

```zz
import std.http
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `http.server` | `http.server() -> http.server` | Create server handle |
| `http.get` | `http.get(server, path, handler) -> http.server` | Register GET route |
| `http.post` | `http.post(server, path, handler) -> http.server` | Register POST route |
| `http.handle` | `http.handle(server, method, path, body) -> str` | Dispatch a request |
| `http.listen` | `http.listen(server, port) -> unit` | Start blocking server |

Handler type: `func(str) -> str`

```zz
import std.http

server := http.server()
    |> http.get(_, "/", |body: str| "Hello, World!")
    |> http.get(_, "/greet", |body: str| "Welcome!")
    |> http.post(_, "/echo", |body: str| body)

println("Server running on :8080")
http.listen(server, 8080)
```

### Testing Handlers

```zz
import std.http

server := http.server()
    |> http.get(_, "/", |body: str| "Hello!")

// Test without starting a server
response := http.handle(server, "GET", "/", "")   // "Hello!"
```

---

## `std.fs` -- Filesystem

```zz
import std.fs
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `fs.read_file` | `fs.read_file(path: str)` | Read file contents |
| `fs.write_file` | `fs.write_file(path: str, contents: str)` | Write file |
| `fs.exists` | `fs.exists(path: str) -> bool` | Check existence |
| `fs.read_bytes` | `fs.read_bytes(path: str)` | Raw bytes as `bytes` (contiguous, ~1x RSS) |
| `fs.append` / `fs.copy` / `fs.move` / `fs.rename` | `(…)` | Append, copy, move |
| `fs.is_file` / `fs.is_dir` | `(path) -> bool` | Type predicates |
| `fs.remove_file` / `fs.remove` | `(path)` | Delete a file |
| `fs.mkdir` / `fs.mkdir_all` | `(path)` | Create directories |
| `fs.read_dir` / `fs.readdir` | `(path)` | Child basenames (sorted) |
| `fs.remove_dir_all` / `fs.walk_dir` | `(path)` | Recursive remove / list |
| `fs.stat` | `(path)` | Metadata dict |
| `File.open` / `fs.open` | `(path, mode)` | Streaming handle (`r`/`w`/`a`) |
| `fs.read_chunk` | `(f, n)` | Text chunk (`""` at EOF; UTF-8) |
| `fs.read_chunk_bytes` | `(f, n)` | Binary-safe chunk as `bytes` (empty at EOF) |
| `fs.write_chunk` / `fs.seek` / `fs.flush` / `fs.close` | | Handle ops |
| `fs.normalize` | `(path) -> str` | Lexical normalize (OS separators, `.`/`..`, roots/UNC) |
| `fs.join` | `(a, b) -> str` | Join + normalize (`b` wins when absolute) |
| `fs.basename` / `fs.dirname` | `(path) -> str` | Final segment / directory part |
| `fs.is_absolute` | `(path) -> bool` | Rooted (`/x`, `C:\x`, `\\unc\…`) |
| `fs.extension` | `(path) -> str` | Extension without dot |
| `fs.osfs` / `fs.memfs` / `fs.tarfs` / `fs.embedfs` | | FS providers (see below) |
| `fs.read_to_string_at` / `fs.read_bytes_at` | `(fsys, path)` | Provider reads |
| `fs.write_at` / `fs.append_at` | `(fsys, path, data)` | Provider writes (Mem/Os only) |
| `fs.exists_at` / `fs.is_file_at` / `fs.is_dir_at` | `(fsys, path) -> bool` | Provider predicates |
| `fs.read_dir_at` / `fs.mkdir_all_at` / `fs.remove_file_at` | | Provider dir ops |

All fallible ops return `Result<_, str>` with unified
`fs:<op>:<code>: <path>` diagnostics (identical in VM and AOT).
`read_chunk` is text-oriented (lossy on arbitrary bytes by construction);
use `read_chunk_bytes` for binary streaming.

### `bytes` — contiguous byte buffers

`fs.read_bytes`, `fs.read_chunk_bytes`, and `fs.read_bytes_at` return
`bytes`: one contiguous buffer (~1 byte RSS per byte, slices share the
store with zero copies). Prints like an int array (`[104, 105]`).

```zz
match fs.read_bytes("data.bin") {
    .ok(b) => {
        println(len(b))    // or b.len()
        println(b[0])      // u8 as int (negatives wrap)
        println(b[1:4])    // zero-copy slice -> bytes
        println(typeof(b)) // bytes
        for x in b {       // iterate ints
            println(x)
        }
    }
    .err(e) => println(e),
}
```

Buffers are immutable (`b[i] = x` is an error) and serialize to JSON as
int arrays. `==` is deep in the VM; in AOT it matches array behavior.

### FS providers (`fs.FS` handle interface)

System calls accept any provider backing the handle:

```zz
import std.fs

match fs.memfs() {
    .ok(m) => {
        fs.write_at(m, "a/b.txt", "hello")
        match fs.read_to_string_at(m, "a/b.txt") {
            .ok(c)  => println(c),   // hello
            .err(e) => println(e),
        }
    }
    .err(e) => println(e),
}

// Read-only view over a plain .tar archive (no extraction):
match fs.tarfs("assets.tar") {
    .ok(t) => println(fs.exists_at(t, "docs/hi.txt")),
    .err(e) => println(e),
}
```

- `fs.osfs()` — the real OS filesystem.
- `fs.memfs()` — thread-safe in-memory tree (tests, caches, transient data).
- `fs.tarfs(path)` — read-only plain-`.tar` view (regular files + dirs;
  symlinks/devices skipped; writes fail `invalid_input`).
- `fs.embedfs()` — read-only `--embed` assets (empty unless the CLI was
  given `--embed <dir>`).

Paths inside a provider live in one `/`-rooted virtual namespace
(backslash accepted, `.`/`..` resolved, `..` above root clamps).

```zz
import std.fs

// Read
match fs.read_file("data.txt") {
    .ok(content)   => println(content),
    .err(e)        => println("Error: {e}"),
}

// Write
match fs.write_file("out.txt", "Hello World") {
    .ok(_)         => println("Written"),
    .err(e)        => println("Error: {e}"),
}

// Check existence
if fs.exists("config.toml") {
    println("config found")
}
```

---

## `std.env` -- Environment

```zz
import std.env
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `env.get` | `env.get(key: str) -> str?` | Value or `.none` |
| `env.get_var` | `env.get_var(name: str)` | Get env var (legacy alias shape) |
| `env.var` | `env.var(name: str)` | Value or `.err` |
| `env.set` | `env.set(key: str, val: str)` | Set (`.err` on bad key) |
| `env.remove` / `env.unset` | `env.remove(key: str)` | Unset (total no-op) |
| `env.vars` | `env.vars() -> map[str]str` | All variables, sorted by key |
| `env.cwd` | `env.cwd()` | Working directory (`.err` on failure) |
| `env.set_cwd` | `env.set_cwd(path: str)` | Change directory (`.err` on failure) |
| `env.exe_path` | `env.exe_path()` | Absolute path of this binary |
| `env.home_dir` | `env.home_dir() -> str?` | `HOME` / `USERPROFILE` |
| `env.temp_dir` | `env.temp_dir() -> str` | System temp dir (total) |
| `env.user` | `env.user() -> str?` | `USER`/`LOGNAME` / `USERNAME` |
| `env.os` | `env.os() -> str` | `"linux"` / `"macos"` / `"windows"` |
| `env.args` | `env.args() -> [str]` | Script arguments |

```zz
import std.env

// Environment variable
match env.get("HOME") {
    .some(home) => println("Home: {home}"),
    .none       => println("HOME not set"),
}

// Set + remove (process-wide, like POSIX setenv)
match env.set("APP_MODE", "demo") {
    .ok(_)  => println("set"),
    .err(e) => println(e),
}
env.remove("APP_MODE")

// Directories + identity
match env.cwd() {
    .ok(c)  => println(c),
    .err(e) => println(e),
}
println(env.os())
println(env.temp_dir())

// Command line args
for arg in env.args() {
    println(arg)
}
```

---

## `std.math` -- Math Functions

```zz
import std.math
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `math.abs` | `math.abs(v: T) -> T` | Absolute value |
| `math.floor` | `math.floor(v: float) -> int` | Floor to int |
| `math.ceil` | `math.ceil(v: float) -> int` | Ceil to int |
| `math.sqrt` | `math.sqrt(v: T) -> float` | Square root |
| `math.pow` | `math.pow(base: T, exp: T) -> float` | Power |
| `math.random` | `math.random() -> float` | Random [0, 1) |

```zz
import std.math

math.abs(-5)          // 5
math.floor(3.7)       // 3
math.ceil(3.2)        // 4
math.sqrt(9.0)        // 3.0
math.pow(2, 10)       // 1024.0
x := math.random()    // 0.0 <= x < 1.0
```

---

## `std.time` -- Time

```zz
import std.time
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `time.now_ms` | `time.now_ms() -> int` | Current time (ms since epoch) |
| `time.sleep_ms` | `time.sleep_ms(ms: int) -> unit` | Sleep for N milliseconds |

```zz
import std.time

start := time.now_ms()
time.sleep_ms(1000)
elapsed := time.now_ms() - start
println("Slept for {elapsed}ms")
```

---

## Variant Methods

These are available on variant values via method dispatch:

### Option Methods

| Method | Signature | Description |
|--------|-----------|-------------|
| `unwrap` | `.unwrap() -> T` | Unwrap or panic |
| `unwrap_or` | `.unwrap_or(default: T) -> T` | Unwrap or use default |
| `expect` | `.expect(msg: str) -> T` | Unwrap or panic with message |

```zz
x := .some(42)
x.unwrap()           // 42
x.unwrap_or(0)       // 42
x.expect("missing")  // 42

y: Option<int> = .none
y.unwrap_or(0)       // 0
```

### Result Methods

| Method | Signature | Description |
|--------|-----------|-------------|
| `unwrap` | `.unwrap() -> T` | Unwrap or panic |
| `unwrap_or` | `.unwrap_or(default: T) -> T` | Unwrap or use default |
| `expect` | `.expect(msg: str) -> T` | Unwrap or panic with message |

```zz
ok_val := .ok(42)
ok_val.unwrap()           // 42
ok_val.unwrap_or(0)       // 42

err_val: Result<int, str> = .err("boom")
err_val.unwrap_or(0)      // 0
```
