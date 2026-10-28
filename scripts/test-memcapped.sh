#!/bin/bash
# Run one integration-test target under a hard address-space cap.
#
# Why: OOM/hang regressions (e.g. the `extern "C"` parse loop that grew
# without bound) must fail fast instead of eating all RAM. The cap has to
# be applied to the *test binary*, not to `cargo`:
#
#     (ulimit -v 1500000; cargo test -p zz_ir roundtrip)
#
# dies in rust-lld with `failed to open ...: Cannot allocate memory"` —
# the linker mmaps far more virtual address space than the test ever
# touches. Build with normal limits, then exec the binary under the cap.
#
# Usage: scripts/test-memcapped.sh <package> <test-target> [<test args>...]
#   e.g. scripts/test-memcapped.sh zz_frontend parser_tests
#        scripts/test-memcapped.sh zz_ir roundtrip --test-threads=2
#
# Env: ZZ_MEMCAP_KB — cap in KiB (default 1500000 ≈ 1.5 GiB).
set -euo pipefail

cd "$(dirname "$0")/.."

pkg="${1:?usage: test-memcapped.sh <package> <test-target> [args...]}"
target="${2:?usage: test-memcapped.sh <package> <test-target> [args...]}"
shift 2

cap="${ZZ_MEMCAP_KB:-1500000}"

# Build (and link) with the normal, uncapped limits.
cargo test -p "$pkg" --test "$target" --no-run >/dev/null

# Newest linked binary for this target: stale hashes linger after test
# edits, so pick by mtime rather than glob order. `sed` (not `head`)
# drains the pipe: under `pipefail`, `head` closing early can turn a
# SIGPIPE into a spurious script failure.
bin=$(find target/debug/deps -maxdepth 1 -type f -executable -name "${target}-*" \
	-printf '%T@ %p\n' | sort -nr | sed -n '1s/^[0-9.]* //p')
if [ -z "$bin" ]; then
	echo "test-memcapped: no linked binary for target \`$target\`" >&2
	exit 1
fi

echo "== $target under ${cap} KiB address-space cap ($bin) =="
(
	ulimit -v "$cap"
	exec "$bin" "$@"
)
