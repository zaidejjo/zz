# FS — `std.fs`

Comprehensive non-blocking filesystem. All fallible ops return `Result<_, str>` with unified `fs:<op>:<code>: <path>` diagnostics (identical on VM/AOT). Predicates return `bool`. Open handles are `Opaque("file")` and dispatch `file.*` methods. Pluggable providers: Os / Mem / Tar / Embed.

```zz
import std.fs
```

## Purpose

`std.fs` covers file read/write, directory ops, streaming handles, path lexing, and pluggable FS providers (Os / Mem / Tar / Embed). Reads share a process-wide stat-validated content cache. All paths use `/` separators internally (backslash accepted, `.`/`..` resolved).

## Quickstart

```zz
import std.fs

match fs.read("data.txt") {
    .ok(content)   => println(content),
    .err(e)        => println("Error: {e}"),
}
match fs.write("out.txt", "Hello World") {
    .ok(_)         => println("Written"),
    .err(e)        => println("Error: {e}"),
}
if fs.exists("config.toml") {
    println("config found")
}
```

## Bytes — contiguous buffers

`read_bytes`, `read_chunk_bytes`, `read_bytes_at` return `bytes`: one contiguous buffer (~1 byte RSS per byte), slices share zero-copy, prints like `[104, 105]`, immutable (`b[i] = x` errors), JSON as int arrays.

```zz
match fs.read_bytes("data.bin") {
    .ok(b) => {
        println(len(b))      // or b.len()
        println(b[0])        // u8 as int
        println(b[1:4])      // zero-copy slice -> bytes
        for x in b { println(x) }
    }
    .err(e) => println(e),
}
```

`read_chunk` is text (lossy on arbitrary bytes); use `read_chunk_bytes` for binary.

## Streaming handles

```zz
import std.fs

func main() -> Result<int, str> {
    match fs.open("log.txt", "a") {
        .ok(f) => {
            fs.write_chunk(f, "line\n")?
            fs.flush(f)?
            fs.close(f)?
        }
        .err(e) => println(e),
    }
    return .ok(0)
}
```

`fs.read_chunk(f, n)` (text, `""` at EOF), `fs.read_chunk_bytes(f, n)` (bytes, empty at EOF), `fs.write_chunk(f, data)`, `fs.seek(f, pos)`, `fs.flush(f)`, `fs.close(f)`.

## Path helpers (pure lexical, no I/O)

| Function | Description |
|----------|-------------|
| `fs.normalize(path)` | Lexical normalize (`.`/`..`, roots, separators) |
| `fs.join(a, b)` | Join + normalize (`b` wins when absolute) |
| `fs.basename(path)` | Final segment |
| `fs.dirname(path)` | Directory part |
| `fs.is_absolute(path)` | Rooted check (`/x`, `C:\x`, `\\unc\…`) |
| `fs.extension(path)` | Extension without dot |

## FS providers (`fs.FS` handle interface)

```zz
match fs.memfs() {
    .ok(m) => {
        fs.write_at(m, "a/b.txt", "hello")
        match fs.read_to_string_at(m, "a/b.txt") {
            .ok(c)  => println(c),
            .err(e) => println(e),
        }
    }
    .err(e) => println(e),
}
```

- `fs.osfs()` — real OS filesystem.
- `fs.memfs()` — thread-safe in-memory tree (tests, caches).
- `fs.tarfs(path)` — read-only plain `.tar` view (regular files + dirs; symlinks/devices skipped).
- `fs.embedfs()` — read-only `--embed` assets (empty unless CLI got `--embed <dir>`).

`*_at` ops: `read_to_string_at`, `read_bytes_at`, `write_at`, `append_at`, `exists_at`, `is_file_at`, `is_dir_at`, `read_dir_at`, `mkdir_all_at`, `remove_file_at`.

## Power helpers

- `fs.scan_counts(path, markers, bstart, bend, nested, whole)` — fused read + NUL-sniff + `str.classify` → `[lines, code, comments, blanks, binary]`.
- `fs.is_generated(path, markers)` — minified/generated-content heuristic.

## Embed at build time

```bash
zz build --embed ./assets main.zz
zz run --embed ./assets main.zz
```

## Exhaustive reference

