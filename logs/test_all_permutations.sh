#!/usr/bin/env bash
# test_all_permutations.sh — Run the bot for all 27 three-hop permutations.
#
# Multiple permutations run in parallel (default: 4, configurable via
# PARALLEL env var). Each permutation runs for 10 minutes (configurable
# via TEST_DURATION env var). Results are saved to logs/permutation_results.tsv
#
# Simulation outcomes are classified into three candidate-outcome categories:
#   ok        — simulation completed without revert (profitable / below_threshold)
#   no_profit — simulation completed (no revert), but gross profit ≤ 0
#   revert    — simulation reverted on-chain
# Each reverted candidate is then attributed four-way (parsed from its
# [sim-diag] JSON line; see logs/permutation_analyzer.py): Drift /
# SolverCalc / Encoding / Unknown — never dismissed as 'stale'.
#
# The simulatability rate = (ok + no_profit) / (ok + no_profit + revert).
# A "no_profit" path is not broken — the encoding works, the arb just isn't profitable.
#
# Usage: ./logs/test_all_permutations.sh [START_INDEX]
#
# START_INDEX: 1-27, to resume from a specific permutation (default: 1)
# PARALLEL:    max concurrent bots (default: 4)
# TEST_DURATION: minutes per permutation (default: 15)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RESULTS_FILE="${SCRIPT_DIR}/permutation_results.tsv"
LOCK_FILE="${SCRIPT_DIR}/.results.lock"
JOBS_DIR="${SCRIPT_DIR}/.jobs"
DURATION_MINUTES="${TEST_DURATION:-15}"
PARALLEL="${PARALLEL:-4}"

# ── Pre-flight: kill leftover workers + bots from any previous run ──────────
# A ^C on a prior run leaves background worker subshells alive (their SIGINT is
# ignored in non-interactive bash). When their `sleep $DURATION` finally expires
# they re-emerge and double-stop / double-analyze bots, overwriting good TSV
# rows with outdated results. Kill any `test_all_permutations.sh` worker processes
# (excluding this very invocation) plus any stray bots before starting fresh.
_self=$$
_leftover=$(pgrep -f "test_all_permutations.sh" 2>/dev/null | grep -v "^${_self}$" || true)
if [[ -n "$_leftover" ]]; then
    echo "Killing leftover harness workers: $_leftover"
    kill $_leftover 2>/dev/null || true
    sleep 1
    kill -9 $_leftover 2>/dev/null || true
fi
pkill -f "eth_backrun_v2_v3_v4_rust.py --permutation" 2>/dev/null || true
sleep 2

PERMUTATIONS=(
    "V2-V2-V2"
    "V2-V2-V3"
    "V2-V2-V4"
    "V2-V3-V2"
    "V2-V3-V3"
    "V2-V3-V4"
    "V2-V4-V2"
    "V2-V4-V3"
    "V2-V4-V4"
    "V3-V2-V2"
    "V3-V2-V3"
    "V3-V2-V4"
    "V3-V3-V2"
    "V3-V3-V3"
    "V3-V3-V4"
    "V3-V4-V2"
    "V3-V4-V3"
    "V3-V4-V4"
    "V4-V2-V2"
    "V4-V2-V3"
    "V4-V2-V4"
    "V4-V3-V2"
    "V4-V3-V3"
    "V4-V3-V4"
    "V4-V4-V2"
    "V4-V4-V3"
    "V4-V4-V4"
)

START_INDEX="${1:-1}"

# Ensure TSV exists with header before launching parallel workers
if [[ ! -f "$RESULTS_FILE" ]] || [[ ! -s "$RESULTS_FILE" ]]; then
    uv run python3 -c "from logs.permutation_analyzer import tsv_header; print(tsv_header())" > "$RESULTS_FILE"
    echo "Created results file: $RESULTS_FILE"
fi

# Clean up job tracking directory
rm -rf "$JOBS_DIR"
mkdir -p "$JOBS_DIR"

# Forward SIGINT/SIGTERM to the whole process group so background worker
# subshells actually die when the user hits ^C — otherwise they survive the
# main script's death (background jobs ignore SIGINT by default in
# non-interactive bash, but NOT SIGTERM) and later double-stop /
# double-analyze bots from a later run, overwriting good TSV rows.
# `kill -TERM 0` signals every process in our group, workers included.
_CLEANING=0
_cleanup() {
    [[ "$_CLEANING" -eq 1 ]] && return 0
    _CLEANING=1
    pkill -f "eth_backrun_v2_v3_v4_rust.py --permutation" 2>/dev/null || true
    kill -TERM 0 2>/dev/null || true
    rm -rf "$JOBS_DIR"
}
trap '_cleanup; exit 130' INT
trap '_cleanup; exit 143' TERM
trap '_cleanup' EXIT

