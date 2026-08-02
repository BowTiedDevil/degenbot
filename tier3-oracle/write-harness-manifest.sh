#!/usr/bin/env bash
# Recompute `tier3-oracle/artifacts/manifest.json`: maps each committed harness
# bytecode artifact to the sha256 of its git-tracked source .sol file. The
# Rust tier-3 tests read this manifest (via `tier3_harness_artifacts.rs`) at
# test time and reject any artifact whose tracked source has changed without
# a rebuild — so the checked-in bytecode can never silently drift from the
# harness sources. Sources only (the canonical vendored libs are version-pinned
# and immutable during the oracle's lifetime; the pinned-version bump case is
# covered by the CI tier3-oracle rebuild job).
set -euo pipefail
TD="$(cd "$(dirname "$0")" && pwd)"
cd "${TD}"

python3 - <<'PY'
import json, hashlib

# artifact (out shape) -> git-tracked source .sol (relative to tier3-oracle/).
MAP = {
    "V2SwapOracleHarness.sol/V2SwapOracleHarness.json": "src-v2/V2SwapOracleHarness.sol",
    "V3SwapOracleHarness.sol/V3SwapOracleHarness.json": "src-v3/V3SwapOracleHarness.sol",
    "V4SwapOracleHarness.sol/V4SwapOracleHarness.json": "src/V4SwapOracleHarness.sol",
    "SwapMathV3Harness.sol/SwapMathV3Harness.json": "src-v3/SwapMathV3Harness.sol",
    "SwapMathV4Harness.sol/SwapMathV4Harness.json": "src/SwapMathV4Harness.sol",
    "CurveSwapOracleHarness.sol/CurveSwapOracleHarness.json": "src-curve/CurveSwapOracleHarness.sol",
    "BalancerSwapOracleHarness.sol/BalancerSwapOracleHarness.json": "src-balancer/BalancerSwapOracleHarness.sol",
    "Echo.sol/Echo.json": "src/Echo.sol",
}

manifest = {}
for artifact, source in MAP.items():
    digest = hashlib.sha256(open(source, "rb").read()).hexdigest()
    manifest[artifact] = {"source": source, "sha256": digest}

with open("artifacts/manifest.json", "w", encoding="utf-8") as fh:
    json.dump(manifest, fh, indent=2, sort_keys=True)
    fh.write("\n")
print(f"wrote artifacts/manifest.json ({len(manifest)} artifacts)")
PY
