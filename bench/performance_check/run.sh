#!/usr/bin/env bash
# =====================================================================
#  Z Z   S T R E S S   S U I T E   —   runner
#
#  Compares ZZ (native AOT, `zz build -p`) vs Go vs Rust on three
#  workloads:
#    * bench_memory_leak    — 50M short-lived allocs across 2 passes
#    * bench_cpu_intensive  — 10M accum + 1M pow/mod + 100k array ops
#    * bench_string_concats — 50×5k + 20×2k + 10k string concats
#
#  Metrics:
#    * elapsed_ms   wall-clock time (best-of-N runs)
#    * peak_rss_kib peak resident set size via /usr/bin/time -v
#                   (ps polling fallback)
#    * binary_kib   compiled artifact size in KiB
#
#  Output:
#    * bench/performance_check/RESULTS.md    (human-readable table)
#    * bench/performance_check/results.json (machine-readable)
#
#  Usage:
#    bash bench/performance_check/run.sh
#    RUNS=5 bash bench/performance_check/run.sh
# =====================================================================
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
RUNS="${RUNS:-3}"

LOG_DIR="$HERE/.log"
RESULTS_JSON="$HERE/results.json"
RESULTS_MD="$HERE/RESULTS.md"
ZZ_BIN_DIR="$HERE/.zzbin"
mkdir -p "$LOG_DIR" "$HERE/.bin" "$ZZ_BIN_DIR"

# ---- colors (only if stdout is a tty) -------------------------------
if [ -t 1 ]; then
	RST=$'\e[0m'
	BLD=$'\e[1m'
	DIM=$'\e[2m'
	RED=$'\e[31m'
	GRN=$'\e[32m'
	YLW=$'\e[33m'
	BLU=$'\e[34m'
	CYN=$'\e[36m'
else
	RST=""
	BLD=""
	DIM=""
	RED=""
	GRN=""
	YLW=""
	BLU=""
	CYN=""
fi

# ---- locate zz binary ----------------------------------------------
ZZ="$ROOT/target/release/zz"
[ -x "$ZZ" ] || ZZ="$ROOT/target/debug/zz"
if [ ! -x "$ZZ" ]; then
	echo "${RED}error${RST}: zz binary not found — run ${BLD}cargo build --release${RST} first." >&2
	exit 1
fi

# ---- check toolchain availability ---------------------------------
HAVE_GO=0
command -v go >/dev/null 2>&1 && HAVE_GO=1
HAVE_RUST=0
command -v rustc >/dev/null 2>&1 && HAVE_RUST=1
[ "$HAVE_GO" -eq 1 ] || echo "${YLW}warn${RST}: go not on PATH — Go column skipped"
[ "$HAVE_RUST" -eq 1 ] || echo "${YLW}warn${RST}: rustc not on PATH — Rust column skipped"

# ---- helpers --------------------------------------------------------
TIME_BIN="/usr/bin/time"
[ -x "$TIME_BIN" ] || TIME_BIN=""

best_ms() { # best_ms <cmd...>  →  best-of-N ms
	local best=999999999
	for _ in $(seq 1 "$RUNS"); do
		local t0 t1 dt
		t0=$(date +%s%N)
		"$@" >/dev/null 2>&1
		t1=$(date +%s%N)
		dt=$(((t1 - t0) / 1000000))
		[ "$dt" -lt "$best" ] && best=$dt
	done
	echo "$best"
}

# measure_rss_kib <label> <cmd...>  →  peak RSS in KiB
measure_rss_kib() {
	local label="$1"
	shift
	local logfile="$LOG_DIR/${label}_rss.log"
	if [ -n "$TIME_BIN" ]; then
		"$TIME_BIN" -v -o "$logfile" "$@" >/dev/null 2>&1 || true
		local kib
		kib=$(awk '/Maximum resident set size/ {print $NF}' "$logfile" 2>/dev/null || echo 0)
		if [ -n "$kib" ] && [ "$kib" -gt 0 ] 2>/dev/null; then
			echo "$kib"
			return
		fi
	fi
	"$@" >/dev/null 2>&1 &
	local pid=$!
	local peak=0
	while kill -0 "$pid" 2>/dev/null; do
		local rss
		rss=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ' || echo 0)
		[ -n "$rss" ] && [ "$rss" -gt "$peak" ] 2>/dev/null && peak="$rss"
		sleep 0.05
	done
	wait "$pid" 2>/dev/null || true
	echo "$peak"
}

binary_size_kib() {
	local f="$1"
	[ -f "$f" ] || {
		echo 0
		return
	}
	local b
	b=$(stat -c %s "$f" 2>/dev/null || stat -f %z "$f" 2>/dev/null || echo 0)
	awk -v b="$b" 'BEGIN { printf "%d", (b + 1023) / 1024 }'
}