| Function | Signature | Description |
|----------|-----------|-------------|
| `fs.read` | `fs.read(path: str) -> Result<str, str>` | Read file contents |
| `fs.read_bytes` | `fs.read_bytes(path: str) -> Result<bytes, str>` | Raw bytes |
| `fs.write` | `fs.write(path: str, content: str) -> Result<unit, str>` | Write file |
| `fs.append` | `fs.append(path: str, content: str) -> Result<unit, str>` | Append |
| `fs.copy` | `fs.copy(src: str, dst: str) -> Result<unit, str>` | Copy |
| `fs.move` | `fs.move(src: str, dst: str) -> Result<unit, str>` | Move/rename |
| `fs.exists` | `fs.exists(path: str) -> bool` | Existence |
| `fs.is_file` | `fs.is_file(path: str) -> bool` | File predicate |
| `fs.is_dir` | `fs.is_dir(path: str) -> bool` | Dir predicate |
| `fs.remove` | `fs.remove(path: str) -> Result<unit, str>` | Delete file |
| `fs.mkdir` | `fs.mkdir(path: str) -> Result<unit, str>` | Create dir |
| `fs.mkdir_all` | `fs.mkdir_all(path: str) -> Result<unit, str>` | Create dirs recursively |
| `fs.read_dir` | `fs.read_dir(path: str) -> Result<[str], str>` | Sorted basenames |
| `fs.remove_dir_all` | `fs.remove_dir_all(path: str) -> Result<unit, str>` | Recursive remove |
| `fs.walk_dir` | `fs.walk_dir(path: str) -> Result<[str], str>` | Recursive list |
| `fs.stat` | `fs.stat(path: str) -> Result<{str:str}, str>` | Metadata dict |
| `fs.scan_counts` | `fs.scan_counts(...) -> Result<[int], str>` | Fused classify |
| `fs.is_generated` | `fs.is_generated(path, markers) -> Result<bool, str>` | Generated heuristic |
| `fs.open` / `File.open` | `fs.open(path, mode) -> Result<file, str>` | Streaming handle (`r`/`w`/`a`) |
| `fs.read_chunk` | `fs.read_chunk(f: file, n: int) -> Result<str, str>` | Text chunk |
| `fs.read_chunk_bytes` | `fs.read_chunk_bytes(f: file, n: int) -> Result<bytes, str>` | Binary chunk |
| `fs.write_chunk` | `fs.write_chunk(f: file, data: str) -> Result<int, str>` | Write to handle |
| `fs.seek` | `fs.seek(f: file, pos: int) -> Result<int, str>` | Seek position |
| `fs.flush` | `fs.flush(f: file) -> Result<unit, str>` | Flush handle |
| `fs.close` | `fs.close(f: file) -> Result<unit, str>` | Close handle |
| `fs.normalize` | `fs.normalize(path: str) -> str` | Lexical normalize |
| `fs.join` | `fs.join(a: str, b: str) -> str` | Join + normalize |
| `fs.basename` | `fs.basename(path: str) -> str` | Final segment |
| `fs.dirname` | `fs.dirname(path: str) -> str` | Directory part |
| `fs.is_absolute` | `fs.is_absolute(path: str) -> bool` | Rooted check |
| `fs.extension` | `fs.extension(path: str) -> str` | Extension without dot |
| `fs.osfs` | `fs.osfs() -> Result<zzfs, str>` | OS provider |
| `fs.memfs` | `fs.memfs() -> Result<zzfs, str>` | Memory provider |
| `fs.tarfs` | `fs.tarfs(path: str) -> Result<zzfs, str>` | Tar provider |
| `fs.embedfs` | `fs.embedfs() -> Result<zzfs, str>` | Embed provider |
| `fs.read_to_string_at` | `fs.read_to_string_at(fsys, path) -> Result<str, str>` | Provider read |
| `fs.read_bytes_at` | `fs.read_bytes_at(fsys, path) -> Result<bytes, str>` | Provider read bytes |
| `fs.write_at` | `fs.write_at(fsys, path, content) -> Result<unit, str>` | Provider write |
| `fs.append_at` | `fs.append_at(fsys, path, content) -> Result<unit, str>` | Provider append |
| `fs.exists_at` | `fs.exists_at(fsys, path) -> bool` | Provider exists |
| `fs.is_file_at` | `fs.is_file_at(fsys, path) -> bool` | Provider is_file |
| `fs.is_dir_at` | `fs.is_dir_at(fsys, path) -> bool` | Provider is_dir |
| `fs.read_dir_at` | `fs.read_dir_at(fsys, path) -> Result<[str], str>` | Provider read_dir |
| `fs.mkdir_all_at` | `fs.mkdir_all_at(fsys, path) -> Result<unit, str>` | Provider mkdir_all |
| `fs.remove_file_at` | `fs.remove_file_at(fsys, path) -> Result<unit, str>` | Provider remove file |

## Method syntax

File handles dispatch method calls:

```zz
import std.fs

func main() -> Result<int, str> {
    f := fs.open("log.txt", "a")?
    f.write_chunk("line\n")?
    f.flush()?
    f.close()?
    return .ok(0)
}
```

## Gotchas

- All fallible ops return `fs:<op>:<code>: <path>` diagnostics — identical on VM/AOT.
- **Reads are cached** (stat-validated); writes/removals invalidate eagerly; outside writes detected by len/mtime.
- **`read_chunk` is lossy** on arbitrary bytes — use `read_chunk_bytes` for binary.
- **`bytes` are immutable** — `b[i] = x` is an error.
- **`fs.rename` was removed** — use `fs.move`.

## Production patterns

```zz
import std.fs

// Atomic-ish write: write to temp, then move
func write_atomic(path: str, content: str) -> Result<unit, str> {
    tmp := "{path}.tmp"
    return match fs.write(tmp, content) {
        .ok(_) => fs.move(tmp, path),
        .err(e) => .err(e),
    }
}

func main() -> Result<int, str> {
    // In-memory test fs
    match fs.memfs() {
        .ok(mem) => {
            fs.write_at(mem, "config.toml", "key = 'val'")?
            println(fs.read_to_string_at(mem, "config.toml")?)
        }
        .err(e) => println(e),
    }
    return .ok(0)
}
```

