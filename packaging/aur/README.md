# AUR packaging for `zz` and `zz-lang`

Both AUR packages install the **same** binaries so `zz run main.zz` works
whichever name the user installed:

- `/usr/bin/zz` — compiler / REPL / runner (`zz run`, `zz check`, `zz build`)
- `/usr/bin/zz-lsp` — language server

They `conflicts=()` each other; pacman allows only one at a time.

## Files

- `PKGBUILD.template` — single source of truth. `@PKGNAME@`, `@PKGVER@`,
  `@SHA256@`, `@CONFLICTS@` are substituted by `render.sh` / CI.
- `render.sh` — renders `zz/PKGBUILD` + `zz-lang/PKGBUILD` for a version.
- `out/` (gitignored) — local render output, never committed.

## Local test

```bash
# 1. Render (fake sha for syntax check)
./packaging/aur/render.sh 0.1.0 \
  0000000000000000000000000000000000000000000000000000000000000000 \
  ./packaging/aur/out

# 2. Real sha for the release tag
curl -fsSL https://github.com/zaidejjo/zz/archive/refs/tags/v0.1.0.tar.gz \
  | sha256sum

# 3. Full Arch check (needs Arch or docker):
docker run --rm -v "$PWD:/src:ro" -w /src archlinux:base-devel bash -c "
  useradd -m builder && cp -r /src/packaging/aur/out/zz /home/builder/pkg &&
  chown -R builder:builder /home/builder/pkg &&
  su builder -c 'cd /home/builder/pkg && makepkg --printsrcinfo > .SRCINFO && namcap PKGBUILD && makepkg -sri --noconfirm'
"
```

## Release flow (CI)

`.github/workflows/aur-publish.yml` runs on **Release published**:

1. Tag `vX.Y.Z` must equal `[workspace.package] version` in `Cargo.toml`.
2. CI downloads the tag tarball, computes sha256, renders both PKGBUILDs,
   generates `.SRCINFO` with `makepkg --printsrcinfo`, lints with `namcap`.
3. CI clones `ssh://aur@aur.archlinux.org/zz.git` and `zz-lang.git`,
   commits `PKGBUILD` + `.SRCINFO` as `upgpkg: <name> <ver>-<rel>`, pushes.
4. Re-running the same Release is a no-op when AUR is already current.

Manual re-publish (pkgrel bump, no new tag): Actions → aur-publish →
Run workflow → `tag: v0.1.0`, `pkgrel: 2`.

## Secret

- `AUR_SSH_PRIVATE_KEY` — SSH private key of the AUR account that owns
  both `zz` and `zz-lang`. Public key must be registered at
  https://aur.archlinux.org/account. SSH user is always `aur`.
