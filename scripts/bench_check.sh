#!/usr/bin/env bash
# bench_check.sh — Benchmark `zz check` on synthetic programs of increasing size.
#
# Generates ZZ programs of 1k, 2.5k, 5k, 10k, 20k lines containing:
#   many functions, many structs, deep generics, long match chains,
#   big arrays, many imports.
# Runs `zz check` (cold, VM engine) on each and records wall time + peak RSS.
#
# Output: a Markdown table appended to docs/checker/bench_results.md.
#
# Usage: ./scripts/bench_check.sh   (run from repo root; builds `zz` in dev profile)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

ZZ_BIN="${ZZ_BIN:-./target/debug/zz}"
RESULTS_FILE="${RESULTS_FILE:-docs/checker/bench_results.md}"
WORK_DIR="${WORK_DIR:-$(mktemp -d -t zzbench.XXXXXX)}"
trap 'rm -rf "$WORK_DIR"' EXIT

mkdir -p docs/checker

# ---------------------------------------------------------------------------
# Build the binary if needed (dev profile; cold check reflects dev builds).
# ---------------------------------------------------------------------------
if [[ ! -x "$ZZ_BIN" ]]; then
	echo "Building zz (dev profile)…"
	cargo build --bin zz 2>&1 | tail -5
fi

# ---------------------------------------------------------------------------
# Synthetic program generator.
# $1 = target line count (approx), $2 = output file.
# ---------------------------------------------------------------------------
gen_program() {
	local lines="$1" out="$2"
	local per_func=18 # lines per function (signature + body)
	local nfuncs=$((lines / per_func))
	[[ $nfuncs -lt 1 ]] && nfuncs=1

	{
		# Imports (many)
		for i in $(seq 1 12); do
			echo "import std.str as s$i"
		done
		echo

		# Big arrays + structs
		echo "struct BigRecord {"
		for j in $(seq 1 20); do
			echo "  f$j: int,"
		done
		echo "}"
		echo "struct Wrapper { inner: BigRecord }"
		echo

		# Deep generics
		echo "struct Box4<T1, T2, T3, T4> { a: T1, b: T2, c: T3, d: T4 }"
		echo

		# Many functions: long match chains + big arrays + generic struct use
		# Shorter bodies (~8 lines) → more functions per line, stressing the
		# checker's per-function overhead (target: ~3k funcs at 20k lines).
		local k=1
		while [[ $k -le $nfuncs ]]; do
			echo "func gen_$k(x: int, n: int) -> int {"
			echo "  arr := [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]"
			echo "  big := Box4{ a: x, b: x, c: x, d: x }"
			echo "  total := 0"
			echo "  i := 0"
			echo "  while i < n {"
			echo "    total = total + arr[i % 10]"
			echo "    i = i + 1"
			echo "  }"
			echo "  m := match x {"
			local arm=1
			while [[ $arm -le 6 ]]; do
				echo "    $arm => $arm * n,"
				arm=$((arm + 1))
			done
			echo "    _ => 0"
			echo "  }"
			echo "  _ = big.a + big.b + big.c + big.d"
			echo "  return total + m"
			echo "}"
			echo
			k=$((k + 1))
		done
		echo 'println("bench_gen_ok")'
	} >"$out"
}

# ---------------------------------------------------------------------------
# Measure wall time + peak RSS for `zz check <file>`.
# Returns: "time_s rss_kb" or "ERROR".
# ---------------------------------------------------------------------------
measure() {
	local file="$1"
	# /usr/bin/time -v gives peak RSS (Maximum resident set size) in KB.
	if ! command -v /usr/bin/time >/dev/null 2>&1; then
		echo "ERROR: /usr/bin/time (GNU time) not found" >&2
		return 1
	fi
	local out
	out=$(/usr/bin/time -v "$ZZ_BIN" check "$file" 2>&1 >/dev/null) || true
	local time_s rss_kb
	time_s=$(echo "$out" | awk '/Elapsed \(wall clock\) time/{print $NF}' | head -1)
	rss_kb=$(echo "$out" | awk '/Maximum resident set size/{print $NF}' | head -1)
	# time_s may be like "0:01.23" → convert to seconds
	if [[ -n "$time_s" ]]; then
		time_s=$(awk -v t="$time_s" 'BEGIN{
      split(t, a, ":"); if (length(a)==3) print a[1]*3600+a[2]*60+a[3]; else print a[1]+a[2]/100.0
    }')
	fi
	echo "${time_s:-?} ${rss_kb:-?}"
}

# ---------------------------------------------------------------------------
# Write results table header (fresh file).
# ---------------------------------------------------------------------------
{
	echo "# $(zz check) benchmark results"
	echo ""
	echo "Binary: \`$ZZ_BIN\` (dev profile). Cold check, VM engine."
	echo "Generated programs: many funcs/structs/deep generics/long match chains/big arrays/imports."
	echo ""
	echo "| Lines | Approx funcs | $(zz check) time (s) | Peak RSS (KB) |"
	echo "|-------|--------------|---------------------|---------------|"
} >"$RESULTS_FILE"

for lines in 1000 2500 5000 10000 20000; do
	file="$WORK_DIR/bench_${lines}.zz"
	gen_program "$lines" "$file"
	actual_lines=$(wc -l <"$file")
	read -r time_s rss_kb < <(measure "$file")
	nfuncs=$((actual_lines / 18))
	echo "| $actual_lines | ~$nfuncs | $time_s | $rss_kb |" >>"$RESULTS_FILE"
	echo "bench $lines lines → ${time_s}s, ${rss_kb}KB RSS"
done

echo ""
echo "Results written to $RESULTS_FILE"
cat "$RESULTS_FILE"
