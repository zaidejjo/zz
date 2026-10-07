#!/bin/bash
# P0 compiler bench: time + peak RSS for check across corpus sizes.
# Usage: bash scripts/bench-compiler.sh [runs]
set -euo pipefail
cd "$(dirname "$0")/.."
RUNS="${1:-3}"
ZZ_BIN="${ZZ_BIN:-./target/release/zz}"

if [ ! -x "$ZZ_BIN" ]; then
	echo "building release zz…"
	cargo build --release -p zz_cli
fi

for f in bench/compiler/small.zz bench/compiler/medium.zz bench/compiler/large.zz bench/compiler/many_funcs.zz bench/compiler/proj/main.zz; do
	echo "== $f =="
	best=""
	best_rss=""
	for ((i = 1; i <= RUNS; i++)); do
		start=$(date +%s%N)
		rss=$("$ZZ_BIN" check "$f" 2>&1 >/dev/null | grep -i -o 'rss[^,]*' || true)
		end=$(date +%s%N)
		ms=$(((end - start) / 1000000))
		echo "  run $i: ${ms}ms ${rss}"
		if [ -z "$best" ] || [ "$ms" -lt "$best" ]; then best="$ms"; fi
	done
	# peak RSS via GNU time when available
	if command -v /usr/bin/time >/dev/null 2>&1; then
		peak=$(/usr/bin/time -v "$ZZ_BIN" check "$f" 2>&1 >/dev/null | grep -i "maximum resident" || true)
		echo "  best: ${best}ms  ${peak}"
	else
		echo "  best: ${best}ms"
	fi
done
echo "Set ZZ_ARENA_STATS=1 to dump arena counters once P2+ wires phases."
