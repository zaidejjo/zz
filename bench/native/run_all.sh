#!/usr/bin/env bash
# =====================================================================
#  ZZ PERFORMANCE SUITE
#  zz VM vs zz build (dev/release) vs Go vs Bun
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
# ENGINES / ENLABEL / EN_COLOR MUST stay same length & same order — index
# j always refers to the same engine across all three, and across RESULTS.
ENGINES=(vm build buildp go bun)
ENLABEL=("zz VM" "zz build" "zz build -p" "Go" "Bun")
EN_COLOR=("$C_CYAN" "$C_BLUE" "$C_MAGENTA" "$C_GREEN" "$C_YELLOW")
N=${#ENGINES[@]}

declare -A RESULTS # bench->engine->ms

# =====================================================================
#  SINGLE SOURCE OF TRUTH FOR TABLE LAYOUT
#  Every row (header, separator, values, bars, summary) is built from
#  these two numbers only. Change them here and every row stays aligned.
# =====================================================================
NAME_W=12 # width of the leftmost "loop / fib / ..." column
CELL_W=10 # visible width of every data cell's inner content

# Prints one bordered cell. $1 = the *plain* text, already exactly
# CELL_W visible chars (built with printf %*s / %-*s beforehand,
# never containing color codes — color codes go in $2 instead so the
# width math is never thrown off by invisible escape sequences).
cell() {
  local text="$1" color="${2:-}"
  printf " %s%s%s │" "$color" "$text" "$C_RESET"
}

# Right-pads/aligns arbitrary text to exactly CELL_W visible chars.
pad_cell() { printf '%*s' "$CELL_W" "$1"; }

# ---- prepare Go / Bun -------------------------------------------------
(cd _go && go build -o main .) >/dev/null 2>&1 || echo "go build failed"

# ----------------------------------------------------------------------
banner() {
  echo
  echo "${C_BG}${C_BOLD}   Z Z   P E R F O R M A N C E   S U I T E   ${C_RESET}"
  local hdr="  zz VM"
  for ((j = 1; j < N; j++)); do hdr+="  |  ${ENLABEL[$j]}"; done
  echo "${C_DIM}${hdr}  |  $(uname -m)${C_RESET}"
  echo
}

run_one() {
  local name="$1"
  local f="categories/$name.zz"
  local buildp="$PWD/.bin/${name}_rel"
  local buildd="$PWD/.bin/${name}_dev"

  $ZZ build -p "$f" >/dev/null 2>&1
  cp -f "categories/$name" "$buildp" 2>/dev/null || true
  $ZZ build "$f" >/dev/null 2>&1
  cp -f "categories/$name" "$buildd" 2>/dev/null || true
  chmod +x "$buildp" "$buildd" 2>/dev/null || true

  RESULTS["$name:vm"]="$(time_ms $ZZ run "$f")"
  RESULTS["$name:build"]="$(time_ms "$buildd")"
  RESULTS["$name:buildp"]="$(time_ms "$buildp")"
  RESULTS["$name:go"]="$(time_ms _go/main "$name")"
  RESULTS["$name:bun"]="$(time_ms bun _go/main.ts "$name")"
}

# ----------------------------------------------------------------------
#  Table rendering — header / values / bars all go through cell()
# ----------------------------------------------------------------------
bar_content() { # bar_content <filled> -> exactly CELL_W visible chars (colored)
  local filled=$1 empty=$((CELL_W - filled)) fstr="" estr="" i
  for ((i = 0; i < filled; i++)); do fstr+="█"; done
  for ((i = 0; i < empty; i++)); do estr+="░"; done
  printf "%s%s%s%s%s" "$C_GREEN" "$fstr" "$C_DIM" "$estr" "$C_RESET"
}

table_header() {
  printf "  %-${NAME_W}s│" ""
  for ((j = 0; j < N; j++)); do
    cell "$(pad_cell "${ENLABEL[$j]}")"
  done
  printf "  %s\n" "vs-fastest"

  printf "  %-${NAME_W}s├" ""
  local dashes
  dashes=$(printf '─%.0s' $(seq 1 $((CELL_W + 2))))
  for ((j = 0; j < N; j++)); do printf "%s┼" "$dashes"; done
  printf "─────────\n"
}

render_table() {
  table_header
  for name in "$@"; do
    # Find best
    best=999999999
    for ((j = 0; j < N; j++)); do
      v=${RESULTS["$name:${ENGINES[$j]}"]}
      [ "$v" -lt "$best" ] && best=$v
    done

    # Row 1: Time values
    printf "  ${C_BOLD}%-${NAME_W}s${C_RESET}│" "$name"
    for ((j = 0; j < N; j++)); do
      v=${RESULTS["$name:${ENGINES[$j]}"]}
      pct=$(awk "BEGIN{printf \"%d\", $v*100/$best}")
      [ "$pct" -lt 1 ] && pct=1
      RESULTS["$name:pct$j"]=$pct

      local content
      content=$(pad_cell "${v}ms")
      if [ "$v" -eq "$best" ]; then
        cell "$content" "${C_GREEN}${C_BOLD}"
      else
        cell "$content" "$C_DIM"
      fi
    done

    # Pct summary side-text
    local pcts=""
    for ((j = 0; j < N; j++)); do
      pcts="${pcts}${EN_COLOR[$j]}${RESULTS["$name:pct$j"]}%${C_RESET} "
    done
    printf "  %s\n" "$pcts"

    # Row 2: Bars
    printf "  %-${NAME_W}s│" ""
    for ((j = 0; j < N; j++)); do
      pct="${RESULTS["$name:pct$j"]}"
      [ "$pct" -gt 400 ] && pct=400
      filled=$((pct * CELL_W / 400))
      [ "$filled" -gt "$CELL_W" ] && filled=$CELL_W

      cell "$(bar_content "$filled")"
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
printf "  %-${NAME_W}s│" ""
for ((j = 0; j < N; j++)); do cell "$(pad_cell "${ENLABEL[$j]}")"; done
echo
printf "  %-${NAME_W}s├" ""
dashes=$(printf '─%.0s' $(seq 1 $((CELL_W + 2))))
for ((j = 0; j < N; j++)); do printf "%s┼" "$dashes"; done
echo "──────────"

for name in loop fib string math; do
  vm_ms=${RESULTS["$name:vm"]}
  printf "  ${C_BOLD}%-${NAME_W}s${C_RESET}│" "$name"
  for ((j = 0; j < N; j++)); do
    v=${RESULTS["$name:${ENGINES[$j]}"]}
    if [ "$j" -eq 0 ]; then
      cell "$(pad_cell "1.0x")"
      continue
    fi
    sp=$(echo "scale=2; $vm_ms / $v" | bc)
    cell "$(pad_cell "${sp}x")"
  done
  echo
done

# geometric-mean summary column
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
printf "  ${C_BOLD}%-${NAME_W}s${C_RESET}│" "overall"
for ((j = 0; j < N; j++)); do
  if [ "$j" -eq 0 ]; then
    gm="1.00"
  else
    gm=$(geom_calc "$j")
  fi
  cell "$(pad_cell "${gm}x")" "${EN_COLOR[$j]}"
done
echo "  (geometric mean vs zz VM)"
echo
echo "${C_DIM}Machine: $(uname -m) · Runs: best-of-$RUNS · Date: $(date +%Y-%m-%d)${C_RESET}"
echo
