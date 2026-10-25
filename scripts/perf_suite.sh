#!/bin/bash
# =====================================================================
# ZZ Phase 1 perf suite — regression gate for #589 (fs O(n²)) + #590 (JSON).
#
# Runs best-of-N timings with an isolated ZZ_HOME so cold/warm check caches
# and run caches don't leak between base/candidate comparisons.
#
# Usage:
#   scripts/perf_suite.sh [runs]
# Env:
#   ZZ_BIN        path to zz binary (default ./target/release/zz)
#   ZZ_HOME       custom ZZ_HOME (default fresh tmpdir)
#   RUNS          best-of-N runs (default 5)
# =====================================================================
set -euo pipefail
cd "$(dirname "$0")/.."
RUNS="${1:-5}"
ZZ_BIN="${ZZ_BIN:-./target/release/zz}"
ZZ_HOME="${ZZ_HOME:-$(mktemp -d -t zz-perf-home.XXXXXX)}"
export ZZ_HOME

if [ ! -x "$ZZ_BIN" ]; then
	echo "building release zz…"
	cargo build --release -p zz_cli >/dev/null 2>&1
fi

# Data fixtures (external corpus is fine; gitignored in user targets).
DATA_DIR="${ZZ_PERF_DATA:-/home/zaid/Projects/.zz-targets}"
READ_DIR="$DATA_DIR/bench-read-dir-20k"
BIG_JSON="$DATA_DIR/bench-big.json"

time_ms() { # time_ms [ENV=VAL ...] <cmd...> -> best ms
	local env_prefix=()
	while [ $# -gt 0 ] && [[ "$1" == *=* ]]; do
		env_prefix+=("$1")
		shift
	done
	local best=999999999
	for _ in $(seq "$RUNS"); do
		local t0=$(date +%s%N)
		if [ ${#env_prefix[@]} -gt 0 ]; then
			env "${env_prefix[@]}" "$@" >/dev/null 2>&1
		else
			"$@" >/dev/null 2>&1
		fi
		local t1=$(date +%s%N)
		local dt=$(((t1 - t0) / 1000000))
		[ "$dt" -lt "$best" ] && best=$dt
	done
	echo "$best"
}

echo "Phase 1 perf gate — $ZZ_BIN · RUNS=$RUNS · ZZ_HOME=$ZZ_HOME"

# ---- cold vs warm zz check ----------------------------------------------
for f in bench/compiler/small.zz bench/compiler/large.zz \
	bench/compiler/single_4k.zz bench/compiler/many_exprs.zz \
	bench/compiler/heavyproj/main.zz; do
	# cold: cache disabled
	cold=$(time_ms ZZ_CHECK_CACHE=0 "$ZZ_BIN" check "$f")
	# warm: cache enabled (first run populates)
	"$ZZ_BIN" check "$f" >/dev/null 2>&1 || true
	warm=$(time_ms "$ZZ_BIN" check "$f")
	echo "check  cold=${cold}ms warm=${warm}ms  $f"
done

# ---- VM run: fs / json / dict fixtures -----------------------------------
echo "vm   read_dir=$(time_ms "$ZZ_BIN" run bench/phase1/fs_read_dir.zz "$READ_DIR")"
echo "vm   walk_dir=$(time_ms "$ZZ_BIN" run bench/phase1/fs_walk_dir.zz "$READ_DIR")"
echo "vm   json_get=$(time_ms "$ZZ_BIN" run bench/phase1/json_get.zz "$READ_DIR" "$BIG_JSON")"
echo "vm   dict_get=$(time_ms "$ZZ_BIN" run bench/phase1/dict_get.zz)"

# ---- AOT build + run -------------------------------------------------------
for b in fs_read_dir fs_walk_dir json_get dict_get; do
	"$ZZ_BIN" build -p -o "$PWD/.bin/phase1_${b}_aot" bench/phase1/${b}.zz >/dev/null 2>&1
done
echo "aot  read_dir=$(time_ms "$PWD/.bin/phase1_fs_read_dir_aot" "$READ_DIR")"
echo "aot  walk_dir=$(time_ms "$PWD/.bin/phase1_fs_walk_dir_aot" "$READ_DIR")"
echo "aot  json_get=$(time_ms "$PWD/.bin/phase1_json_get_aot" "$READ_DIR" "$BIG_JSON")"
echo "aot  dict_get=$(time_ms "$PWD/.bin/phase1_dict_get_aot")"

# ---- startup ---------------------------------------------------------------
echo "startup version=$(time_ms "$ZZ_BIN" --version)"
printf 'func main() -> int { println("hi") return 0 }\n' >"$PWD/.bin/hello.zz"
echo "startup run=$(time_ms "$ZZ_BIN" run "$PWD/.bin/hello.zz")"

echo "done"
