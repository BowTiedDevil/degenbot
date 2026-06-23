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
# rows with stale results. Kill any `test_all_permutations.sh` worker processes
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
    echo -e "#\tPermutation\tCandidates\tSimOK\tNoProfit\tReverts\tSimRate\tClassification\tIIA_Reverts\tOther_Reverts" > "$RESULTS_FILE"
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
    # so if STARTFILE is stale/missing we refuse to overwrite good TSV rows.
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
    # refuse to analyze rather than overwrite good results with stale data.
    if [[ ! -f "$LOGFILE" ]] || [[ ! -f "$STARTFILE" ]]; then
        echo "=== [$i/27] $PERM: missing log/start file, skipping analysis ==="
        return 0
    fi
    local LAUNCH_EPOCH LOG_MTIME
    LAUNCH_EPOCH=$(cat "$STARTFILE")
    LOG_MTIME=$(stat -c %Y "$LOGFILE")
    if [[ "$LOG_MTIME" -lt "$LAUNCH_EPOCH" ]]; then
        echo "=== [$i/27] $PERM: log predates this run's bot launch (stale), skipping analysis ==="
        return 0
    fi

    # ── Analyze results (three-category split, with revert decomposition) ──
    ANALYSIS=$(cd /home/ralph/code/degenbot && uv run python3 -c "
import re, sys
log = open('$LOGFILE').read()

# [sim] summary: 'N ok (X profitable, Y below threshold), M failed, Z exceptions'
ok_total = sum(int(m.group(1)) for m in re.finditer(r'(\d+) ok \(', log))

# Count reverts from [sim-fail] lines ONLY (not [sim-revert-data] which
# duplicates the same data with calldata for offline debugging).
revert_total = len(re.findall(r'\[sim-fail\].*revert=0x', log))
no_profit = len(re.findall(r'\[sim-fail\].*no profit', log))

# Decompose reverts by the decoded reason.
# error.data extraction means revert_hex is now populated for all
# Error(string) reverts. The revert_reason is appended after the hex.
# Log format: [sim-fail] ... revert=0x08c379a0... IIA ...
# or: [sim-fail] ... revert=0x4e487b71... PANIC(0x...) ...
# or: [sim-fail] ... revert=0x5212cba1... CurrencyNotSettled() ...

iia_reverts = len(re.findall(r'\[sim-fail\].*revert=0x.*IIA', log))
currency_not_settled = len(re.findall(r'\[sim-fail\].*revert=0x.*CurrencyNotSettled', log))
invalid_command = len(re.findall(r'\[sim-fail\].*revert=0x.*InvalidCommand', log))
insufficient_balance = len(re.findall(r'\[sim-fail\].*revert=0x.*InsufficientBalance', log))
insufficient_profit = len(re.findall(r'\[sim-fail\].*revert=0x.*InsufficientProfit', log))
panic_reverts = len(re.findall(r'\[sim-fail\].*revert=0x4e487b71', log))

# Classify known categories
known_reverts = iia_reverts + currency_not_settled + invalid_command + insufficient_balance + insufficient_profit + panic_reverts
other_reverts = revert_total - known_reverts

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

# Revert decomposition: IIA (state divergence) vs encoding bugs vs unknown
stale_pct = f'{iia_reverts}' if revert_total > 0 else '0'
bug_pct = f'{other_reverts}' if revert_total > 0 else '0'
print(f'{total}\t{ok_total}\t{no_profit}\t{revert_total}\t{pct}\t{cls}\t{stale_pct}\t{bug_pct}')
")
    TOTAL=$(echo "$ANALYSIS" | cut -f1)
    OK_TOTAL=$(echo "$ANALYSIS" | cut -f2)
    NO_PROFIT=$(echo "$ANALYSIS" | cut -f3)
    REVERT_TOTAL=$(echo "$ANALYSIS" | cut -f4)
    PERCENT=$(echo "$ANALYSIS" | cut -f5)
    CLASS=$(echo "$ANALYSIS" | cut -f6)
    STALE=$(echo "$ANALYSIS" | cut -f7)
    BUG=$(echo "$ANALYSIS" | cut -f8)

    echo "=== [$i/27] $PERM: ok=$OK_TOTAL no_profit=$NO_PROFIT reverts=$REVERT_TOTAL (iia=$STALE other=$BUG) simulatable=$PERCENT $CLASS ==="

    # Update TSV under lock (prevent parallel workers corrupting the file)
    (
        flock -x 200
        sed -i "/\t${PERM}\t/d" "$RESULTS_FILE"
        echo -e "$i\t$PERM\t$TOTAL\t$OK_TOTAL\t$NO_PROFIT\t$REVERT_TOTAL\t$PERCENT\t$CLASS\t$STALE\t$BUG" >> "$RESULTS_FILE"
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