#!/usr/bin/env bash
# Build the Tier-3 PancakeSwap V3 `PancakeV3Pool.swap` oracle harness (task:
# PancakeSwap V3 variant harness). Compiles
# `src-pancake-v3/PancakeV3SwapOracleHarness.sol` (which imports the REAL
# `PancakeV3Pool` — the Etherscan-verified deployed source, solc 0.7.6,
# vendored under `lib/pancake-src/`) directly with solc 0.7.6 (same toolchain
# as the Uniswap V3 harness — the PancakeSwap fork is also `pragma 0.7.6`).
# The standard-json output is reshaped into foundry's
# `out/<File>.sol/<Contract>.json` shape so the Rust loader reads it uniformly.
#
# Remappings: `pancake-v3-core/=lib/pancake-src/` (the pool + interfaces +
# libraries, preserved from the Etherscan verified build) and
# `@pancakeswap/=lib/pancake-src/` (the single `v3-lm-pool` import resolves to
# `lib/pancake-src/v3-lm-pool/...`).
#
# Artifact: out/PancakeV3SwapOracleHarness.sol/PancakeV3SwapOracleHarness.json
set -euo pipefail

TD="$(cd "$(dirname "$0")" && pwd)"   # absolute tier3-oracle/
cd "${TD}"
OUT_DIR="${OUT_DIR:-${TD}/out}"
PUBLISH="${PUBLISH:-1}"

# Ensure the Etherscan-sourced PancakeSwap V3 reference source is present
# (idempotent; no-op if already vendored).
"${TD}/bootstrap-libs.sh"

SOLC_VER="0.7.6"
SOLC_LONG="0.7.6+commit.7338295f"
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
  "sources": { "src-pancake-v3/PancakeV3SwapOracleHarness.sol": { "urls": ["src-pancake-v3/PancakeV3SwapOracleHarness.sol"] } },
  "settings": {
    "optimizer": { "enabled": true, "runs": 1 },
    "outputSelection": { "*": { "*": ["abi", "evm.bytecode.object", "evm.deployedBytecode.object"] } },
    "remappings": [
      "pancake-v3-core/=lib/pancake-src/",
      "@pancakeswap/=lib/pancake-src/"
    ]
  }
}
JSON

RAW="$(mktemp)"
"${SOLC_BIN}" --base-path . --allow-paths . --standard-json < "${STD_JSON}" > "${RAW}"
rm -f "${STD_JSON}"

mkdir -p "${OUT_DIR}/PancakeV3SwapOracleHarness.sol"
python3 - "${RAW}" "${OUT_DIR}/PancakeV3SwapOracleHarness.sol/PancakeV3SwapOracleHarness.json" <<'PY'
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
inner = raw["contracts"]["src-pancake-v3/PancakeV3SwapOracleHarness.sol"]["PancakeV3SwapOracleHarness"]
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
echo "Tier-3 PancakeSwap V3 swap oracle harness built:"
echo "  PancakeV3 (solc 0.7.6): ${TD}/out/PancakeV3SwapOracleHarness.sol/PancakeV3SwapOracleHarness.json"
