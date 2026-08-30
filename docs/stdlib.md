# Standard Library

Exhaustive reference of all built-in functions and modules.

## Built-in Functions

Available without imports:

| Function | Signature | Description |
|----------|-----------|-------------|
| `print` | `print(v: T) -> unit` | Print value without newline |
| `println` | `println(v: T) -> unit` | Print value with newline |
| `input` | `input() -> str` | Read line from stdin |
| `typeof` | `typeof(v: T) -> str` | Runtime type name |
| `str` | `str(v: T) -> str` | Convert to string |
| `int` | `int(v: T)` | Parse/convert to int (`.none` on failure) |
| `float` | `float(v: T)` | Parse/convert to float (`.none` on failure) |
| `len` | `len(v: T) -> int` | Length of array, string, dict, or range |
| `range` | `range(start: int, stop: int, step: int)` | Create integer range |
| `map` | `map(arr: [T] \| T.., f: func(T) -> U) -> [U]` | Apply function to each element |
| `filter` | `filter(arr: [T] \| T.., f: func(T) -> bool) -> [T]` | Keep elements where predicate is true |
| `enumerate` | `enumerate(arr: [T] \| T..)` | Index + value pairs |
| `zip` | `zip(a: [T] \| T.., b: [U] \| U..)` | Pair elements from two iterables |

## Module Index

| Module | Purpose |
|--------|---------|
| `std.io` | Console I/O |
| `std.str` | String manipulation |
| `std.vec` | Array operations |
| `std.json` | JSON parsing/serialization |
| `std.http` | HTTP server |
| `std.fs` | Filesystem operations |
| `std.env` | Environment variables, CLI args |
| `std.math` | Math functions |
| `std.time` | Time and sleep |

---

## `std.io` -- Console I/O

```zz
import std.io
```

| Function | Signature | Description |
|----------|-----------|-------------|
| `io.printz` | `io.printz(v: T) -> unit` | Print without newline |
| `io.println` | `io.println(v: T) -> unit` | Print with newline |
| `io.read_line` | `io.read_line() -> str` | Read line from stdin |

```zz
import std.io

io.printz("Enter name: ")
name := io.read_line()
io.println("Hello, {name}")
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
| `env.get_var` | `env.get_var(name: str)` | Get env var |
| `env.args` | `env.args() -> [str]` | Script arguments |

```zz
import std.env

// Environment variable
match env.get_var("HOME") {
    .some(home) => println("Home: {home}"),
    .none       => println("HOME not set"),
}

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
