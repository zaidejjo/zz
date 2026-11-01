# Path — `std.path`

Pure-ZZ lexical path helpers. No I/O — pure string manipulation, OS-aware separator style. Mirrors `std.fs` path functions for backward compatibility.

```zz
import std.path
```

## Purpose

`std.path` resolves, joins, and inspects paths without touching the disk. Separator style follows `std.env.os()` (`"windows"` → `\`, else `/`). Use for building paths before passing them to `std.fs`.

## Quickstart

```zz
import std.path

path.join("a", "b.txt")         // a/b.txt (+ normalize)
path.join_all(["a", "b", "c"])  // a/b/c
path.normalize("a/./b/../c")    // a/c
path.basename("a/b.txt")        // b.txt
path.dirname("a/b.txt")         // a
path.is_absolute("/x")          // true (also C:\x, \\unc\…)
path.extension("a.tar.gz")      // gz (no dot)
```

`path.join(a, b)` normalizes: `b` wins when absolute — `join(dir, "f.txt")`, not `join(dir, "/f.txt")`.

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `path.join` | `path.join(a: str, b: str) -> str` | Join + normalize (`b` wins if absolute) | `path.join("a","b.txt")` → `"a/b.txt"` |
| `path.join_all` | `path.join_all(parts: [str]) -> str` | Join many parts | `path.join_all(["a","b","c"])` → `"a/b/c"` |
| `path.normalize` | `path.normalize(path: str) -> str` | Lexical normalize (`.`/`..`, roots) | `path.normalize("a/./b/../c")` → `"a/c"` |
| `path.basename` | `path.basename(path: str) -> str` | Final segment | `path.basename("a/b.txt")` → `"b.txt"` |
| `path.dirname` | `path.dirname(path: str) -> str` | Directory part | `path.dirname("a/b.txt")` → `"a"` |
| `path.is_absolute` | `path.is_absolute(path: str) -> bool` | Rooted (`/x`, `C:\x`, `\\unc\…`) | `path.is_absolute("/x")` → true |
| `path.extension` | `path.extension(path: str) -> str` | Extension without dot | `path.extension("a.tar.gz")` → `"gz"` |

## Edge cases & gotchas

- **Purely lexical** — never checks the filesystem. A path may be "valid" here and still not exist on disk.
- **`..` above root clamps**; `.` segments are dropped.
- **Backslashes accepted** on all platforms; canonical separator follows `std.env.os()`.
- **`join` semantics:** `b` wins when absolute — `join(dir, "/f.txt")` returns `/f.txt`.

