#!/usr/bin/env bash
# Build the Tier-3 Curve stableswap `get_dy` oracle harness (ergo task
# YXMNWB, epic UP5NH6 — family 2/3 of SH6HAK's Tier-3 cutover). Compiles
# `src-curve/CurveSwapOracleHarness.sol` directly with solc 0.8.26 — the
# harness is a standalone faithful Solidity port of the STANDARD stableswap
# `get_dy` (Curve's canonical source is Vyper; no vyper toolchain here, so
# the algorithm itself is the on-chain reference, see the .sol header). No
# lib/ remapping needed (no external imports). The svm 0.8.26 binary is
# cached by the V4/forge builds; fetched from binaries.soliditylang.org if
# absent. The standard-json output is reshaped into foundry's
# `out/<File>.sol/<Contract>.json` shape so the Rust loader reads it
# uniformly with the V2/V3/V4 harnesses.
#
# Artifact: out/CurveSwapOracleHarness.sol/CurveSwapOracleHarness.json
set -euo pipefail

TD="$(cd "$(dirname "$0")" && pwd)"   # absolute tier3-oracle/
cd "${TD}"
OUT_DIR="${OUT_DIR:-${TD}/out}"
PUBLISH="${PUBLISH:-1}"

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
cat > "${STD_JSON}" <<'JSON'
{
  "language": "Solidity",
  "sources": { "src-curve/CurveSwapOracleHarness.sol": { "urls": ["src-curve/CurveSwapOracleHarness.sol"] } },
  "settings": {
    "optimizer": { "enabled": true, "runs": 200 },
    "outputSelection": { "*": { "*": ["abi", "evm.bytecode.object", "evm.deployedBytecode.object"] } }
  }
}
JSON

RAW="$(mktemp)"
"${SOLC_BIN}" --base-path . --allow-paths . --standard-json < "${STD_JSON}" > "${RAW}"
rm -f "${STD_JSON}"

mkdir -p "${OUT_DIR}/CurveSwapOracleHarness.sol"
python3 - "${RAW}" "${OUT_DIR}/CurveSwapOracleHarness.sol/CurveSwapOracleHarness.json" <<'PY'
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
inner = raw["contracts"]["src-curve/CurveSwapOracleHarness.sol"]["CurveSwapOracleHarness"]
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
echo "Tier-3 Curve stableswap get_dy oracle harness built:"
echo "  Curve (solc 0.8.26): ${TD}/out/CurveSwapOracleHarness.sol/CurveSwapOracleHarness.json"
