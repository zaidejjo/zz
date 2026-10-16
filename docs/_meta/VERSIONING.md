# Versioning — docs per release (spec for the website repo)

`docs/` in this repo tracks `dev` (unreleased). The website repo
(`zz-lang.pages.dev`) owns versioned rendering. This spec keeps the two in sync.

## Source of truth

- Version lives in workspace `Cargo.toml` (`[workspace.package] version`, now `0.3.0`).
- Releases cut from `main` via `.github/workflows/release.yml` (per-arch zips
  with `lib/` native runtime). Downstream pins follow the tag.
- `zz --version` prints the same version the docs describe.

## Website contract

1. On every tag (`vX.Y.Z`), the website snapshots `docs/` at that tag under
   `/vX.Y.Z/...` and keeps `/...` (latest) pointing at `dev`.
2. `docs/_meta/search-index.json` (`version: 2` now) gains a `tag` field per
   snapshot so symbol search can scope to a version.
3. `docs/llms.txt` snapshots alongside — AI consumers pin `llms.txt` at a tag
   for stable answers.
4. `docs/stdlib/reference.md` header records the generator run
   (`691 entries` now); drift between tags is reviewable in git.

## Changelog link

No `CHANGELOG.md` in this repo yet. Until one lands, history is
`git log --oneline dev` + GitHub releases per tag. Docs must never claim
behavior newer than the workspace version without a `since X.Y.Z` note.

## Offline equivalent

No network? `docs/` + `rg` is the versioned bundle:
`rg "std.http.route" docs/stdlib/` reads the same files the site renders.
See [zz doc spec](../tooling/doc-spec.md) for the planned CLI wrapper.
