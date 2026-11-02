# Cheatsheet — one page, zero fluff

## New Project

```bash
# Create a new project in a fresh folder (templates: cli | lib | web | plugin)
zz new my-app     # also you can use the --template cli
cd my-app

# Or initialize zz.toml + src/main.zz in the current folder
zz init           # also you can use --template cli

# Add a dependency and install it + zz.lock
zz add rand@1.2.3

# install the dependency in the zz.toml file
zz install

# Health check (run after install)
zz setup          # wire PATH + shell completions (bash/zsh/fish/powershell)
zz doctor         # audit binary, clang, shell, git, registry
zz doctor --fix   # auto-fix what it can
```

## Run / Build / Eval

```bash
zz run                      # type-check + run src/main.zz (VM, no artifacts)
zz run hello.zz             # run a single file (VM)
zz run --watch hello.zz     # hot-reload on save (VM only)

zz eval "1 + 2"             # evaluate an expression, prints 3

zz build hello.zz           # compile a native binary (needs clang or zig cc)
zz build --release hello.zz # optimized native binary
```

## Check / Fix / Format

```bash
zz check hello.zz   # type-check, rich errors + suggestions (fast, cached)
zz check src/       # check a whole directory

zz fix hello.zz     # apply safe auto-fixes (unused imports, vars, functions...)

zz fmt hello.zz     # format in place
zz fmt -c src/      # check formatting only (CI-friendly)
```

## Tests / Benchmarking

```bash
zz test           # run @test functions (uses ./tests/ if it holds .zz, else .)
zz test hello.zz  # test a single file

zz bench          # run @bench functions and report timings
zz cover hello.zz # run a program and report line coverage
```

## Vars / Funcs / Closures / Pipelines

```zz
// `:=` infers the type; `: T =` pins an explicit type.
x := 42              // x: int
pi: float = 3.14     // pi: float

// Named functions return through `return`.
func add(a: int, b: int) -> int {
    return a + b
}

// Closures are anonymous functions; `|_|` ignores its argument.
f := |x: int| x * 2     // f: func(int) -> int
println(f(21))          // 42

// Pipelines pass the left value as the first argument to the right call.
result := "  Hello World  "
    |> str.trim()       // "Hello World"
    |> str.to_upper()   // "HELLO WORLD"
```

## Strings

```zz
name := "zaidejjo"
pi := 3.14159

println("hi {name}")          // hi zaidejjo  (interpolation)
print("{{name}}")             // {name}      (escaped brace)
print(`raw {"a": 1}`)         // raw {"a": 1} (backtick = raw, no interpolation)
print("PI: {pi:.2f}")         // PI: 3.14    (format specifier)
print("  hi  ".trim().to_upper())  // HI        (method chaining)
```

## Collections

```zz
// Arrays are values; slicing is [start:end], negative index counts from the end.
arr := [1, 2, 3, 4, 5]
arr[1:3]      // [2, 3]   (elements at index 1 and 2)
arr[-1]       // 5        (last element)
arr[0]        // 1        (first element)

// Dicts are {key: value}; lookup returns the value directly.
d := {"a": 1, "b": 2}
d["a"]        // 1

// Tuples are fixed-size, heterogeneous groups.
p := (7, "seven")     // (int, str)
p[0]                  // 7
p[1]                  // "seven"

// Ranges: range(stop), range(start, stop), range(start, stop, step).
range(5)              // 0..5  = [0, 1, 2, 3, 4]
range(1, 5)           // 1..5  = [1, 2, 3, 4]
range(1, 10, 2)       // [1, 3, 5, 7, 9]

// Comprehension: build a new array with a filter.
evens := [x * 2 for x in range(10) if x % 2 == 0]  // [0, 4, 8, 12, 16]

// Pipeline over collections (each step passes left as first arg).
range(5) |> map(|x| x * 2) |> filter(|x| x > 4)    // [6, 8]
```

## Match / Errors / Fallbacks

```zz
// `match` must handle every case.
s := "42"
match int(s) {
    .some(v) => println(v),      // parsed OK
    .none    => println("bad"),  // parse failed
}

// `?` propagates a Result/Option error out of the current function.
func parse_age(s: str) -> Option<int> {
    val := int(s)?                   // returns early if .none
    return .some(val)
}

// `??` falls back to a default when the value is .none.
port := int("nope") ?? 80            // 80 if int("nope") is .none
```

## Structs / Enums

```zz
struct Point { x: int, y: int }
p := Point{ x: 1, y: 2 }     // construct with named fields
p.x                          // 1

enum T { Eof, IntLit(int) }   // unit variant + payload variant
t := T.IntLit(42)

match t {
    .Eof        => println("end"),
    .IntLit(n)  => println("value {n}"),
}
```

## Modules

```zz
import std.math                  // import the stdlib module. math.PI, math.pow(), ...
import app.math                  // import a package module as math.*
import app.math(add as f)       // selective import: bring `add` in as `f`
```

## HTTP

```zz
import std.http

// A complete one-file HTTP server.
func main() {
    s := http.server()                                    // create server handle
    s = s.route("GET", "/", |req| "ok")                   // route GET / to a handler
    s = s.route("GET", "/users/:id",                      // route with a path param
        |req| "user-{http.param(req, "id") ?? "?"}")      // read the :id param with fallback
    http.listen(s, 8080)                                  // blocks; graceful drain on SIGINT/SIGTERM
}
```

## Filesystem (fs)

```zz
import std.fs

// Read a file — fallible, so handle both .ok and .err.
match fs.read("f.txt") {
    .ok(content) => println(content),   // success path
    .err(e)      => println("read failed: {e}"),  // error path
}
```

## Concurrency (chan + task)

```zz
import std.chan
import std.task

// Channels: unbounded, lock-free, never block the sender.
c := chan()
chan.send(c, 42)                      // push a value
v: int = chan.recv(c)                 // blocks until a value arrives

// Tasks: green threads with snapshot isolation (no shared mutable state).
h := task.spawn(|_| { 42 })           // spawn a green thread
r: int = task.join(h)                 // wait for the result — 42
```

## Where next

- Learning: [Language overview](../language/overview.md).
- Tables: [Operators](../language/operators.md), [Stdlib overview](../stdlib/overview.md).
- Stuck: [Error catalog](../guides/errors.md), [FAQ & glossary](../guides/faq-glossary.md).
- Full CLI: [CLI reference](../tooling/cli-reference.md).
