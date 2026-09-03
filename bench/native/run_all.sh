#!/usr/bin/env bash
# =====================================================================
#  ZZ PERFORMANCE SUITE
#  zz VM vs zz --native vs zz build (dev/release) vs Go vs Bun
# =====================================================================
set -u
cd "$(dirname "$0")"

# ---- colors -----------------------------------------------------------
C_RESET=$'\e[0m'
C_BOLD=$'\e[1m'
C_DIM=$'\e[2m'
C_RED=$'\e[31m'
C_GREEN=$'\e[32m'
C_YELLOW=$'\e[33m'
C_BLUE=$'\e[34m'
C_MAGENTA=$'\e[35m'
C_CYAN=$'\e[36m'
C_BG=$'\e[47m\e[30m'

ZZ="$PWD/../../target/release/zz"
[ -x "$ZZ" ] || ZZ="$PWD/../../target/debug/zz"
[ -x "$ZZ" ] || {
	echo "build zz first: cargo build --release"
	exit 1
}

RUNS="${RUNS:-3}" # iterations per benchmark (best-of)

# ---- timing -----------------------------------------------------------
time_ms() { # time_ms <cmd...> -> best ms
	local best=999999999
	for _ in $(seq "$RUNS"); do
		local t0=$(date +%s%N)
		"$@" >/dev/null 2>&1
		local t1=$(date +%s%N)
		local dt=$(((t1 - t0) / 1000000))
		[ "$dt" -lt "$best" ] && best=$dt
	done
	echo "$best"
}

# ---- engine labels ----------------------------------------------------
ENGINES=(vm native build buildp go bun)
ENLABEL=("zz VM" "zz --native" "zz build" "zz build -p" "Go" "Bun")
EN_COLOR=("$C_CYAN" "$C_BLUE" "$C_MAGENTA" "$C_GREEN" "$C_YELLOW" "$C_RED")

declare -A RESULTS # bench->engine->ms

# ---- prepare Go / Bun -------------------------------------------------
(cd _go && go build -o main .) >/dev/null 2>&1 || echo "go build failed"

# ----------------------------------------------------------------------
#  Build zz binaries (cached)
# ----------------------------------------------------------------------
banner() {
	echo
	echo "${C_BG}${C_BOLD}   Z Z   P E R F O R M A N C E   S U I T E   ${C_RESET}"
	echo "${C_DIM}  zz VM  |  zz --native  |  zz build  |  zz build -p  |  Go  |  Bun  |  $(uname -m)${C_RESET}"
	echo
}

eng_label() { # eng_label <engine-idx>
	echo -n "${EN_COLOR[$1]}${ENLABEL[$1]}${C_RESET}"
}

# run engine for a benchmark file (name without .zz)
run_one() {
	local name="$1"
	local f="categories/$name.zz"
	local buildp="$PWD/.bin/${name}_rel"
	local buildd="$PWD/.bin/${name}_dev"

	# Pre-build (cached) zz variants so timing is pure exec. `zz build` puts
	# the binary next to the source (categories/<name>); copy into .bin.
	$ZZ build -p "$f" >/dev/null 2>&1
	cp -f "categories/$name" "$buildp" 2>/dev/null || true
	$ZZ build "$f" >/dev/null 2>&1
	cp -f "categories/$name" "$buildd" 2>/dev/null || true
	chmod +x "$buildp" "$buildd" 2>/dev/null || true

	RESULTS["$name:vm"]="$(time_ms $ZZ run "$f")"
	RESULTS["$name:native"]="$(time_ms $ZZ run --native "$f")"
	RESULTS["$name:build"]="$(time_ms "$buildd")"
	RESULTS["$name:buildp"]="$(time_ms "$buildp")"
	RESULTS["$name:go"]="$(time_ms _go/main "$name")"
	RESULTS["$name:bun"]="$(time_ms bun _go/main.ts "$name")"
}

# ----------------------------------------------------------------------
#  Table rendering
# ----------------------------------------------------------------------
colw=10

