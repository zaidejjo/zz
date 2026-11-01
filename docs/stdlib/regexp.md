# Regexp — `std.regexp`

Compiled patterns are opaque `"regexp"` handles, so method syntax works. Backed by the same engine on VM and AOT. `compile` is fallible (bad patterns yield `.err`, never panic).

```zz
import std.regexp
```

## What it does

`std.regexp` compiles regex patterns into reusable handles, then matches, finds, replaces, and captures. Compile once, reuse many times — handles are cheap to query, compilation is the cost.

## Quickstart

```zz
import std.regexp

match regexp.compile(`^[0-9a-f]{8}$`) {
    .ok(re) => {
        println(re.is_match("deadbeef"))  // true
        match re.find("xx deadbeef yy") {
            .some(m) => println("found {m}"),
            .none    => println("no match"),
        }
        println(re.replace_all("a1 b2", "X"))  // "aX bX"
        println(re.captures("2024-02-29"))     // groups, "" for unmatched, [] on no match
    },
    .err(e) => println("bad pattern: {e}"),
}
```

## Method syntax

Regexp handles dispatch method calls:

```zz
import std.regexp

func main() -> Result<int, str> {
    re := regexp.compile(`\d+`)?
    re.is_match("abc123")    // true
    re.find("abc123")        // .some("123")
    re.replace_all("a1b2", "X")  // "aXbX"
    re.captures("2024-02-29")   // ["2024-02-29", ...]
    return .ok(0)
}
```

## Exhaustive table

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `regexp.compile` | `regexp.compile(pat: str) -> Result<regexp, str>` | Compile pattern (`.err` on bad pattern) | `regexp.compile(`\d+`)` |
| `regexp.is_match` | `regexp.is_match(re: regexp, s: str) -> bool` | Full match? | `re.is_match("hi")` |
| `regexp.find` | `regexp.find(re: regexp, s: str) -> Option<str>` | First match | `re.find("hi")` |
| `regexp.replace_all` | `regexp.replace_all(re: regexp, s: str, rep: str) -> str` | Replace all matches | `re.replace_all("a1","X")` |
| `regexp.captures` | `regexp.captures(re: regexp, s: str) -> [str]` | Group list (`""` unmatched, `[]` no match) | |
| `regexp.is_email` | `regexp.is_email(s: str) -> bool` | Email convenience predicate | |

## Regexp constructor

| Function | Signature | Description |
|----------|-----------|-------------|
| `Regexp.new` | `Regexp.new(pat: str) -> Result<regexp, str>` | Constructor namespace spelling |

## Tips

- Prefer raw backticks for patterns: `` `^[0-9a-f]{8}$` `` needs no `{{ }}` doubling.
  In `"..."` strings, quantifier braces must double (`"{{8}}"`) like Rust `format!`.
- Compile once, reuse: handles are cheap to query, compilation is the cost.
- Handles can go stale across weird lifecycles — calls then error
  `` `...`: regexp handle is no longer valid `` instead of hanging.
- For byte-offset search without regex, [`std.str`](str.md) `find`/`rfind`/`*_in` is faster.

