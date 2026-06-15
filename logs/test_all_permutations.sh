#!/usr/bin/env bash
# test_all_permutations.sh — Run the bot for all 27 three-hop permutations.
#
# Multiple permutations run in parallel (default: 4, configurable via
# PARALLEL env var). Each permutation runs for 10 minutes (configurable
# via TEST_DURATION env var). Results are saved to logs/permutation_results.tsv
#
# Simulation outcomes are classified into three categories:
#   ok        — simulation completed without revert (subdivided: profitable / below_threshold)
#   no_profit — simulation completed (no revert), but gross profit ≤ 0
#   revert    — simulation reverted on-chain (encoding bug or token transfer issue)
#
# The simulatability rate = (ok + no_profit) / (ok + no_profit + revert).
# A "no_profit" path is not broken — the encoding works, the arb just isn't profitable.
#
# Usage: ./logs/test_all_permutations.sh [START_INDEX]
#
# START_INDEX: 1-27, to resume from a specific permutation (default: 1)
# PARALLEL:    max concurrent bots (default: 4)
# TEST_DURATION: minutes per permutation (default: 10)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RESULTS_FILE="${SCRIPT_DIR}/permutation_results.tsv"
LOCK_FILE="${SCRIPT_DIR}/.results.lock"
JOBS_DIR="${SCRIPT_DIR}/.jobs"
DURATION_MINUTES="${TEST_DURATION:-10}"
PARALLEL="${PARALLEL:-4}"

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
    echo -e "#\tPermutation\tCandidates\tSimOK\tNoProfit\tReverts\tSimRate\tClassification" > "$RESULTS_FILE"
    echo "Created results file: $RESULTS_FILE"
fi

# Clean up job tracking directory
rm -rf "$JOBS_DIR"
mkdir -p "$JOBS_DIR"
trap 'rm -rf "$JOBS_DIR"' EXIT

# --- Worker: runs a single permutation end-to-end ---
run_permutation() {
    local i=$1
    local PERM=$2
    local LOGFILE="${SCRIPT_DIR}/perm-${PERM}.log"

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

    # Wait for duration
    sleep "$((DURATION_MINUTES * 60))"

    # Stop the bot
    echo "=== [$i/27] Stopping bot for $PERM ==="
    pkill -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true
    sleep 3
    pkill -9 -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true

    # ── Analyze results (three-category split) ──
    ANALYSIS=$(cd /home/ralph/code/degenbot && uv run python3 -c "
import re, sys
log = open('$LOGFILE').read()

# [sim] summary: 'N ok (X profitable, Y below threshold), M failed, Z exceptions'
ok_total = sum(int(m.group(1)) for m in re.finditer(r'(\d+) ok \(', log))

# sim-fail lines split into: no_profit vs revert
no_profit = log.count('no profit')
revert_total = log.count('revert=')

total = ok_total + no_profit + revert_total

if total == 0:
    cls = '⬜ No Opportunity'
    pct = '—'
else:
    sim_ok = ok_total + no_profit
    pct_val = sim_ok * 100 // total
    if pct_val >= 80: cls = '✅ Passing'
    elif pct_val >= 20: cls = '⚠️ Partial'
    else: cls = '❌ Broken'
    pct = f'{pct_val}%'
print(f'{total}\t{ok_total}\t{no_profit}\t{revert_total}\t{pct}\t{cls}')
")
    TOTAL=$(echo "$ANALYSIS" | cut -f1)
    OK_TOTAL=$(echo "$ANALYSIS" | cut -f2)
    NO_PROFIT=$(echo "$ANALYSIS" | cut -f3)
    REVERT_TOTAL=$(echo "$ANALYSIS" | cut -f4)
    PERCENT=$(echo "$ANALYSIS" | cut -f5)
    CLASS=$(echo "$ANALYSIS" | cut -f6)

    echo "=== [$i/27] $PERM: ok=$OK_TOTAL no_profit=$NO_PROFIT reverts=$REVERT_TOTAL simulatable=$PERCENT $CLASS ==="

    # Update TSV under lock (prevent parallel workers corrupting the file)
    (
        flock -x 200
        sed -i "/\t${PERM}\t/d" "$RESULTS_FILE"
        echo -e "$i\t$PERM\t$TOTAL\t$OK_TOTAL\t$NO_PROFIT\t$REVERT_TOTAL\t$PERCENT\t$CLASS" >> "$RESULTS_FILE"
        { head -1 "$RESULTS_FILE"; tail -n +2 "$RESULTS_FILE" | sort -t$'\t' -k1,1n; } > "${RESULTS_FILE}.tmp" && mv "${RESULTS_FILE}.tmp" "$RESULTS_FILE"
    ) 200>"$LOCK_FILE"
}

echo "=== Testing all 27 permutations (${DURATION_MINUTES} min each, ${PARALLEL} parallel) ==="
echo "Starting from index $START_INDEX"
echo ""

for i in $(seq "$START_INDEX" 27); do
    PERM="${PERMUTATIONS[$((i - 1))]}"

    # Throttle: wait until a slot opens up
    while [[ $(ls "$JOBS_DIR" 2>/dev/null | wc -l) -ge $PARALLEL ]]; do
        sleep 10
    done

    # Launch worker in a subshell — creates a job marker, removes when done
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
