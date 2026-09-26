#!/usr/bin/env bash
#
# pytest-flake-probe.sh — run the full pytest suite N times, serially, and
# report a one-line verdict per run plus a final tally.
#
# Motivation: the suite intermittently fails exactly one test per run, and the
# victim migrates. `filterwarnings = ["error"]` turns a ResourceWarning raised
# while the garbage collector finalises an object into an error attributed to
# whichever test that xdist worker happens to be running, and parallel workers
# also put timing-sensitive tests under CPU contention. One run proves nothing;
# this harness measures the rate so a fix's effect is visible.
#
# Usage:
#   scripts/pytest-flake-probe.sh [RUNS]      # RUNS defaults to 5
#
# Env:
#   FLAKE_PROBE_LOGDIR  where per-run pytest logs land (default: a mktemp dir)
#   FLAKE_PROBE_EXTRA   extra args appended to the pytest invocation
#
# The harness runs one full suite at a time and nothing else: running the suite
# concurrently with another heavy job manufactures the very flakiness it is
# measuring (see AGENTS.md, "Rust test scope"). It uses no retry plugin.
#
# The invocation is the pyproject-configured suite (xdist, marker filter) minus
# `-x`, so a run reports every failure rather than stopping at the first. `-rf`
# and `--tb=line` keep the log small while still naming each failure and its
# exception.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

RUNS="${1:-5}"
LOGDIR="${FLAKE_PROBE_LOGDIR:-$(mktemp -d "${TMPDIR:-/tmp}/pytest-flake-probe.XXXXXX")}"
mkdir -p "$LOGDIR"

PYTEST=(uv run --no-sync pytest -q --no-header -rf --tb=line)
if [ -n "${FLAKE_PROBE_EXTRA:-}" ]; then
    read -r -a extra_args <<< "${FLAKE_PROBE_EXTRA}"
    PYTEST+=("${extra_args[@]}")
fi

declare -A failure_counts=()
clean=0
failed=0

for i in $(seq 1 "$RUNS"); do
    log="$LOGDIR/run-$i.log"
    start=$(date +%s)
    "${PYTEST[@]}" >"$log" 2>&1
    status=$?
    elapsed=$(( $(date +%s) - start ))

    if [ "$status" -eq 0 ]; then
        clean=$((clean + 1))
        printf 'run %d (%ds): CLEAN\n' "$i" "$elapsed"
        continue
    fi

    failed=$((failed + 1))
    mapfile -t fail_lines < <(grep -E '^(FAILED|ERROR) ' "$log" || true)
    if [ "${#fail_lines[@]}" -eq 0 ]; then
        printf 'run %d (%ds): FAIL (no FAILED line; log: %s)\n' "$i" "$elapsed" "$log"
        failure_counts["<unparsed>"]=$(( ${failure_counts["<unparsed>"]:-0} + 1 ))
        continue
    fi

    printf 'run %d (%ds): FAIL\n' "$i" "$elapsed"
    for line in "${fail_lines[@]}"; do
        printf '    %s\n' "$line"
        id="${line#FAILED }"
        id="${id#ERROR }"
        id="${id%% *}"
        failure_counts["$id"]=$(( ${failure_counts["$id"]:-0} + 1 ))
    done
done

echo
printf 'tally: %d run(s), %d clean, %d failed\n' "$RUNS" "$clean" "$failed"
if [ "${#failure_counts[@]}" -gt 0 ]; then
    echo 'distinct failures:'
    for id in "${!failure_counts[@]}"; do
        printf '%4dx %s\n' "${failure_counts["$id"]}" "$id"
    done | sort -rn
fi
echo "logs: $LOGDIR"

[ "$failed" -eq 0 ]