render_table() {
	local pct last col
	# header
	printf "  %-12s│" ""
	for j in $(seq 0 5); do
		printf " %10s │" "${ENLABEL[$j]}"
	done
	printf "  %s\n" "vs-fastest"
	printf "  %-12s├" ""
	for j in $(seq 0 5); do printf "────────────┼"; done
	printf "─────────\n"

	for name in "$@"; do
		# find best
		best=999999999
		for j in 0 1 2 3 4 5; do
			v=${RESULTS["$name:${ENGINES[$j]}"]}
			[ "$v" -lt "$best" ] && best=$v
		done
		printf "  ${C_BOLD}%-12s${C_RESET}│" "$name"
		for j in 0 1 2 3 4 5; do
			v=${RESULTS["$name:${ENGINES[$j]}"]}
			# pct vs fastest (integer)
			pct=$(awk "BEGIN{printf \"%d\", $v*100/$best}")
			[ "$pct" -lt 1 ] && pct=1
			if [ "$v" -eq "$best" ]; then
				printf " ${C_GREEN}%8sms▀${C_RESET}  │" "$v"
			else
				printf " ${C_DIM}%8sms${C_RESET}  │" "$v"
			fi
			RESULTS["$name:pct$j"]=$pct
		done
		# pct-of-fastest summary (colored, each with %)
		local pcts=""
		for j in 0 1 2 3 4 5; do
			pcts="${pcts}${EN_COLOR[$j]}${RESULTS["$name:pct$j"]}%${C_RESET} "
		done
		printf " %s" "$pcts"
		echo
		# bar row: percent fill (fastest=full width)
		printf "  %-12s│" ""
		for j in 0 1 2 3 4 5; do
			pct="${RESULTS["$name:pct$j"]}"
			[ "$pct" -gt 400 ] && pct=400
			filled=$((pct * 10 / 400))
			[ "$filled" -gt 10 ] && filled=10
			empty=$((10 - filled))
			local fstr="" estr=""
			[ "$filled" -gt 0 ] && fstr=$(printf '█%.0s' $(seq 1 "$filled"))
			[ "$empty" -gt 0 ] && estr=$(printf '░%.0s' $(seq 1 "$empty"))
			printf " ${C_GREEN}%s${C_DIM}%s${C_RESET} │" "$fstr" "$estr"
		done
		printf "  (bars: shorter = faster)\n"
	done
}

# ----------------------------------------------------------------------
#  Run everything
# ----------------------------------------------------------------------
banner
mkdir -p .bin
echo "${C_DIM}Preparing zz native binaries (cached) ...${C_RESET}"
run_one loop
run_one fib
run_one string
run_one math

echo
echo "${C_BOLD}Benchmarks${C_RESET}  (lower is better, best of $RUNS runs)"
echo
render_table loop fib string math

# ---- summary: mean speedup vs zz VM ------------------------------------
echo
echo "${C_BOLD}Summary — mean speedup vs zz VM (higher = faster)${C_RESET}"
printf "  %-12s│" ""
for j in 0 1 2 3 4 5; do printf " %10s │" "${ENLABEL[$j]}"; done
echo
printf "  %-12s├" ""
for j in $(seq 0 5); do printf "────────────┼"; done
echo "──────────"
for name in loop fib string math; do
	vm_ms=${RESULTS["$name:vm"]}
	printf "  ${C_BOLD}%-12s${C_RESET}│" "$name"
	for j in 0 1 2 3 4 5; do
		v=${RESULTS["$name:${ENGINES[$j]}"]}
		[ "$j" -eq 0 ] && {
			printf " %9sx │" "1.0"
			continue
		}
		sp=$(echo "scale=2; $vm_ms / $v" | bc)
		printf " %8sx │" "$sp"
	done
	echo
done

# geometric-mean summary column (awk log-mean, avoids bc fractional ^)
geom_calc() {
	local j=$1
	awk -v vm0="${RESULTS["loop:vm"]}" -v m0="${RESULTS["loop:${ENGINES[$j]}"]}" \
		-v vm1="${RESULTS["fib:vm"]}" -v m1="${RESULTS["fib:${ENGINES[$j]}"]}" \
		-v vm2="${RESULTS["string:vm"]}" -v m2="${RESULTS["string:${ENGINES[$j]}"]}" \
		-v vm3="${RESULTS["math:vm"]}" -v m3="${RESULTS["math:${ENGINES[$j]}"]}" \
		'BEGIN{ if (m0==0||m1==0||m2==0||m3==0){printf "n/a"; exit}
	            r=(vm0/m0)*(vm1/m1)*(vm2/m2)*(vm3/m3);
	            printf "%.2f", exp(log(r)/4) }'
}
# overall row
echo
printf "  ${C_BOLD}overall${C_RESET}"
for j in 0 1 2 3 4 5; do
	if [ "$j" -eq 0 ]; then gm="1.0"; else gm=$(geom_calc "$j"); fi
	printf " │ ${EN_COLOR[$j]}%8sx${C_RESET}" "$gm"
done
echo "  (geometric mean vs zz VM)"
echo
echo "${C_DIM}Machine: $(uname -m) · Runs: best-of-$RUNS · Date: $(date +%Y-%m-%d)${C_RESET}"
echo
