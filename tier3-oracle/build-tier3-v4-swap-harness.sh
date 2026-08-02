#!/usr/bin/env bash
# Build the Tier-3b V4 `PoolManager.swap` end-to-end oracle harness (ergo
# task 2LTKVO, epic UP5NH6). Compiles `src/V4SwapOracleHarness.sol` (which
# imports the real v4-core `PoolManager` as canonical bytecode) with solc
# 0.8.26 via standard-json, mirroring the direct-solc approach the V3/V2
# oracle harnesses use (foundry cannot resolve the sibling v3-core's solc
# <0.8 here, and a full `forge build` would wipe the V2/V3 swap-oracle
# artifacts those scripts write into `out/`). Using direct solc keeps this
# script independent of `out/` state and idempotent.
#
# Artifact: out/V4SwapOracleHarness.sol/V4SwapOracleHarness.json
set -euo pipefail

TD="$(cd "$(dirname "$0")" && pwd)"   # absolute tier3-oracle/
cd "${TD}"
OUT_DIR="${OUT_DIR:-${TD}/out}"
PUBLISH="${PUBLISH:-1}"

# Ensure the canonical v4-core reference source + its libs are present
# (idempotent; no-op if already cloned).
"${TD}/bootstrap-libs.sh"
# v4-core's PoolManager pulls solmate's `Owned.sol`; ensure it's vendored.
if [ ! -f lib/v4-core/lib/solmate/src/auth/Owned.sol ]; then
    git clone --depth 1 https://github.com/transmissions11/solmate lib/v4-core/lib/solmate
fi

SOLC_VER="0.8.26"
SOLC_LONG="0.8.26+commit.8a97fa7a"
SVM_DIR="${HOME}/.local/share/svm/${SOLC_VER}"
SOLC_BIN="${SVM_DIR}/solc-${SOLC_VER}"

if [ ! -x "${SOLC_BIN}" ]; then
    echo "fetching solc ${SOLC_VER} (not in svm cache)…"
    mkdir -p "${SVM_DIR}"
    curl -fsSL -o "${SOLC_BIN}" \
        "https://binaries.soliditylang.org/linux-amd64/solc-linux-amd64-v${SOLC_LONG}"
    chmod +x "${SOLC_BIN}"
fi

STD_JSON="$(mktemp)"
cat > "${STD_JSON}" <<JSON
{
  "language": "Solidity",
  "sources": { "src/V4SwapOracleHarness.sol": { "urls": ["src/V4SwapOracleHarness.sol"] } },
  "settings": {
    "optimizer": { "enabled": true, "runs": 800 },
    "outputSelection": { "*": { "*": ["abi", "evm.bytecode.object", "evm.deployedBytecode.object"] } },
    "remappings": [
      "v4-core/=lib/v4-core/",
      "solmate/=lib/v4-core/lib/solmate/",
      "@openzeppelin/=lib/v4-core/lib/openzeppelin-contracts/",
      "forge-std/=lib/v4-core/lib/forge-std/src/"
    ]
  }
}
JSON

RAW="$(mktemp)"
"${SOLC_BIN}" --base-path . --allow-paths . --standard-json < "${STD_JSON}" > "${RAW}"
rm -f "${STD_JSON}"

mkdir -p "${OUT_DIR}/V4SwapOracleHarness.sol"
python3 - "${RAW}" "${OUT_DIR}/V4SwapOracleHarness.sol/V4SwapOracleHarness.json" <<'PY'
import json, sys
raw = json.load(open(sys.argv[1]))
errs = [e for e in raw.get("errors", []) if e.get("severity") == "error"]
if errs:
    for e in errs:
        print(f"solc error: {e['formattedMessage']}", file=sys.stderr)
    sys.exit(1)
for e in raw.get("errors", []):
    if e.get("severity") == "warning":
        print(f"solc warning: {e.get('formattedMessage','')}", file=sys.stderr)
raw_contracts = raw["contracts"]
# find the contract data regardless of exact source name
inner = next(
    c
    for v in raw_contracts.values()
    for k, c in v.items()
    if k == "V4SwapOracleHarness"
)
shaped = {
    "abi": inner["abi"],
    "bytecode": {"object": inner["evm"]["bytecode"]["object"]},
    "deployedBytecode": {"object": inner["evm"]["deployedBytecode"]["object"]},
}
json.dump(shaped, open(sys.argv[2], "w"))
print(f"wrote {sys.argv[2]}")
PY
rm -f "${RAW}"


# Publish committed bytecode + drift manifest (toolchain-free `cargo test` path).
if [ "${PUBLISH}" != "0" ]; then
  "${TD}/publish-artifacts.sh"
fi
echo "Tier-3b V4 swap oracle harness built:"
echo "  V4 (solc 0.8.26): ${TD}/out/V4SwapOracleHarness.sol/V4SwapOracleHarness.json"
