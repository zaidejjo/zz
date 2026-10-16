# Filesystem and bytes — `std.fs`

```zz
import std.fs
```

All fallible ops return `Result<_, str>` as `fs:<op>:<code>: <path>`, identical on VM/AOT.

## Everyday files

```zz
match fs.read("data.txt") {
    .ok(c)  => println(c),
    .err(e) => println(e),
}
match fs.write("out.txt", "hi") {
    .ok(_)  => println("written"),
    .err(e) => println(e),
}
if fs.exists("config.toml") {
    println("found")
}
```

`append`, `copy`, `move`, `remove`, `is_file`/`is_dir`,
`mkdir`/`mkdir_all`, `read_dir` (sorted basenames), `remove_dir_all`,
`walk_dir`, `stat`, path helpers below.

## Bytes — contiguous buffers

`read_bytes`, `read_chunk_bytes`, `read_bytes_at` return `bytes`:
one contiguous buffer (~1 byte RSS per byte), slices share zero-copy,
prints like `[104, 105]`, immutable (`b[i] = x` errors), JSON as int arrays.

```zz
match fs.read_bytes("data.bin") {
    .ok(b) => {
        println(len(b))    // or b.len()
        println(b[0])      // u8 as int
        println(b[1:4])    // zero-copy slice
        for x in b { println(x) }
    },
    .err(e) => println(e),
}
```

`read_chunk` is text (lossy on arbitrary bytes); use `read_chunk_bytes` for binary.
`==` is deep in VM and matches array behavior in AOT.

## Streaming handles

```zz
// r / w / a modes
// read_chunk(f, n): text, "" at EOF
// read_chunk_bytes(f, n): bytes, empty at EOF
// write_chunk / seek / flush / close
```

## Paths — lexical, OS-aware

`normalize`, `join(a, b)` + normalize (`b` wins when absolute — use
`join(dir, "file.txt")`, not `join(dir, "/file.txt")`),
`basename`, `dirname`, `is_absolute` (`/x`, `C:\x`, `\\unc\…`),
`extension` (no dot). Pure-ZZ `std.path.*` mirrors these.

## Providers — one `FS` handle interface

```zz
match fs.memfs() {
    .ok(m) => {
        fs.write_at(m, "a/b.txt", "hello")
        match fs.read_to_string_at(m, "a/b.txt") {
            .ok(c)  => println(c),
            .err(e) => println(e),
        }
    },
    .err(e) => println(e),
}
```

- `osfs()` — real OS fs.
- `memfs()` — thread-safe in-memory tree (tests, caches).
- `tarfs(path)` — read-only plain `.tar` view (files+dirs, symlinks skipped).
- `embedfs()` — read-only `--embed` assets (empty unless CLI got `--embed <dir>`).

Paths inside a provider are `/`-rooted (`\` accepted, `.`/`..` resolved, `..` clamps).
`*_at` ops: `read_to_string_at`, `read_bytes_at`, `write_at`, `append_at`,
`exists_at`, `is_file_at`, `is_dir_at`, `read_dir_at`, `mkdir_all_at`, `remove_file_at`.

## Power helpers

- `scan_counts(path, markers, bstart, bend, nested, whole)` — fused read + NUL-sniff
  + `str.classify` → `[lines, code, comments, blanks, binary]`.
- `is_generated(path, markers)` — minified/generated heuristic.

Embed at build time:

```bash
zz build --embed ./assets main.zz
zz run --embed ./assets main.zz
```

```zz
match fs.embedfs() {
    .ok(e) => match fs.read_to_string_at(e, "index.txt") {
        .ok(c)  => println(c),
        .err(x) => println(x),
    },
    .err(x) => println(x),
}
```
