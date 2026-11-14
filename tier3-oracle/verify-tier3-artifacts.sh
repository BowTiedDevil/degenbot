#!/usr/bin/env bash
# Validate that recompiling every Tier-3 harness from source reproduces EXACTLY
# the committed artifacts the Rust tests load from `artifacts/`. This is the
# authoritative compile-vs-use check: each unique build script named by
# `harness-artifacts.tsv` runs ONCE into a throwaway OUT_DIR (PUBLISH=0, so the
# committed artifacts and manifest are never mutated) and every artifact row that
# script owns is byte-compared against its committed artifact — creation code AND
# deployed runtime. Any mismatch means the checked-in bytecode is stale (a
# harness source or pinned vendored-lib edit without a rebuild) and must be
# republished and re-committed.
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

MAP_TOOL="${TD}/harness-artifacts.py"
REBUILD="just rebuild-tier3-artifacts (oracle) / just rebuild-tier3-executor-stubs (executor)"

fail=0

# One fresh build per unique build script, then a byte-compare of exactly the
# rows that script owns — the map is the only source of that partition, so a new
# harness is verified the moment it is added there.
build_scripts="$(python3 "${MAP_TOOL}" scripts)"
while IFS= read -r script; do
    echo "── recompiling via ${script}"
    if ! OUT_DIR="${SCRATCH}" PUBLISH=0 "${TD}/${script}" >/dev/null; then
        echo "  ✗ ${script}: fresh compile failed"
        fail=1
        continue
    fi
    if ! python3 "${MAP_TOOL}" verify --out-dir "${SCRATCH}" --script "${script}"; then
        fail=1
    fi
done <<< "${build_scripts}"

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
        echo "    Re-run build-tier3-pancake-v2-swap-harness.sh / refresh artifacts/PancakeV2Pair/ then commit."
        fail=1
    else
        echo "  ✓ PancakeV2 pair creation code matches a fresh compile at pinned settings (keccak → 0x57224589… asserted in Rust)"
    fi
}
check_pancake2_initcode

if [ "${fail}" != "0" ]; then
    echo
    echo "ERROR: one or more committed tier-3 artifact entries do not match a fresh compile."
    echo "Republish with PUBLISH=1 (${REBUILD}), then commit the refreshed tier3-oracle/artifacts/."
    exit 1
fi

echo
echo "OK: every committed tier-3 artifact entry matches a fresh solc/forge compile (creation + deployed runtime)."
