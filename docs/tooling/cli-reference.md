# CLI reference — every command, every flag

Source of truth is `crates/zz_cli/src/main.rs` dispatch + per-command modules.
Exit `0` success, `1` errors / fmt-check failed, `2` test discovery/compile error.

## Run / eval / dis / profile

```bash
zz                              # REPL (:help, :quit, :type expr, :clear)
zz run [<file.zz>]              # VM, no artifacts; defaults to project entry
zz run --native [<file.zz>]     # AOT cached build + exec private staged copy
zz run --watch [<file.zz>] [--clear] [--debounce <ms>] [--watch-ignore <glob>]...
zz eval "<src>"                 # eval string, print result
zz dis [<file.zzc|file.zz>]     # disassemble bytecode/chunk
zz profile [<file.zz>] [-- args]# PGO profile flow (llvm-profdata merge)
```

`--watch` rejects `--native`/`--bytecode`. Roots >20k entries refuse with a hint.

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
