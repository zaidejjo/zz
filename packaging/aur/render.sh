#!/usr/bin/env bash
# Render production PKGBUILDs for both AUR packages (`zz`, `zz-lang`)
# from PKGBUILD.template. Used locally and by .github/workflows/aur-publish.yml.
#
# Usage:
#   ./packaging/aur/render.sh <pkgver> <sha256> [outdir]
#   ./packaging/aur/render.sh 0.1.0 abc123... /tmp/aur-out
#
# Output:
#   <outdir>/zz/PKGBUILD
#   <outdir>/zz-lang/PKGBUILD
#
# pkgver: upstream version WITHOUT leading `v` (e.g. 0.1.0, from tag v0.1.0).
# sha256: sha256 of https://github.com/zaidejjo/zz/archive/refs/tags/v<pkgver>.tar.gz
set -euo pipefail

TEMPLATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEMPLATE="$TEMPLATE_DIR/PKGBUILD.template"

if [[ $# -lt 2 ]]; then
	echo "usage: render.sh <pkgver> <sha256> [outdir]" >&2
	exit 2
fi

PKGVER="$1"
SHA256="$2"
OUTDIR="${3:-$TEMPLATE_DIR/out}"

if [[ ! -f "$TEMPLATE" ]]; then
	echo "render.sh: template not found: $TEMPLATE" >&2
	exit 1
fi

# Arch pkgver forbids `-` and leading `v`; enforce early so CI fails fast
# instead of pushing a PKGBUILD that makepkg rejects.
if [[ "$PKGVER" =~ ^v ]]; then
	echo "render.sh: pkgver must not start with 'v': $PKGVER" >&2
	exit 1
fi
if [[ "$PKGVER" == *"-"* ]]; then
	echo "render.sh: pkgver must not contain '-': $PKGVER" >&2
	exit 1
fi
if [[ ! "$SHA256" =~ ^[0-9a-fA-F]{64}$ ]]; then
	echo "render.sh: sha256 must be 64 hex chars" >&2
	exit 1
fi

render_one() {
	local pkgname="$1"
	local conflicts="$2"
	local dest="$OUTDIR/$pkgname/PKGBUILD"
	mkdir -p "$(dirname "$dest")"
	sed -e "s/@PKGNAME@/$pkgname/g" \
		-e "s/@PKGVER@/$PKGVER/g" \
		-e "s/@SHA256@/$SHA256/g" \
		-e "s/@CONFLICTS@/$conflicts/g" \
		"$TEMPLATE" >"$dest"
	echo "wrote $dest"
}

render_one "zz" "zz-lang"
render_one "zz-lang" "zz"

# Sanity: both files must differ only in pkgname/conflicts.
if ! grep -q "^pkgname=zz$" "$OUTDIR/zz/PKGBUILD"; then
	echo "render.sh: zz PKGBUILD missing pkgname=zz" >&2
	exit 1
fi
if ! grep -q "^pkgname=zz-lang$" "$OUTDIR/zz-lang/PKGBUILD"; then
	echo "render.sh: zz-lang PKGBUILD missing pkgname=zz-lang" >&2
	exit 1
fi
