#!/usr/bin/env bash
# Reproducible R3 benchmark run — POSIX.
# Records the environment block (commit, rustc, CPU, RAM) into R3_BENCH_*
# env vars consumed by the harness, then runs the release binary and leaves
# the report in benchmarks/results/r3/.
set -euo pipefail

ROWS=100000
ITERATIONS=3
OUT="${PWD}/benchmarks/results/r3"

usage() {
  echo "usage: $0 [--rows N] [--iterations K] [--out DIR]"
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --rows) ROWS="$2"; shift 2 ;;
    --iterations) ITERATIONS="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    -h|--help) usage ;;
    *) echo "unknown argument: $1"; usage ;;
  esac
done

export R3_BENCH_COMMIT="$(git rev-parse HEAD 2>/dev/null || echo 'n/a')"
export R3_BENCH_RUSTC="$(rustc --version 2>/dev/null || echo 'n/a')"
if [[ -f /proc/cpuinfo ]]; then
  export R3_BENCH_CPU="$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | sed 's/^ //')"
else
  export R3_BENCH_CPU="n/a"
fi
if [[ -f /proc/meminfo ]]; then
  export R3_BENCH_RAM="$(grep -m1 MemTotal /proc/meminfo | awk '{print $2 " kB"}')"
else
  export R3_BENCH_RAM="n/a"
fi

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target-repro}"
echo "commit=$R3_BENCH_COMMIT rustc=$R3_BENCH_RUSTC cpu=$R3_BENCH_CPU ram=$R3_BENCH_RAM"

cargo run -p qmind-sql --release --bin bench_r3 -- \
  --rows "$ROWS" --iterations "$ITERATIONS" --out "$OUT"