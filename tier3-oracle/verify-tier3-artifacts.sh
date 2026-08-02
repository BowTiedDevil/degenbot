#!/usr/bin/env bash
# Validate that recompiling each Tier-3 harness from source produces EXACTLY the
# committed bytecode the Rust tests load from `artifacts/`. This is the
# authoritative compile-vs-use check the user asked for: it rebuilds every
# harness with the real solc/forge toolchain into a throwaway dir (PUBLISH=0,
# so the committed artifacts are never mutated) and byte-compares the creation
# `bytecode.object` against the committed artifact. Any mismatch means the
# checked-in bytecode is stale (a harness source or pinned vendored-lib edit
# without a rebuild) and must be re-published + re-committed.
#
# Complemented at `cargo test` time (toolchain-free) by
# `tier3_harness_artifacts.rs`, which hashes the tracked harness sources.
#
# Requires the toolchain (solc via svm + bootstrap-libs for the vendored
# reference gitignored libs). Wired into the CI `tier3-oracle` job.
set -euo pipefail
TD="$(cd "$(dirname "$0")" && pwd)"
cd "${TD}"

# Ensure reference sources + solc binaries are present (idempotent).
"${TD}/bootstrap-libs.sh"

SCRATCH="$(mktemp -d)"
trap 'rm -rf "${SCRATCH}"' EXIT

fail=0

# `creation_object <artifact-json>` – print the creation `bytecode.object` hex.
creation_object() {
    python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));print(d["bytecode"]["object"])' "$1"
}

# check_one <build-script> <artifact-entry> [<artifact-entry> ...]
check_one() {
    local script="$1"; shift
    echo "── recompiling via ${script}"
    OUT_DIR="${SCRATCH}" PUBLISH=0 "${TD}/${script}" >/dev/null
    for entry in "$@"; do
        local committed="artifacts/${entry}"
        local rebuilt="${SCRATCH}/${entry}"
        if [ ! -f "${committed}" ] || [ ! -f "${rebuilt}" ]; then
            echo "  ✗ ${entry}: missing (committed=${committed}, rebuilt=${rebuilt})"
            fail=1
            continue
        fi
        local a b
        a="$(creation_object "${committed}")"
        b="$(creation_object "${rebuilt}")"
        if [ "${a}" != "${b}" ]; then
            echo "  ✗ ${entry}: committed bytecode ≠ fresh compile"
            echo "    Rebuild + re-commit: run the harness build script (default PUBLISH=1) then commit artifacts/."
            fail=1
        else
            echo "  ✓ ${entry}: bytecode matches fresh compile"
        fi
    done
}

check_one build-tier3-v2-swap-harness.sh        V2SwapOracleHarness.sol/V2SwapOracleHarness.json
check_one build-tier3-v3-swap-harness.sh        V3SwapOracleHarness.sol/V3SwapOracleHarness.json
check_one build-tier3-v4-swap-harness.sh        V4SwapOracleHarness.sol/V4SwapOracleHarness.json
check_one build-tier3-harnesses.sh              SwapMathV3Harness.sol/SwapMathV3Harness.json \
                                                SwapMathV4Harness.sol/SwapMathV4Harness.json \
                                                Echo.sol/Echo.json
check_one build-tier3-curve-swap-harness.sh     CurveSwapOracleHarness.sol/CurveSwapOracleHarness.json
check_one build-tier3-balancer-swap-harness.sh  BalancerSwapOracleHarness.sol/BalancerSwapOracleHarness.json

if [ "${fail}" != "0" ]; then
    echo
    echo "ERROR: one or more committed tier-3 harness artifacts do not match a fresh compile."
    echo "Re-run the affected build-tier3-*-swap-harness.sh (PUBLISH=1), then commit the updated artifacts/."
    exit 1
fi

echo
echo "OK: all committed tier-3 harness artifacts match a fresh solc/forge compile."
