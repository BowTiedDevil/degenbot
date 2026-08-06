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
check_one build-tier3-pancake-swap-harness.sh PancakeV3SwapOracleHarness.sol/PancakeV3SwapOracleHarness.json
check_one build-tier3-pancake2-swap-harness.sh PancakeV2SwapOracleHarness.sol/PancakeV2SwapOracleHarness.json

# The PINNED PancakeSwap V2 pair (artifacts/PancakeV2Pair/): recompile the
# committed source at the pinned settings and assert the creation code is
# byte-identical to the pinned artifact. The toolchain-free Rust test
# (tier3_pancake_v2_initcode.rs) then keccaks that committed creation code and
# asserts it equals the canonical INIT_CODE_PAIR_HASH 0x57224589… — so this
# byte-compare + that keccak together close the source→init-code-hash loop.
check_pancake2_initcode() {
    local solc="${HOME}/.local/share/svm/0.5.16/solc-0.5.16"
    if [ ! -x "${solc}" ]; then
        echo "  ✗ PancakeV2 init code: solc 0.5.16 not present (run the pancake2 harness build first)"
        fail=1
        return
    fi
    local src="artifacts/PancakeV2Pair/sources/contracts/PancakeFactory.sol"
    local pinned="artifacts/PancakeV2Pair/PancakeV2Pair.json"
    # Standard-json at the Sourcify-pinned settings: evmVersion istanbul,
    # optimizer runs 99999 (the exact settings whose metadata match a CLI
    # recompile cannot reproduce — see the Rust test doc).
    local stdin_json; stdin_json="$(mktemp)"
    cat > "${stdin_json}" <<JSON
{
  "language": "Solidity",
  "sources": { "contracts/PancakeFactory.sol": { "urls": ["${src}"] } },
  "settings": {
    "evmVersion": "istanbul",
    "libraries": {},
    "optimizer": { "enabled": true, "runs": 99999 },
    "remappings": [],
    "outputSelection": { "*": { "*": ["evm.bytecode.object"] } }
  }
}
JSON
    local out; out="$(mktemp)"
    ( cd "${TD}" && "${solc}" --allow-paths . --standard-json < "${stdin_json}" ) > "${out}" 2>/dev/null
    rm -f "${stdin_json}"
    local recompiled committed a b
    recompiled=$(python3 -c 'import json,sys;r=json.load(open(sys.argv[1]));print(r["contracts"]["contracts/PancakeFactory.sol"]["PancakePair"]["evm"]["bytecode"]["object"])' "${out}" 2>/dev/null || echo "") || true
    committed=$(python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));print(d["bytecode"]["object"])' "${pinned}")
    rm -f "${out}"
    if [ -z "${recompiled}" ]; then
        echo "  ✗ PancakeV2 init code: recompile failed (solc 0.5.16 standard-json)"
        fail=1
        return
    fi
    # Normalize the 0x prefix + case: solc's evm.bytecode.object has no 0x;
    # the committed artifact preserves Sourcify's 0x… form.
    local a b
    a=$(python3 -c "import sys;s=sys.argv[1].lower();print(s[2:] if s.startswith('0x') else s)" "${recompiled}")
    b=$(python3 -c "import sys;s=sys.argv[1].lower();print(s[2:] if s.startswith('0x') else s)" "${committed}")
    if [ "${a}" != "${b}" ]; then
        echo "  ✗ PancakeV2 pair creation code ≠ fresh compile at pinned settings"
        echo "    Re-run build-tier3-pancake2-swap-harness.sh / refresh artifacts/PancakeV2Pair/ then commit."
        fail=1
    else
        echo "  ✓ PancakeV2 pair creation code matches a fresh compile at pinned settings (keccak → 0x57224589… asserted in Rust)"
    fi
}
check_pancake2_initcode

if [ "${fail}" != "0" ]; then
    echo
    echo "ERROR: one or more committed tier-3 harness artifacts do not match a fresh compile."
    echo "Re-run the affected build-tier3-*-swap-harness.sh (PUBLISH=1), then commit the updated artifacts/."
    exit 1
fi

echo
echo "OK: all committed tier-3 harness artifacts match a fresh solc/forge compile."