# ---------------------------------------------------------------------
#  Build all artifacts
# ---------------------------------------------------------------------
echo "${BLD}${CYN}━━ building all artifacts ━━${RST}"

GO_BIN="$HERE/.bin/bench_go"
if [ "$HAVE_GO" -eq 1 ]; then
	(cd "$HERE/go" && go build -o "$GO_BIN" .) >"$LOG_DIR/go_build.log" 2>&1
	if [ ! -x "$GO_BIN" ]; then
		echo "${RED}go build failed${RST} — see $LOG_DIR/go_build.log"
		HAVE_GO=0
	fi
fi

RUST_BIN="$HERE/.bin/bench_rust"
if [ "$HAVE_RUST" -eq 1 ]; then
	(cd "$HERE/rust" && cargo build --release --quiet) >"$LOG_DIR/rust_build.log" 2>&1
	if [ -x "$HERE/rust/target/release/bench_stress" ]; then
		cp -f "$HERE/rust/target/release/bench_stress" "$RUST_BIN"
	else
		echo "${RED}rust build failed${RST} — see $LOG_DIR/rust_build.log"
		HAVE_RUST=0
	fi
fi

# ZZ — AOT build every workload into a separate binary.
declare -A ZZ_BIN
for bench in memory_leak cpu_intensive string_concats; do
	src="$HERE/zz/bench_${bench}.zz"
	out="$ZZ_BIN_DIR/bench_${bench}"
	rm -f "$out"
	if "$ZZ" build -p "$src" >"$LOG_DIR/zz_build_${bench}.log" 2>&1; then
		# `zz build -p` writes to the source dir as `<basename>`.
		srcbin="$HERE/zz/bench_${bench}"
		if [ -x "$srcbin" ]; then
			mv -f "$srcbin" "$out"
			chmod +x "$out"
			ZZ_BIN[$bench]="$out"
		else
			echo "${YLW}warn${RST}: zz build -p succeeded but no binary at $srcbin"
			ZZ_BIN[$bench]=""
		fi
	else
		echo "${YLW}warn${RST}: zz build -p failed for $bench — see $LOG_DIR/zz_build_${bench}.log"
		ZZ_BIN[$bench]=""
	fi
done

# ---------------------------------------------------------------------
#  Run all benchmarks × engines
# ---------------------------------------------------------------------
BENCHES=(memory_leak cpu_intensive string_concats)

# Engines we actually have
ENGINES=("zz")
[ "$HAVE_GO" -eq 1 ] && ENGINES+=("go")
[ "$HAVE_RUST" -eq 1 ] && ENGINES+=("rust")

# JSON accumulator
echo "{" >"$RESULTS_JSON"
echo "  \"runs\": $RUNS," >>"$RESULTS_JSON"
echo "  \"machine\": \"$(uname -m)\"," >>"$RESULTS_JSON"
echo "  \"date\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"," >>"$RESULTS_JSON"
echo "  \"engines\": [$(printf '"%s",' "${ENGINES[@]}" | sed 's/,$//')]," >>"$RESULTS_JSON"
echo "  \"results\": {" >>"$RESULTS_JSON"

first_bench=1
for bench in "${BENCHES[@]}"; do
	[ "$first_bench" -eq 0 ] && echo "," >>"$RESULTS_JSON"
	first_bench=0
	echo "${DIM}━━ $bench ━━${RST}"

	declare -A ems erss

	# ZZ (AOT)
	zbin="${ZZ_BIN[$bench]:-}"
	if [ -n "$zbin" ] && [ -x "$zbin" ]; then
		ems[zz]=$(best_ms "$zbin")
		erss[zz]=$(measure_rss_kib "zz_${bench}" "$zbin")
	else
		ems[zz]=0
		erss[zz]=0
	fi

	# Go
	if [ "$HAVE_GO" -eq 1 ]; then
		ems[go]=$(best_ms "$GO_BIN" "$bench")
		erss[go]=$(measure_rss_kib "go_${bench}" "$GO_BIN" "$bench")
	else
		ems[go]=0
		erss[go]=0
	fi

	# Rust
	if [ "$HAVE_RUST" -eq 1 ]; then
		ems[rust]=$(best_ms "$RUST_BIN" "$bench")
		erss[rust]=$(measure_rss_kib "rust_${bench}" "$RUST_BIN" "$bench")
	else
		ems[rust]=0
		erss[rust]=0
	fi

	echo "  ${BLD}zz${RST}  : ${ems[zz]}ms  rss=${erss[zz]}KiB"
	[ "$HAVE_GO" -eq 1 ] && echo "  ${BLD}go${RST}  : ${ems[go]}ms  rss=${erss[go]}KiB"
	[ "$HAVE_RUST" -eq 1 ] && echo "  ${BLD}rust${RST}: ${ems[rust]}ms  rss=${erss[rust]}KiB"

	cat >>"$RESULTS_JSON" <<EOF
    "$bench": {
      "zz":   { "elapsed_ms": ${ems[zz]},   "peak_rss_kib": ${erss[zz]}   },
      "go":   { "elapsed_ms": ${ems[go]},   "peak_rss_kib": ${erss[go]}   },
      "rust": { "elapsed_ms": ${ems[rust]}, "peak_rss_kib": ${erss[rust]} }
    }
