# CLI reference — every command, every flag

Source of truth is `crates/zz_cli/src/main.rs` dispatch + per-command modules.
Exit `0` success, `1` errors / fmt-check failed, `2` test discovery/compile error.

## Run / eval / dis / profile

```bash
zz                              # REPL (:help, :quit, :type expr, :clear)
zz run [<file.zz>]              # VM, no artifacts; defaults to project entry
zz run --bytecode [<file.zz>|file.zzc]  # force the chunk VM (never the tree walker)
zz run --native [<file.zz>]     # AOT cached build + exec private staged copy
zz run --embed <dir> [<file.zz>]        # expose assets (`fs.embedfs()`)
zz run --native -o <name> [<file.zz>]   # name the published binary
zz run --watch [<file.zz>] [--clear] [--debounce <ms>] [--watch-ignore <glob>]...
zz run --features a,b [<file.zz>]       # enable project features (repeatable)
zz run --all-features [<file.zz>]       # enable every declared feature
zz eval "<src>"                 # eval string, print result
zz dis [<file.zzc|file.zz>]     # disassemble bytecode/chunk
zz dis --vm [--stats] [<file>]  # VM chunk ops (or per-class mix) + source sites
zz profile [<file.zz>] [-- args]# PGO profile flow (llvm-profdata merge)
```

`--watch` rejects `--native`/`--bytecode`. Roots >20k entries refuse with a hint.

`zz run` engine = run-cache state: warm → cached IR in the chunk VM, miss →
tree walker (+ store). `ZZ_RUN_CACHE=0` forces the walker, `ZZ_VERBOSE=1`
prints hit/miss and stage lines; entries live in `<build-cache>/run-v2`
(LRU, 256), cleared by `zz cache clean`. `zz run --help` lists the flags.

`ZZ_VM_STATS=1 zz run f.zz` counts dispatched ops and prints a per-class
summary (fused/int-op/stack/arith/control/call/access/alloc) to **stderr**
at exit; program stdout is untouched. `zz dis --vm` shows the same chunks
statically, `zz dis --vm --stats` the class mix and top opcodes.

## Build

```bash
zz build [-p|--release|-O3|--static|--dynamic|--full|--pgo]
  [--target <triple>] [--cc <clang|zig>] [--embed <dir>] [-o <name>] [<file.zz>]
```

- `-p` release `-O3 -flto=thin` cached `~/.zz/cache`; `--full` max LTO (+ PGO training with `-- <args>`).
- `--static` self-contained (not macOS), `--dynamic` fast `-O0 -g`.
- `--target` cross (implies `-p`, safe baseline CPU, `-fuse-ld=lld`, `-lws2_32` on Windows).
- `--cc zig|clang`, `--verbose` prints clang line, `--embed <dir>` bakes assets.
- `--pgo` native-host only; `--allow-source-builds` / `--allow-hooks` gate source builds.

Outputs: standalone `./<stem>`; project `<root>/bin/<name>`; `--target` adds
`-<triple>`; `-o` renames. Atomic publish; sidecars `app.c`/`build.sh`/`build.bat`.

### Build phases

`zz build` reports five generic steps on stderr, identical for every backend:

```
building demopkg (static)
==> [1/5] Checking: 12 modules (10 cached)
==> [2/5] Optimizing: 512 functions
==> [3/5] Compiling: 14 files (3 cached)
==> [4/5] Linking: 14 files
==> [5/5] Publishing: bin/demopkg (1.2 MB)
✓ built bin/demopkg (static, 1.2 MB)
```

- The header names the target: a project build uses the `zz.toml` package
  name (`building demopkg (static)`, plus a dim entry when it is not the
  conventional `src/main.zz`), a loose file its file name
  (`building hello.zz (static)`).
- On a TTY each step owns one line, rewritten in place with a live counter
  (modules checked, files compiled) and closed with `✓` and elapsed time.
  Piped stderr gets exactly one plain line per step — no ANSI, no `\r`.
- Unchanged source short-circuits before type-checking:
  `==> cached · source unchanged`, then only the `Publishing` step. A cached
  build does not need `clang` on `PATH`.
- Colour is on by default on a TTY. `ZZ_COLOR=never` disables it and
  `ZZ_COLOR=always` forces ANSI through a pipe; `NO_COLOR` and `TERM=dumb`
  always win over both. `auto` (the default) means TTY only.
- `ZZ_BUILD_CACHE=0` forces a real build (no cache probe, no reuse), like
  `ZZ_RUN_CACHE=0` / `ZZ_CHECK_CACHE=0`.
- `--verbose` adds timing lines; a cache hit names the reused entry
  (`zz: cache hit <path>`) instead of a toolchain line. `zz profile`,
  `zz build --full -- <args>` and `zz install` nest the same five steps
  under their own numbered pipeline; `zz run --native`, `zz test` and
  `zz bench` stay silent.

## Check / fix / fmt

```bash
zz check [PATH] [--fix|-f] [--hard] [--interactive|-i] [--stats] [--format json]
zz fix [PATH] [--hard]              # = check --fix
zz fmt [PATH] [-c|--check]          # -c dry run
```

`zz check --check` rejected (use `zz fmt --check`).

## Test / bench / cover

```bash
zz test [path] [--exact] [--tag <t>] [--skip <s>] [--jobs N] [--serial]
  [--nocapture] [--seed <n>] [--list] [--repeat N] [--fail-fast] [--fail-on-empty]
  [--native] [--engine vm|native] [-p|--release] [--timeout <ms>] [--soft-timeout]
  [--json] [--junit <path>] [--slow-threshold <ms>] [--changed]
  [--update-snapshots] [--no-doctests]

zz bench [path] [--engine native] [--list]
  [--filter/-f <s>] [--exact] [--tag/-t <t>] [--skip/-s <s>]
  [--iters/-n N] [--warmup/-w N] [--timeout <ms>] [--json]

zz cover [path]                     # VM + AOT coverage, see coverage.md
```

`--nocapture` forces `--serial` with a note. `zz.toml [test]` mirrors flags; CLI wins.

## Packages

```bash
zz init [name] | zz new <name>
zz add <dep> | zz install [--frozen] | zz install -i | zz remove <dep>
zz update | zz outdated | zz deps | zz audit
zz search <q> | zz info <pkg> | zz registry | zz login | zz publish
zz generate-wrapper
zz clean [--deps]                    # bin/build/src/bin (+ vendor + lock)
zz cache clean | zz cache gc
```

Manifest wins on source; stale lock pins ignored with a note; fresh clones fetch+link.
`publish` bundles `DOCS.md` else `docs/**/*.md` (max 200KB).
`--features a,b` / `--all-features` also apply to `build`, `check`, and
`install`; without `[features]` the flags are a no-op (default selection).

## System

```bash
zz setup | zz completion [shell]    # PATH + completions once
zz upgrade [--dry-run] | zz doctor [--fix]
zz toolchain install [--version X] | zz toolchain status|use|uninstall
```

`ZZ_HOME` relocates cache; `ZZ_TOOLCHAIN_ROOT` relocates managed toolchain.
Toolchain from official `ziglang.org` index.json, sha256-checked; pin prefers
managed `zig` unless `--cc` forces provider.

Full narratives: [CLI](cli.md), [Check/fmt](check-fmt.md), [Test/bench](test-bench.md),
[Packages](packages.md).
