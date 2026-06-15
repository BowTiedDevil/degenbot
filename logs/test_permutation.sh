#!/usr/bin/env bash
# test_permutation.sh — Run the bot for a single permutation, capture logs for 15 minutes.
#
# Usage: ./logs/test_permutation.sh V2-V3-V4 [DURATION_MINUTES]
#
# Produces: logs/perm-V2-V3-V4.log
#
# Simulation outcomes are classified into three categories:
#   ok        — simulation completed without revert (subdivided: profitable / below_threshold)
#   no_profit — simulation completed (no revert), but gross profit ≤ 0
#   revert    — simulation reverted on-chain (encoding bug or token transfer issue)
#
# The simulatability rate = (ok + no_profit) / (ok + no_profit + revert).
# A "no_profit" path is not broken — the encoding works, the arb just isn't profitable.

set -euo pipefail

PERM="${1:?Usage: $0 V2-V3-V4 [DURATION_MINUTES]}"
LOGDIR="$(cd "$(dirname "$0")" && pwd)"
LOGFILE="${LOGDIR}/perm-${PERM}.log"
DURATION_MINUTES="${2:-15}"
DURATION_SECONDS=$((DURATION_MINUTES * 60))

# Validate permutation format
if [[ ! "$PERM" =~ ^V[234]-V[234]-V[234]$ ]]; then
    echo "Invalid permutation format: $PERM (expected e.g. V2-V3-V4)"
    exit 1
fi

# Kill any existing run for this permutation.
EXISTING=$(pgrep -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true)
if [[ -n "$EXISTING" ]]; then
    echo "Killing existing process(es) for $PERM: $EXISTING"
    pkill -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true
    sleep 3
    pkill -9 -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true
fi

echo "=== Starting permutation $PERM ==="
echo "Log: $LOGFILE"
echo "Duration: ${DURATION_MINUTES} minutes"

cd /home/ralph/code/degenbot

# Start the bot in the background
nohup uv run python examples/eth_backrun_v2_v3_v4_rust.py \
    --permutation "$PERM" \
    > "$LOGFILE" 2>&1 &

# Wait for the specified duration
echo "Waiting ${DURATION_MINUTES} minutes..."
sleep "$DURATION_SECONDS"

# Stop the bot
echo "Stopping bot for $PERM..."
pkill -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true
sleep 3
pkill -9 -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true

# ── Analyze results ──────────────────────────────────────────
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
    print(f'0\t0\t0\t0\t—\tNO_OPPORTUNITY')
else:
    sim_ok = ok_total + no_profit  # encoding works (just may be unprofitable)
    pct = sim_ok * 100 // total
    if pct >= 80: cls = 'PASS'
    elif pct >= 20: cls = 'PARTIAL'
    else: cls = 'BROKEN'
    print(f'{total}\t{ok_total}\t{no_profit}\t{revert_total}\t{pct}%\t{cls}')
")
TOTAL=$(echo "$ANALYSIS" | cut -f1)
OK_TOTAL=$(echo "$ANALYSIS" | cut -f2)
NO_PROFIT=$(echo "$ANALYSIS" | cut -f3)
REVERT_TOTAL=$(echo "$ANALYSIS" | cut -f4)
PERCENT=$(echo "$ANALYSIS" | cut -f5)
CLASS=$(echo "$ANALYSIS" | cut -f6)

echo "  Total candidates: $TOTAL"
echo "  Sim-OK (profitable + below-threshold): $OK_TOTAL"
echo "  No-profit (encoding works, arb not profitable): $NO_PROFIT"
echo "  Reverts (encoding/state error):          $REVERT_TOTAL"
if [[ "$CLASS" == "NO_OPPORTUNITY" ]]; then
    echo "  Simulatability: —"
    echo "  Class:          ⬜ No Opportunity"
else
    echo "  Simulatability: ${PERCENT}"
    if [[ "$CLASS" == "PASS" ]]; then echo "  Class:          ✅ Passing"
    elif [[ "$CLASS" == "PARTIAL" ]]; then echo "  Class:          ⚠️ Partial"
    else echo "  Class:          ❌ Broken"; fi
fi

# Extract top revert reasons (with decoded error names)
if [[ "$REVERT_TOTAL" -gt 0 ]]; then
    echo ""
    echo "=== Top revert reasons ==="
    grep '\[sim-fail\]' "$LOGFILE" 2>/dev/null | grep 'revert=' | \
        sed -E 's/.*revert=0x[0-9a-f]*/revert=0x/' | \
        sed -E 's/.*(InsufficientProfit|InsufficientBalance|InvalidCallback|Unauthorized|InvalidCommand|BipsTooHigh|InvalidMsgValue|NotPlainEthTransfer|CurrencyNotSettled|PoolNotInitialized|IIA|!OWNER).*/\1/' | \
        sort | uniq -c | sort -rn | head -10
fi

# Extract path counts from build_paths summary
echo ""
echo "=== Path building summary ==="
grep '\[build_paths\]' "$LOGFILE" 2>/dev/null | tail -5

echo ""
echo "Full log: $LOGFILE"