# --- Worker: runs a single permutation end-to-end ---
run_permutation() {
    local i=$1
    local PERM=$2
    local LOGFILE="${SCRIPT_DIR}/perm-${PERM}.log"
    local STARTFILE="${JOBS_DIR}/starts/${i}"

    echo "=== [$i/27] Starting $PERM ==="

    # Kill any existing run for this permutation.
    EXISTING=$(pgrep -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true)
    if [[ -n "$EXISTING" ]]; then
        echo "Killing existing process(es) for $PERM: $EXISTING"
        pkill -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true
        sleep 10
        pkill -9 -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true
    fi

    # Start the bot
    cd /home/ralph/code/degenbot
    nohup uv run python examples/eth_backrun_v2_v3_v4_rust.py \
        --permutation "$PERM" \
        > "$LOGFILE" 2>&1 &

    # Record when we launched the bot THIS invocation. Used after the sleep to
    # detect zombie re-analysis: a worker from a ^C'd prior run has no fresh
    # STARTFILE (its JOBS_DIR was wiped by `rm -rf` at the top of this run),
    # so if STARTFILE is outdated/missing we refuse to overwrite good TSV rows.
    mkdir -p "${JOBS_DIR}/starts"
    date +%s > "$STARTFILE"

    # Wait for duration
    sleep "$((DURATION_MINUTES * 60))"

    # Stop the bot
    echo "=== [$i/27] Stopping bot for $PERM ==="
    pkill -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true
    sleep 3
    pkill -9 -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true

    # ── Zombie re-analysis guard ────────────────────────────────────────────
    # Only analyze if the bot actually wrote to the log during THIS invocation
    # (log mtime >= our recorded launch epoch). A zombie worker from an aborted
    # previous run will reference a STARTFILE/LOGFILE state that predates this
    # run, so its log mtime will be older than its own (bogus) launch epoch —
    # refuse to analyze rather than overwrite good results with outdated data.
    if [[ ! -f "$LOGFILE" ]] || [[ ! -f "$STARTFILE" ]]; then
        echo "=== [$i/27] $PERM: missing log/start file, skipping analysis ==="
        return 0
    fi
    local LAUNCH_EPOCH LOG_MTIME
    LAUNCH_EPOCH=$(cat "$STARTFILE")
    LOG_MTIME=$(stat -c %Y "$LOGFILE")
    if [[ "$LOG_MTIME" -lt "$LAUNCH_EPOCH" ]]; then
        echo "=== [$i/27] $PERM: log predates this run's bot launch (outdated), skipping analysis ==="
        return 0
    fi

    # ── Analyze results: four-way classification (Drift/SolverCalc/Encoding/
    # Unknown) from [sim-diag] JSON lines, via the shared analyzer module.
    # Falls back to Unknown for every revert when [sim-diag] is absent (older
    # logs). NoProfit comes from the bot's authoritative 'by reason:' summary.
    ROW=$(cd /home/ralph/code/degenbot && uv run python3 -c "
from logs.permutation_analyzer import analyze_logfile, result_to_tsv_row
r = analyze_logfile('$LOGFILE', '$PERM')
print(result_to_tsv_row($i, r))
")
    TOTAL=$(echo "$ROW" | cut -f3)
    OK_TOTAL=$(echo "$ROW" | cut -f4)
    NO_PROFIT=$(echo "$ROW" | cut -f5)
    REVERT_TOTAL=$(echo "$ROW" | cut -f6)
    PERCENT=$(echo "$ROW" | cut -f7)
    CLASS=$(echo "$ROW" | cut -f8)
    DRIFT=$(echo "$ROW" | cut -f9)
    SOLVERCALC=$(echo "$ROW" | cut -f10)
    ENCODING=$(echo "$ROW" | cut -f11)
    UNKNOWN=$(echo "$ROW" | cut -f12)

    echo "=== [$i/27] $PERM: ok=$OK_TOTAL no_profit=$NO_PROFIT reverts=$REVERT_TOTAL (drift=$DRIFT solvercalc=$SOLVERCALC encoding=$ENCODING unknown=$UNKNOWN) simulatable=$PERCENT $CLASS ==="

    # Update TSV under lock (prevent parallel workers corrupting the file)
    (
        flock -x 200
        sed -i "/\t${PERM}\t/d" "$RESULTS_FILE"
        echo "$ROW" >> "$RESULTS_FILE"
        { head -1 "$RESULTS_FILE"; tail -n +2 "$RESULTS_FILE" | sort -t$'\t' -k1,1n; } > "${RESULTS_FILE}.tmp" && mv "${RESULTS_FILE}.tmp" "$RESULTS_FILE"
    ) 200>"$LOCK_FILE"
}

echo "=== Testing all 27 permutations (${DURATION_MINUTES} min each, ${PARALLEL} parallel) ==="
echo "Starting from index $START_INDEX"
echo ""

for i in $(seq "$START_INDEX" 27); do
    PERM="${PERMUTATIONS[$((i - 1))]}"

    # Throttle: wait until a slot opens up. Count only numeric job markers,
    # NOT the `starts/*` launch-epoch files (which live under JOBS_DIR too).
    while [[ $(find "$JOBS_DIR" -maxdepth 1 -type f -name '[0-9]*' 2>/dev/null | wc -l) -ge $PARALLEL ]]; do
        sleep 10
    done

    # Launch worker in a subshell — creates a job marker, removes when done.
    (
        touch "$JOBS_DIR/$i"
        run_permutation "$i" "$PERM"
        rm -f "$JOBS_DIR/$i"
    ) &
done

echo ""
echo "All permutations launched — waiting for remaining workers..."
wait

echo ""
echo "=== All permutations complete ==="
echo "Results: $RESULTS_FILE"
cat "$RESULTS_FILE"