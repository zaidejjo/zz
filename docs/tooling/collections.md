# Collections — directory-based typed registries

A **collection** turns a directory into a typed registry: every `.zz` file
directly inside it is a member, and the compiler synthesizes `get` / `all`
accessors. There is no index file and no manual registration.

```toml
# zz.toml
[collect.commands]
dir  = "commands"
sig  = "func(args: [str]) -> int"   # every member exports `pub run` with this shape
name = "file"                       # optional, and the only supported value in v1
export = "run"                      # optional; the required function name
```

```
commands/
  build.zz      # pub func run(args: [str]) -> int { ... }
  deploy.zz
  greet.zz
```

```zz
// src/main.zz
import collect.commands

func main() {
    for entry in commands.all() {
        println(entry.name)       // "build", "deploy", "greet"
    }

    match commands.get("greet") {
        .some(entry) => entry.run(["Ada"]),
        .none        => println("no such command"),
    }
}
```

## Generated module

The loader synthesizes a normal module before checking:

```zz
pub struct Entry {
    name: str,
    run: func([str]) -> int,
}

pub const ALL: [Entry] = [ ... ]   // sorted by member name
pub func all() -> [Entry]
pub func get(name: str) -> Option<Entry>
```

Because both engines consume the same synthesized AST, VM and AOT runs
produce identical output. The module is namespaced like any import, so
`import collect.commands as c` makes it `c.all()`.

`get` misses return `.none`; use `??` or `match` to handle them. There is
no `require()` in v1.

## Discovery and ordering

- Only `.zz` files **directly** inside `dir` are members. Nested
  directories are ignored (nested collections are a follow-up).
- Member names come from file stems (`greet.zz` → `"greet"`), with `-`
  normalized to `_` (`deploy-prod.zz` → `"deploy_prod"`).
- Members are sorted alphabetically by name, so `all()` is deterministic.
- Adding or removing a file changes the collection on the next run/check;
  no other edit is needed.
- An empty directory is valid: `all()` returns `[]`.

## Validation

Each member must export `pub <export>` (default `run`) with exactly the
declared signature. Mismatches fail `zz check` at the member file:

```
error: commands/deploy.zz does not match collection `commands`
  expected: func run(args: [str]) -> int
  found:    func run(args: int) -> int
  hint: change `run` in `deploy.zz` to `func run(args: [str]) -> int`
```

A missing export gets the same shape with `found: pub run is missing`.

Manifest errors are reported while loading the root manifest, even if no
file imports the collection yet:

- `dir` must exist, be relative, and stay inside the package (symlink
  escapes are rejected).
- `sig` must parse as a function type through the real grammar. Both
  `func([str]) -> int` and `func(args: [str]) -> int` are accepted.
- `name` must be `"file"`; `export` must be a valid identifier.
- Two files whose names normalize to the same member
  (`foo-bar.zz` + `foo_bar.zz`) are an error.
- A file stem that is not a valid identifier (`9lives.zz`) is an error.

## Editor support

With the LSP running:

- `import collect.` completes declared collection names.
- `commands.` completes `get`, `all`, and `Entry`.
- `commands.get("…")` completes member names, filtered by the typed prefix.
- Go-to-definition on `"greet"` (or on the import path) opens
  `commands/greet.zz`.

Completion and navigation read `zz.toml` and the directory from disk, so
new files appear without restarting the server.

## Limits (v1)

- Root package collections only. A dependency importing its own
  `[collect.*]` gets a “not supported yet” diagnostic.
- Single-level directories, one signature, one export per member.
- No custom ordering or priority.

A runnable project lives at
[`tests/fixtures/collections/`](../../tests/fixtures/collections/):
`zz run tests/fixtures/collections/src/main.zz`.
