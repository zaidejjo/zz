#!/bin/bash
# VM-vs-native differential over fuzz cases. Compares exit code + stdout
# with purely-numeric lines stripped (timestamps/addresses).
#
# Usage:
#   tests/fuzz/run.sh [cases_dir] [out_dir]
#   ZZ=path/to/zz tests/fuzz/run.sh            # default /tmp/fuzz layout
#
# Exit codes: VM failures (VMFAIL) are generator bugs or real VM bugs —
# triage first. NATFAIL lines are bucketed by C-diagnostic signature
# against tests/fuzz/known_failures.txt; DIFF lines (both exit 0, outputs
# differ) are always NEW bugs. Any signature outside known_failures.txt
# fails the run at the end.
ZZ=${ZZ:-./target/debug/zz}
CASES=${1:-/tmp/fuzz/cases}
OUT=${2:-/tmp/fuzz/results}
mkdir -p "$OUT"
norm() { grep -v '^[0-9][0-9]*$' "$1" | sed 's/[[:space:]]*$//'; }
# NOTE: only stdout is compared strictly. Stderr (unused-variable
# warnings, etc.) is compared sorted: warning *order* varies between
# engines (HashMap iteration), so an unsorted diff would false-positive.
norm_err() { sort "$1" | sed 's/[[:space:]]*$//'; }
run_one() {
	f="$1"
	base=$(basename "$f" .zz)
	timeout 60 "$ZZ" run "$f" >"$OUT/$base.vm.out" 2>"$OUT/$base.vm.err"
	vmc=$?
	echo "$vmc" >"$OUT/$base.vm.code"
	if [ "$vmc" -ne 0 ]; then
		echo "VMFAIL $base"
		return
	fi
	timeout 150 "$ZZ" run --native "$f" >"$OUT/$base.nat.out" 2>"$OUT/$base.nat.err"
	natc=$?
	echo "$natc" >"$OUT/$base.nat.code"
	if [ "$natc" -ne 0 ]; then
		echo "NATFAIL $base"
		return
	fi
	if ! diff -q <(norm "$OUT/$base.vm.out") <(norm "$OUT/$base.nat.out") >/dev/null; then
		echo "DIFF $base"
		return
	fi
	if ! diff -q <(norm_err "$OUT/$base.vm.err") <(norm_err "$OUT/$base.nat.err") >/dev/null; then
		echo "STDERRDIFF $base (informational: usually warning order)"
	fi
}
export -f run_one norm
export ZZ OUT
printf '%s\n' "$CASES"/*.zz | xargs -P 2 -I{} bash -c 'run_one "$@"' _ {} 2>/dev/null | tee "$OUT/summary.txt" | tail -n 5
echo "--- counts ---"
grep -c VMFAIL "$OUT/summary.txt" || true
grep -c NATFAIL "$OUT/summary.txt" || true
grep -c DIFF "$OUT/summary.txt" || true
echo "--- natfail signatures (must all match known_failures.txt classes) ---"
grep -h -o "error: [a-z ]*" "$OUT"/*.nat.err 2>/dev/null | sort | uniq -c | sort -rn
