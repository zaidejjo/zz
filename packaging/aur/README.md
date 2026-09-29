# AUR packaging for `zz-lang` (`zz` follows after adoption)

The AUR package installs:
- `/usr/bin/zz` — compiler / REPL / runner (`zz run main.zz`, `zz check`, `zz build`)
- `/usr/bin/zz-lsp` — language server

It `conflicts=('zz')` so only one of the two names is installed at a time.
(`zz` pushes are paused until the AUR package is adopted — orphan request
accepted, adoption click still pending. Template already renders both.)

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

`.github/workflows/release.yml` runs on push to `main` when `Cargo.toml`
changes: it reads `[workspace.package] version`, and if tag `vX.Y.Z` does
not exist yet, creates the tag + GitHub Release with auto-generated notes.
So cutting a release is just: bump the version, merge to `main`.

`.github/workflows/aur-publish.yml` then runs on **Release published** (pushes
`zz-lang` only for now):

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
