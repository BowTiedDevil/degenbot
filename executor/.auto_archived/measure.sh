#!/bin/bash
set -euo pipefail

# ── Autoresearch measure script ──
# Primary METRIC: total_gas (sum of cmd_executor gas across all 27 three-hop
# permutations, lower is better). Also reports per-path gas for instrumentation.

cd /home/ralph/code/executor

# Verify fake contract checksums
CHECKSUMS_FILE=".auto/baseline_checksums.json"

CHECKSUM_FAIL=0
python3 << 'PYEOF'
import json, hashlib, sys

with open("/home/ralph/code/executor/.auto/baseline_checksums.json") as f:
    baseline = json.load(f)

with open("/home/ralph/code/executor/.build/__local__.json") as f:
    build = json.load(f)

types = build.get("contractTypes", {})
failures = []
for name, expected_sha in sorted(baseline.items()):
    info = types.get(name)
    if info is None:
        failures.append(f"  {name}: NOT FOUND in build output")
        continue
    rbc_hex = info.get("runtimeBytecode", {}).get("bytecode", "")
    if not rbc_hex or not rbc_hex.startswith("0x"):
        failures.append(f"  {name}: no runtime bytecode")
        continue
    rbc_bytes = bytes.fromhex(rbc_hex[2:])
    actual_sha = hashlib.sha256(rbc_bytes).hexdigest()
    if actual_sha != expected_sha:
        failures.append(f"  {name}: MISMATCH  expected={expected_sha[:16]}...  actual={actual_sha[:16]}...")

if failures:
    print("CHECKSUM FAILURES:")
    for f in failures:
        print(f)
    sys.exit(1)
else:
    print(f"CHECKSUM OK: {len(baseline)} fake contracts verified")
PYEOF

# ── Step 3: Run the 27 three-hop permutation gas benchmarks ──
# Tests write results to .gas-results (one line per test: GAS <label> <gas>).
# The file is cleared at session start by conftest.py.
uv run ape test tests/test_cmd_executor_three_hop_optimized.py -v -m "" 2>&1 | tail -5

# ── Step 4: Extract metrics from .gas-results ──
RESULTS_FILE=".gas-results"

if [ ! -f "$RESULTS_FILE" ]; then
    echo "ERROR: $RESULTS_FILE not found — tests may not have run"
    exit 1
fi

LINE_COUNT=$(wc -l < "$RESULTS_FILE")
if [ "$LINE_COUNT" -ne 27 ]; then
    echo "WARNING: expected 27 gas results, got $LINE_COUNT"
fi

# Sum all gas values → total_gas (primary metric)
TOTAL_GAS=$(awk '{sum += $3} END {print sum}' "$RESULTS_FILE")

# Emit per-path metrics for instrumentation
# Labels follow the pattern TestV2V2V2, TestV4V3V4, etc.
while IFS=' ' read -r _ label gas; do
    # Convert TestV4V4V4 → v4_v4_v4
    KEY=$(echo "$label" | sed 's/^Test//; s/\([A-Z]\)/_\l\1/g; s/^_//')
    echo "METRIC ${KEY}=${gas}"
done < "$RESULTS_FILE"

echo ""
echo "METRIC total_gas=${TOTAL_GAS:-0}"
echo "METRIC bytecode_size=$(python3 -c "
import json
data = json.load(open('/home/ralph/code/executor/.build/__local__.json'))
rbc = data.get('contractTypes', {}).get('cmd_executor', {}).get('runtimeBytecode', {}).get('bytecode', '')
if rbc and rbc.startswith('0x'):
    print(len(bytes.fromhex(rbc[2:])))
else:
    print(0)
")"
