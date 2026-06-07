#!/usr/bin/env bash
# test_permutation.sh — Run the bot for a single permutation, capture logs for 15 minutes.
#
# Usage: ./logs/test_permutation.sh V2-V3-V4 [DURATION_MINUTES]
#
# Produces: logs/perm-V2-V3-V4.log

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
# Use pkill -f to kill both the uv wrapper and the child Python process
# (kill on the wrapper PID alone leaves the child orphaned).
EXISTING=$(pgrep -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true)
if [[ -n "$EXISTING" ]]; then
    echo "Killing existing process(es) for $PERM: $EXISTING"
    pkill -f "eth_backrun_v2_v3_v4_rust.py --permutation $PERM" 2>/dev/null || true
    sleep 3
    # Force-kill any survivors
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
if pct >= 80: cls = 'PASS'
elif pct >= 20: cls = 'PARTIAL'
else: cls = 'BROKEN'
print(f'{total}\t{ok_total}\t{fail_total}\t{pct}\t{cls}')
")
TOTAL=$(echo "$ANALYSIS" | cut -f1)
OK_TOTAL=$(echo "$ANALYSIS" | cut -f2)
FAIL_TOTAL=$(echo "$ANALYSIS" | cut -f3)
PERCENT=$(echo "$ANALYSIS" | cut -f4)
CLASS=$(echo "$ANALYSIS" | cut -f5)

echo "  Candidates: $TOTAL"
echo "  Simulatable: $OK_TOTAL"
echo "  Failed:      $FAIL_TOTAL"
echo "  Rate:        ${PERCENT}%"
if [[ "$CLASS" == "PASS" ]]; then echo "  Class:       ✅ Passing"; elif [[ "$CLASS" == "PARTIAL" ]]; then echo "  Class:       ⚠️ Partial"; else echo "  Class:       ❌ Broken"; fi

# Extract top failure modes from [sim-fail] lines
echo ""
echo "=== Top failure modes ==="
grep '\[sim-fail\]' "$LOGFILE" 2>/dev/null | \
    sed -E 's/.*revert=0x[0-9a-f]*( sel=0x[0-9a-f]+| [A-Z].*)?/\1/' | \
    sort | uniq -c | sort -rn | head -10

# Extract path counts from build_paths summary
echo ""
echo "=== Path building summary ==="
grep '\[build_paths\]' "$LOGFILE" 2>/dev/null | tail -5

echo ""
echo "Full log: $LOGFILE"
