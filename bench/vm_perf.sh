#!/bin/bash
# VM perf gate: runs bench/vm_gate fixtures under two zz binaries and
# compares best-of-N elapsed ms (inside-VM timers, startup excluded).
#
# Usage: bench/vm_perf.sh [runs] [new_bin] [base_bin]
#   runs     best-of-N repetitions per fixture (default 3)
#   new_bin  candidate binary (default ZZ_NEW_BIN or ~/.bench/zz_new)
#   base_bin baseline binary (default ZZ_BASE_BIN or ~/.bench/zz_base)
#
# Each fixture prints "<name>: <ms>ms result=<value>". The result= payload
# must match between binaries or the fixture is reported FAIL (behavior
# gate: speedups must never change semantics).
# Prints a markdown table with per-fixture delta percent; exit nonzero on
# any result mismatch or fixture failure.

set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GATE="$ROOT/bench/vm_gate"
RUNS="${1:-3}"
NEW_BIN="${2:-${ZZ_NEW_BIN:-$HOME/.bench/zz_new}}"
BASE_BIN="${3:-${ZZ_BASE_BIN:-$HOME/.bench/zz_base}}"

for b in "$NEW_BIN" "$BASE_BIN"; do
	[ -x "$b" ] || {
		echo "missing binary: $b"
		echo "usage: bench/vm_perf.sh [runs] [new_bin] [base_bin]"
		exit 1
	}
done

FIXTURES="loop_sum fib22 str_index str_slice_len dict_lookup method_push"

# run_one <bin> <fixture> -> prints "<ms> <result>" (last line parsed).
run_one() {
	$1 run "$GATE/$2.zz" 2>/dev/null | tail -n 1 | sed -e 's/^.*: //' -e 's/ms result=/ /'
}

# best_of <bin> <fixture> -> prints "<best_ms> <result>".
best_of() {
	best_ms=""
	best_result=""
	n=0
	while [ "$n" -lt "$RUNS" ]; do
		out="$(run_one "$1" "$2")"
		ms="$(echo "$out" | awk '{print $1}')"
		res="$(echo "$out" | awk '{print $2}')"
		case "$ms" in
		'' | *[^0-9]*)
			echo "fixture $2 failed under $1 (output: $out)"
			exit 1
			;;
		esac
		if [ -z "$best_ms" ] || [ "$ms" -lt "$best_ms" ]; then
			best_ms="$ms"
			best_result="$res"
		fi
		n=$((n + 1))
	done
	echo "$best_ms $best_result"
}

echo "| fixture | base best | new best | delta | result |"
echo "|---|---|---|---|---|"

fail=0
for f in $FIXTURES; do
	base="$(best_of "$BASE_BIN" "$f")"
	new="$(best_of "$NEW_BIN" "$f")"
	base_ms="$(echo "$base" | awk '{print $1}')"
	base_res="$(echo "$base" | awk '{print $2}')"
	new_ms="$(echo "$new" | awk '{print $1}')"
	new_res="$(echo "$new" | awk '{print $2}')"
	if [ "$base_res" = "$new_res" ]; then
		status="same"
	else
		status="MISMATCH"
		fail=1
	fi
	delta="$(awk -v b="$base_ms" -v n="$new_ms" 'BEGIN { if (b > 0) printf "%+.1f%%", (b - n) * 100.0 / b; else print "n/a" }')"
	echo "| $f | ${base_ms}ms | ${new_ms}ms | $delta | $status |"
done

exit "$fail"
