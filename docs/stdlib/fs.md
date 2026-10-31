# `std.fs` — filesystem, providers, and file handles

Read/write files, create/remove directories, traverse trees, and work with
pluggable filesystems (`zzfs`): OS, in-memory, embedded, and tar backends.

```zz
import std.fs

match fs.write("notes.txt", "hello") {
    .ok(_)  => println("written"),
    .err(e) => println(e),
}
match fs.read("notes.txt") {
    .ok(c)  => println(c),
    .err(e) => println(e),
}
match fs.read_dir(".") {
    .ok(names) => for n in names { println(n) },
    .err(e)    => println(e),
}
match fs.mkdir("cache") {
    .ok(_)  => {},
    .err(e) => println(e),
}
fs.remove("notes.txt")?   // propagate in Result-returning functions
```

## Types

| Type | Notes |
|---|---|
| `zzfs` | Opaque filesystem handle. Created by `osfs`/`memfs`/`embedfs`/`tarfs`; passed as first arg to `*_at` variants. |
| `file` | Opaque file handle from `fs.open`. Use `read_chunk`/`read_chunk_bytes`/`write_chunk`/`seek`/`flush`/`close`. |

## Variables

None.

## Functions — plain-path (OS filesystem)

| Function | Signature | Notes |
|---|---|---|
| `read(path)` | `read(path: str) -> Result<str, str>` | Read entire file as text. |
| `read_bytes(path)` | `read_bytes(path: str) -> Result<bytes, str>` | Read entire file as raw bytes. |
| `write(path, content)` | `write(path: str, content: str) -> Result<unit, str>` | Create/overwrite with text. |
| `append(path, content)` | `append(path: str, content: str) -> Result<unit, str>` | Append text. |
| `exists(path)` | `exists(path: str) -> bool` | Whether `path` exists. |
| `is_file(path)` | `is_file(path: str) -> bool` | Whether `path` is a regular file. |
| `is_dir(path)` | `is_dir(path: str) -> bool` | Whether `path` is a directory. |
| `is_absolute(path)` | `is_absolute(path: str) -> bool` | Whether `path` is absolute. |
| `basename(path)` | `basename(path: str) -> str` | Final path component. |
| `dirname(path)` | `dirname(path: str) -> str` | Parent directory. |
| `extension(path)` | `extension(path: str) -> str` | File extension. |
| `join(a, b)` | `join(a: str, b: str) -> str` | Join two path components. |
| `normalize(path)` | `normalize(path: str) -> str` | Canonicalize `.`/`..`. |
| `remove(path)` | `remove(path: str) -> Result<unit, str>` | Remove a file. |
| `remove_dir_all(path)` | `remove_dir_all(path: str) -> Result<unit, str>` | Recursively remove a dir. |
| `mkdir(path)` | `mkdir(path: str) -> Result<unit, str>` | Create a directory. |
| `mkdir_all(path)` | `mkdir_all(path: str) -> Result<unit, str>` | Create dir + parents. |
| `copy(src, dst)` | `copy(src: str, dst: str) -> Result<unit, str>` | Copy a file. |
| `move(src, dst)` | `move(src: str, dst: str) -> Result<unit, str>` | Rename/move. |
| `read_dir(path)` | `read_dir(path: str) -> Result<[str], str>` | List directory entries. |
| `stat(path)` | `stat(path: str) -> Result<{str: str}, str>` | File metadata dict. |
| `walk_dir(path)` | `walk_dir(path: str) -> Result<[str], str>` | Recursively list all paths. |
| `scan_counts(path, markers, bstart, bend, nested, whole)` | `scan_counts(...) -> Result<[int], str>` | Count marker occurrences in a file. |

## Functions — provider/handle (`*_at`)

| Function | Signature | Notes |
|---|---|---|
| `osfs()` | `osfs() -> Result<zzfs, str>` | OS filesystem provider. |
| `memfs()` | `memfs() -> Result<zzfs, str>` | In-memory filesystem. |
| `embedfs()` | `embedfs() -> Result<zzfs, str>` | Embedded (compile-time) FS. |
| `tarfs(path)` | `tarfs(path: str) -> Result<zzfs, str>` | Tar archive as a filesystem. |
| `open(path, mode)` | `open(path: str, mode: str) -> Result<file, str>` | Open a file handle. |
| `read_at(fsys, path)` | `read_at(fsys: zzfs, path: str) -> Result<str, str>` | Read via a provider. |
| `read_bytes_at(fsys, path)` | `read_bytes_at(fsys: zzfs, path: str) -> Result<bytes, str>` | Read bytes via a provider. |
| `read_to_string_at(fsys, path)` | `read_to_string_at(fsys: zzfs, path: str) -> Result<str, str>` | Read to string via provider. |
| `write_at(fsys, path, content)` | `write_at(fsys: zzfs, path: str, content: str) -> Result<unit, str>` | Write via provider. |
| `exists_at(fsys, path)` | `exists_at(fsys: zzfs, path: str) -> bool` | Exists via provider. |
| `is_dir_at(fsys, path)` / `is_file_at(fsys, path)` | `(fsys: zzfs, path: str) -> bool` | Type checks via provider. |
| `mkdir_all_at(fsys, path)` | `mkdir_all_at(fsys: zzfs, path: str) -> Result<unit, str>` | Mkdir via provider. |
| `remove_file_at(fsys, path)` | `remove_file_at(fsys: zzfs, path: str) -> Result<unit, str>` | Remove via provider. |
| `read_dir_at(fsys, path)` | `read_dir_at(fsys: zzfs, path: str) -> Result<[str], str>` | List via provider. |

## Functions — file handle

| Function | Signature | Notes |
|---|---|---|
| `close(f)` | `close(f: file) -> Result<unit, str>` | Close a file handle. |
| `flush(f)` | `flush(f: file) -> Result<unit, str>` | Flush buffered writes. |
| `seek(f, pos)` | `seek(f: file, pos: int) -> Result<int, str>` | Seek to byte position. |
| `read_chunk(f, n)` | `read_chunk(f: file, n: int) -> Result<str, str>` | Read up to `n` bytes as text. |
| `read_chunk_bytes(f, n)` | `read_chunk_bytes(f: file, n: int) -> Result<bytes, str>` | Read up to `n` raw bytes. |
| `write_chunk(f, data)` | `write_chunk(f: file, data: str) -> Result<int, str>` | Write text, returns bytes written. |