EOF
	unset ems erss
done

zz_bin_kib=$(binary_size_kib "${ZZ_BIN[memory_leak]:-/dev/null}")
go_bin_kib=$(binary_size_kib "$GO_BIN")
rust_bin_kib=$(binary_size_kib "$RUST_BIN")

cat >>"$RESULTS_JSON" <<EOF

  },
  "binary_kib": {
    "zz":   $zz_bin_kib,
    "go":   $go_bin_kib,
    "rust": $rust_bin_kib
  }
}
EOF

# ---------------------------------------------------------------------
#  Markdown table
# ---------------------------------------------------------------------
echo
echo "${BLD}${CYN}━━ results ━━${RST}"

val() { # val <bench> <engine> <field>
	python3 - "$RESULTS_JSON" "$1" "$2" "$3" <<'PY' 2>/dev/null || echo 0
import json, sys
path, bench, engine, field = sys.argv[1:5]
try:
    with open(path) as f:
        data = json.load(f)
    print(data["results"][bench][engine][field])
except Exception:
    print(0)
PY
}

cat >"$RESULTS_MD" <<EOF
# Performance / Stress / Memory Benchmark — ZZ vs Go vs Rust

**Machine:** \`$(uname -m)\` · **Date:** $(date -u +%Y-%m-%dT%H:%M:%SZ) · **Best-of:** $RUNS runs

## Workloads

| Benchmark              | Workload                                                                  |
|------------------------|---------------------------------------------------------------------------|
| \`bench_memory_leak\`   | 50M short-lived array/dict allocs in deeply nested loops, two passes        |
| \`bench_cpu_intensive\` | 10M integer accum + 1M pow/mod + 100k array fill/sum                       |
| \`bench_string_concats\`| 50×5000 single-char concat + 20×2000 multi-char concat + 10k final concat   |

## Execution time (lower is better)

| Benchmark              |    ZZ     | Rust    | Go      |
|------------------------|----------:|--------:|--------:|
EOF

for bench in "${BENCHES[@]}"; do
	z=$(val "$bench" zz elapsed_ms)
	r=$(val "$bench" rust elapsed_ms)
	g=$(val "$bench" go elapsed_ms)
	printf "| \`bench_%-16s\` | %s ms | %s ms | %s ms |\n" \
		"$bench" "$z" "$r" "$g" >>"$RESULTS_MD"
done

cat >>"$RESULTS_MD" <<EOF

## Peak RSS (lower is better; KiB / MB)

| Benchmark              |    ZZ     | Rust    | Go      |
|------------------------|----------:|--------:|--------:|
EOF
for bench in "${BENCHES[@]}"; do
	z=$(val "$bench" zz peak_rss_kib)
	r=$(val "$bench" rust peak_rss_kib)
	g=$(val "$bench" go peak_rss_kib)
	zmb=$(awk -v k="$z" 'BEGIN { printf "%.2f", k/1024 }')
	rmb=$(awk -v k="$r" 'BEGIN { printf "%.2f", k/1024 }')
	gmb=$(awk -v k="$g" 'BEGIN { printf "%.2f", k/1024 }')
	printf "| \`bench_%-16s\` | %s / %sMB | %s / %sMB | %s / %sMB |\n" \
		"$bench" "$z" "$zmb" "$r" "$rmb" "$g" "$gmb" >>"$RESULTS_MD"
done

cat >>"$RESULTS_MD" <<EOF

## Binary size (KiB)

| Engine | Binary KiB |
|--------|-----------:|
| ZZ     | $zz_bin_kib |
| Go     | $go_bin_kib |
| Rust   | $rust_bin_kib |

## How to reproduce

\`\`\`bash
cargo build --release
RUNS=5 bash bench/performance_check/run.sh
\`\`\`

- ZZ column uses \`zz build -p <file>\` (AOT native, \`-O3\`).
- Go/Rust compiled once into \`bench/performance_check/.bin/\`.
- Peak RSS via \`/usr/bin/time -v\` (GNU) or \`ps\` polling fallback.
- Machine-readable: \`bench/performance_check/results.json\`.

EOF

echo
echo "${BLD}${GRN}done${RST}"
echo "  table → ${BLD}$RESULTS_MD${RST}"
echo "  json  → ${BLD}$RESULTS_JSON${RST}"
echo
cat "$RESULTS_MD"
