#!/usr/bin/env bash
# Build the Tier-3 V2 pair swap oracle harness (ergo task TLBUNW, epic
# UP5NH6). Compiles `src-v2/V2SwapOracleHarness.sol` (which imports the real
# v2-core `UniswapV2Pair`) directly with solc 0.5.16 — v2-core pragmas
# `=0.5.16` and the mock tokens must compile under the same 0.5.x (no
# `>=0.8` available). Foundry's solc manager cannot resolve solc <0.8 in
# this environment (see `build-tier3-harnesses.sh`), so we fetch the 0.5.16
# binary directly from binaries.soliditylang.org (cached under the svm dir).
# The standard-json output is reshaped into foundry's
# `out/<File>.sol/<Contract>.json` shape so the Rust loader reads it
# uniformly with the V3/V4 harnesses.
#
# Artifact: out/V2SwapOracleHarness.sol/V2SwapOracleHarness.json
set -euo pipefail

TD="$(cd "$(dirname "$0")" && pwd)"   # absolute tier3-oracle/
cd "${TD}"
OUT_DIR="${OUT_DIR:-${TD}/out}"
PUBLISH="${PUBLISH:-1}"

# Ensure the canonical v2-core reference source is present (idempotent;
# no-op if already cloned).
"${TD}/bootstrap-libs.sh"

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
  "sources": { "src-v2/V2SwapOracleHarness.sol": { "urls": ["src-v2/V2SwapOracleHarness.sol"] } },
  "settings": {
    "optimizer": { "enabled": true, "runs": 200 },
    "outputSelection": { "*": { "*": ["abi", "evm.bytecode.object", "evm.deployedBytecode.object"] } },
    "remappings": ["v2-core/=lib/v2-core/"]
  }
}
JSON

RAW="$(mktemp)"
"${SOLC_BIN}" --allow-paths . --standard-json < "${STD_JSON}" > "${RAW}"
rm -f "${STD_JSON}"

mkdir -p "${OUT_DIR}/V2SwapOracleHarness.sol"
python3 - "${RAW}" "${OUT_DIR}/V2SwapOracleHarness.sol/V2SwapOracleHarness.json" <<'PY'
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
inner = raw["contracts"]["src-v2/V2SwapOracleHarness.sol"]["V2SwapOracleHarness"]
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
echo "Tier-3 V2 pair swap oracle harness built:"
echo "  V2 (solc 0.5.16): ${TD}/out/V2SwapOracleHarness.sol/V2SwapOracleHarness.json"
