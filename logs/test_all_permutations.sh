#!/usr/bin/env bash
# test_all_permutations.sh — Run the bot for all 27 three-hop permutations sequentially.
#
# Each permutation runs for 15 minutes (configurable via TEST_DURATION env var).
# Results are appended to logs/permutation_results.tsv
#
# Usage: ./logs/test_all_permutations.sh [START_INDEX]
#
# START_INDEX: 1-27, to resume from a specific permutation (default: 1)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RESULTS_FILE="${SCRIPT_DIR}/permutation_results.tsv"
DURATION_MINUTES="${TEST_DURATION:-15}"

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

# Write TSV header if file is new
if [[ ! -f "$RESULTS_FILE" ]] || [[ ! -s "$RESULTS_FILE" ]]; then
    echo -e "#\tPermutation\tCandidates\tSimulatable\tFailed\tRate\tClassification" > "$RESULTS_FILE"
    echo "Created results file: $RESULTS_FILE"
fi

echo "=== Testing all 27 permutations (${DURATION_MINUTES} min each) ==="
echo "Starting from index $START_INDEX"
echo ""

for i in $(seq "$START_INDEX" 27); do
    PERM="${PERMUTATIONS[$((i - 1))]}"
    echo ""
    echo "=== [$i/27] $PERM ==="

    LOGFILE="${SCRIPT_DIR}/perm-${PERM}.log"

    # Kill any existing run for this permutation.
    # Use pkill -f to kill both the uv wrapper and the child Python process.
    EXISTING=$(pgrep -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true)
    if [[ -n "$EXISTING" ]]; then
        echo "Killing existing process(es) for $PERM: $EXISTING"
        pkill -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true
        sleep 3
        pkill -9 -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true
    fi

    # Start the bot
    cd /home/ralph/code/degenbot
    nohup uv run python examples/eth_backrun_v2_v3_v4_rust.py \
        --permutation "$PERM" \
        > "$LOGFILE" 2>&1 &

    # Wait
    echo "Waiting ${DURATION_MINUTES} minutes..."
    sleep "$((DURATION_MINUTES * 60))"

    # Stop the bot — pkill -f catches both uv wrapper and Python child
    echo "Stopping bot for $PERM..."
    pkill -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true
    sleep 3
    pkill -9 -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true

    # Analyze results
    ANALYSIS=$(cd /home/ralph/code/degenbot && uv run python3 -c "
import re, sys
log = open('$LOGFILE').read()
ok_total = sum(int(m.group(1)) for m in re.finditer(r'(\d+) ok \(', log))
fail_total = sum(int(m.group(1)) for m in re.finditer(r'(\d+) failed', log))
total = ok_total + fail_total
pct = ok_total * 100 // total if total > 0 else 0
if pct >= 80: cls = '✅ Passing'
elif pct >= 20: cls = '⚠️ Partial'
else: cls = '❌ Broken'
print(f'{total}\t{ok_total}\t{fail_total}\t{pct}%\t{cls}')
")
    TOTAL=$(echo "$ANALYSIS" | cut -f1)
    OK_TOTAL=$(echo "$ANALYSIS" | cut -f2)
    FAIL_TOTAL=$(echo "$ANALYSIS" | cut -f3)
    PERCENT=$(echo "$ANALYSIS" | cut -f4)
    CLASS=$(echo "$ANALYSIS" | cut -f5)

    echo "$PERM: $OK_TOTAL/$TOTAL simulatable ($PERCENT) $CLASS"
    echo -e "$i\t$PERM\t$TOTAL\t$OK_TOTAL\t$FAIL_TOTAL\t$PERCENT\t$CLASS" >> "$RESULTS_FILE"

    # Brief pause between permutations
    sleep 10
done

echo ""
echo "=== All permutations complete ==="
echo "Results: $RESULTS_FILE"
cat "$RESULTS_FILE"
