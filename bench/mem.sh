#!/bin/bash
# zz memory-management Phase 1 before/after benchmark.
# Usage: bash bench_mem.sh [new_bin] [base_bin]
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NEW_BIN="${1:-$ROOT/target/release/zz}"
BASE_BIN="${2:-/tmp/opencode/zz_base}"

for b in "$NEW_BIN" "$BASE_BIN"; do
	[ -x "$b" ] || {
		echo "missing binary: $b"
		exit 1
	}
done

echo "============================================================"
echo "ZZ Memory Management — Phase 1 before/after"
echo " base : $BASE_BIN"
echo " new  : $NEW_BIN"
echo " load : $(cut -d' ' -f1-3 /proc/loadavg) on $(nproc) cores"
echo "============================================================"

run_rss() { # run_rss <bin> <cmd...> -> "ms rssKB"
	local t0=$(date +%s%N)
	"$@" >/dev/null 2>&1
	local t1=$(date +%s%N)
	local ms=$(((t1 - t0) / 1000000))
	# peak RSS via /usr/bin/time when available
	local rss=""
	if command -v /usr/bin/time >/dev/null 2>&1; then
		rss=$(/usr/bin/time -v "$@" 2>&1 >/dev/null | grep -i "maximum resident" | awk '{print $4}')
	fi
	echo "${ms}ms ${rss:-n/a}KB"
}

echo
echo "---- zz check (compiler-side memory) ----"
for f in bench/compiler/small.zz bench/compiler/large.zz bench/compiler/many_exprs.zz bench/compiler/single_4k.zz bench/compiler/heavyproj/main.zz; do
	echo "### $f"
	echo -n "  base cold:  "
	run_rss "$BASE_BIN" check "$f"
	echo -n "  new  cold:  "
	run_rss "$NEW_BIN" check "$f"
	# warm run (cache populated)
	"$BASE_BIN" check "$f" >/dev/null 2>&1 || true
	"$NEW_BIN" check "$f" >/dev/null 2>&1 || true
	echo -n "  base warm:  "
	run_rss "$BASE_BIN" check "$f"
	echo -n "  new  warm:  "
	run_rss "$NEW_BIN" check "$f"
done

echo
echo "---- zz check repeated x5 (RSS drift detection) ----"
F=bench/compiler/large.zz
echo -n "  base : "
for i in 1 2 3 4 5; do "$BASE_BIN" check "$F" >/dev/null 2>&1; done
run_rss "$BASE_BIN" check "$F"
echo -n "  new  : "
for i in 1 2 3 4 5; do "$NEW_BIN" check "$F" >/dev/null 2>&1; done
run_rss "$NEW_BIN" check "$F"

echo
echo "---- zz run VM (runtime memory) ----"
for f in bench/vm_gate/alloc_churn.zz bench/vm_gate/loop_sum.zz bench/vm_gate/array_sum.zz bench/vm_gate/dict_lookup.zz; do
	echo "### $f"
	echo -n "  base: "
	run_rss "$BASE_BIN" run "$f"
	echo -n "  new : "
	run_rss "$NEW_BIN" run "$f"
done

echo
echo "---- zz run --native AOT (runtime memory) ----"
for f in bench/vm_gate/alloc_churn.zz bench/vm_gate/loop_sum.zz bench/vm_gate/array_sum.zz bench/vm_gate/dict_lookup.zz; do
	echo "### $f"
	"$BASE_BIN" build -p -o /tmp/opencode/bench/base_$(basename $f .zz) "$f" >/dev/null 2>&1 || true
	"$NEW_BIN" build -p -o /tmp/opencode/bench/new_$(basename $f .zz) "$f" >/dev/null 2>&1 || true
	echo -n "  base: "
	run_rss /tmp/opencode/bench/base_$(basename $f .zz)
	echo -n "  new : "
	run_rss /tmp/opencode/bench/new_$(basename $f .zz)
done

echo
echo "DONE."
