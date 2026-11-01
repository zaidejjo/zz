# CLI — `run`, `build`, `eval`, `watch`, maintenance

## `zz run`

VM by default (no artifacts). `--native` builds via cached Clang pipeline and runs.

```bash
zz run hello.zz
zz run --native hello.zz
zz run --embed ./assets main.zz
zz eval "1 + 2"            # 3
zz eval "println('hi')"    # hi + ()
```

Inside a project the file defaults to the entry (`src/main.zz`).

`--bytecode` always executes the chunk VM (never the tree walker).
Otherwise the [run cache](../cli.md) picks the engine: a warm entry runs
the cached IR in the chunk VM, a miss runs the tree walker and stores IR
for next time. `ZZ_RUN_CACHE=0` forces the walker, `ZZ_VERBOSE=1` prints
hit/miss lines, `zz cache clean` wipes the cache (`zz cache gc` collects
unused CAS entries only).

## `zz build` — single Clang backend

Every build is a real native binary; needs `clang` 18+ or `zig` on PATH.
`zz run` without `--native` is the only VM path.

```bash
zz build main.zz            # standalone -> ./main
zz build                    # project -> <root>/bin/<pkg-name>
zz build -p main.zz          # release -O3 -flto=thin, cached ~/.zz/cache
zz build -p --full main.zz   # max LTO; with -- <args> adds PGO training run
zz build --static main.zz    # self-contained (not on macOS)
zz build --dynamic main.zz   # fast -O0 -g
zz build -o server main.zz   # rename
zz build --pgo main.zz       # profile-guided, native host only
zz build --target aarch64-unknown-linux-gnu main.zz  # cross (implies -p)
zz build -p --cc zig main.zz
zz build -p --verbose main.zz
zz build --embed ./assets main.zz
```

Static default skips phantom syslibs: no-fetch programs skip libcurl, no-query
skip libsqlite3. Cross adds `-fuse-ld=lld` (+ `-lws2_32` on Windows),
uses safe baseline CPU, rejects `--pgo` on foreign + `--static` on macOS.
All builds use `-fwrapv`, no `-ffast-math`, no `-march=native` — VM/native agree.
No provider? Build still emits `app.c` + build scripts for manual clang.

Output rule: standalone → `./<stem>`; project → `<root>/bin/<name>`;
`--target` adds `-<triple>`; `-o` renames. Publishes atomically; `run --native`
execs a private staged copy so parallel publish never swaps mid-exec.

### Build phases

Every build reports five generic steps on stderr — the same five for the HIR
and chunk backends, so the wording never goes stale:

```
building demopkg (static)
==> [1/5] Checking: 12 modules (10 cached)
==> [2/5] Optimizing: 512 functions
==> [3/5] Compiling: 14 files (3 cached)
==> [4/5] Linking: 14 files
==> [5/5] Publishing: bin/demopkg (1.2 MB)
✓ built bin/demopkg (static, 1.2 MB)
```

The header names the target: the `zz.toml` package for a project build
(`building demopkg (static)`, with the entry appended when it is not the
conventional `src/main.zz`), the file for a loose source
(`building hello.zz (static)`). On a TTY each step is one line rewritten in
place with a live counter and elapsed time; piped stderr gets one plain line
per step. Unchanged source stops before type-checking
(`==> cached · source unchanged` plus the `Publishing` step) and needs no
`clang` on `PATH`.

Colour is on by default on a TTY: `ZZ_COLOR=never` turns it off,
`ZZ_COLOR=always` forces ANSI through a pipe, `NO_COLOR` / `TERM=dumb` always
win, `auto` (default) means TTY only. `ZZ_BUILD_CACHE=0` forces a real build
(no cache probe, no reuse). `--verbose` adds timing lines; on a cache hit it
names the reused entry (`zz: cache hit <path>`) rather than a toolchain line.
`zz profile`, `zz build --full -- <args>` and `zz install` nest the same five
steps inside their own numbered pipeline; `zz run --native`, `zz test` and
`zz bench` stay silent.

## `zz run --watch` — hot reload (VM only)

```bash
zz run --watch server.zz
zz run --watch --clear --debounce 200 --watch-ignore '*.gen.zz' src/main.zz
```

Fresh `zz run` child per generation (no leaked sockets/threads).
Watches owning project (`*.zz`, `zz.toml`/`zz.lock`) + `--embed` assets;
skips `vendor/ .git/ bin/ build/ target/`. On change: debounce (default 150ms),
`zz check`, broken edits keep old server running, restarts print `generation N`
(child sees `ZZ_WATCH_GEN`). Rejects `--native`/`--bytecode`. Roots >20k entries
refuse with a hint.

## Maintenance

```bash
zz clean          # bin/, build/, src/bin/
zz clean --deps   # + vendor/ + zz.lock
zz cache clean    # wipe ~/.zz/cache (binaries, runtime archives, objects)
zz cache gc       # GC unused package downloads
zz doctor         # audit binary/clang/shell/git/registry
zz doctor --fix
zz upgrade        # self-update
zz setup          # PATH + completions once
zz toolchain install [--version 0.17.0]
zz toolchain status / use / uninstall
zz dis [file]     # disassemble bytecode/chunk
zz dis --vm [file]  # VM bytecode with source sites (--stats: class mix)
zz profile [file] # PGO profile flow
```

`ZZ_HOME` relocates cache; `ZZ_TOOLCHAIN_ROOT` relocates managed toolchain.
Toolchain sources are official `ziglang.org` index.json, sha256-checked; pin
prefers managed `zig` unless `--cc` forces a provider, and busts the runtime cache.

Details: [cli.md](../cli.md).
