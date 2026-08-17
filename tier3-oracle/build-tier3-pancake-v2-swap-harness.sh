#!/usr/bin/env bash
# Build the Tier-3 PancakeSwap V2 pair swap oracle harness. Compiles
# `src-pancake-v2/PancakeV2SwapOracleHarness.sol` with solc 0.5.16 (the mock
# tokens + harness shell are `pragma =0.5.16`). The harness does NOT compile a
# PancakePair: it deploys the PINNED on-chain creation bytecode (committed
# under `artifacts/PancakeV2Pair/PancakeV2Pair.json`, Sourcify-verified
# `exact_match` against the live mainnet pair) via a raw EVM `create` passed as
# a constructor arg — so no vendored source, remapping, or lib fetch is needed.
# The standard-json output is reshaped into foundry's
# `out/<File>.sol/<Contract>.json` shape so the Rust loader reads it uniformly
# with the other harnesses.
#
# Artifact: out/PancakeV2SwapOracleHarness.sol/PancakeV2SwapOracleHarness.json
set -euo pipefail

TD="$(cd "$(dirname "$0")" && pwd)"   # absolute tier3-oracle/
cd "${TD}"
OUT_DIR="${OUT_DIR:-${TD}/out}"
PUBLISH="${PUBLISH:-1}"

SOLC_VER="0.5.16"
SOLC_LONG="0.5.16+commit.9c3226ce"
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
cat > "${STD_JSON}" <<'JSON'
{
  "language": "Solidity",
  "sources": { "src-pancake-v2/PancakeV2SwapOracleHarness.sol": { "urls": ["src-pancake-v2/PancakeV2SwapOracleHarness.sol"] } },
  "settings": {
    "optimizer": { "enabled": true, "runs": 200 },
    "outputSelection": { "*": { "*": ["abi", "evm.bytecode.object", "evm.deployedBytecode.object"] } },
    "remappings": []
  }
}
JSON

RAW="$(mktemp)"
"${SOLC_BIN}" --allow-paths . --standard-json < "${STD_JSON}" > "${RAW}"
rm -f "${STD_JSON}"

mkdir -p "${OUT_DIR}/PancakeV2SwapOracleHarness.sol"
python3 - "${RAW}" "${OUT_DIR}/PancakeV2SwapOracleHarness.sol/PancakeV2SwapOracleHarness.json" <<'PY'
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
inner = raw["contracts"]["src-pancake-v2/PancakeV2SwapOracleHarness.sol"]["PancakeV2SwapOracleHarness"]
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
echo "Tier-3 PancakeSwap V2 pair swap oracle harness built (pins on-chain pair bytecode):"
echo "  PancakeV2 (solc 0.5.16): ${TD}/out/PancakeV2SwapOracleHarness.sol/PancakeV2SwapOracleHarness.json"
echo "  pinned pair: ${TD}/artifacts/PancakeV2Pair/PancakeV2Pair.json"
