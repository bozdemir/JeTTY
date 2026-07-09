#!/bin/sh
# Informational perf report (v0.17) — NOT a gate.
#
# Runs the CPU-only bench a few times and prints its numbers so a PR reviewer (and
# the CI log) can see the trend. It NEVER fails the build: hard floors calibrated to
# a fast dev machine would false-fail on a 2-3x slower, sometimes sustained-contended
# shared GitHub runner (best-of-N does not defend sustained contention). Hard gating
# is a v0.18 follow-up, set at ~50% of the CI runner's observed minimum AFTER we have
# watched its real distribution across many runs (see docs/perf-budget.md).
#
# CPU-only avoids GPU-availability / software-rasterizer timing variance on runners.
# It is display-independent and deterministic.
#
# Usage: [BIN=path/to/jetty-bench] [N=5] scripts/perf-report.sh
set -u

BIN="${BIN:-target/release/jetty-bench}"
N="${N:-5}"

if [ ! -x "$BIN" ]; then
    echo "perf-report: bench binary not found/executable at '$BIN'" >&2
    echo "perf-report: build it first: cargo build --release -p jetty-app --bin jetty-bench" >&2
    # Informational job: do not fail the build on a missing binary either.
    exit 0
fi

echo "perf-report: JETTY_BENCH_CPU_ONLY=1 $BIN  (best-of-$N, informational — never fails the build)"
i=1
while [ "$i" -le "$N" ]; do
    echo "--- run $i ---"
    JETTY_BENCH_CPU_ONLY=1 "$BIN" || echo "perf-report: run $i exited non-zero (ignored)"
    i=$((i + 1))
done
echo "perf-report: done (informational; no thresholds enforced this release)"
exit 0
