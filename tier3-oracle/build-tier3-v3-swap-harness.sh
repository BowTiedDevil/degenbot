#!/usr/bin/env bash
# Build the Tier-3b V3 Pool.swap end-to-end oracle harness (ergo task 2LTKVO,
# epic UP5NH6). Compiles `src-v3/V3SwapOracleHarness.sol` (which imports the
# real v3-core `UniswapV3Pool`) directly with solc 0.7.6 — foundry's solc
# manager cannot resolve solc <0.8 in this environment (see the Tier-3a build
# note in `build-tier3-harnesses.sh`). The standard-json output is reshaped
# into foundry's `out/<File>.sol/<Contract>.json` shape so the Rust loader
# (`tier3_compute_swap_step_vs_revm.rs` loader pattern) reads it uniformly.
#
# Artifact: out/V3SwapOracleHarness.sol/V3SwapOracleHarness.json
set -euo pipefail

TD="$(cd "$(dirname "$0")" && pwd)"   # absolute tier3-oracle/
cd "${TD}"

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
  "sources": { "src-v3/V3SwapOracleHarness.sol": { "urls": ["src-v3/V3SwapOracleHarness.sol"] } },
  "settings": {
    "optimizer": { "enabled": true, "runs": 800 },
    "outputSelection": { "*": { "*": ["abi", "evm.bytecode.object", "evm.deployedBytecode.object"] } },
    "remappings": ["v3-core/=lib/v3-core/"]
  }
}
JSON

RAW="$(mktemp)"
"${SOLC_BIN}" --base-path . --allow-paths . --standard-json < "${STD_JSON}" > "${RAW}"
rm -f "${STD_JSON}"

mkdir -p "${TD}/out/V3SwapOracleHarness.sol"
python3 - "${RAW}" "${TD}/out/V3SwapOracleHarness.sol/V3SwapOracleHarness.json" <<'PY'
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
inner = raw["contracts"]["src-v3/V3SwapOracleHarness.sol"]["V3SwapOracleHarness"]
shaped = {
    "abi": inner["abi"],
    "bytecode": {"object": inner["evm"]["bytecode"]["object"]},
    "deployedBytecode": {"object": inner["evm"]["deployedBytecode"]["object"]},
}
json.dump(shaped, open(sys.argv[2], "w"))
print(f"wrote {sys.argv[2]}")
PY
rm -f "${RAW}"

echo "Tier-3b V3 swap oracle harness built:"
echo "  V3 (solc 0.7.6): ${TD}/out/V3SwapOracleHarness.sol/V3SwapOracleHarness.json"
