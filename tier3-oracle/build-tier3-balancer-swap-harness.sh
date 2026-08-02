#!/usr/bin/env bash
# Build the Tier-3 Balancer weighted/stable swap oracle harness (ergo task
# EZLECC, epic UP5NH6 — family 3/3 of SH6HAK's Tier-3 cutover). Compiles
# `src-balancer/BalancerSwapOracleHarness.sol` directly with solc 0.7.6
# (all canonical Balancer libs are `pragma ^0.7.0`). The harness imports the
# canonical balancer-v2-monorepo math cores (FixedPoint/LogExpMath/
# WeightedMath/StableMath) vendored under `lib/balancer-src/` (pinned commit
# f8b6f44), fetched by `bootstrap-libs.sh`. solc 0.7.6 is cached in the svm
# dir (used by the V3 harness); fetched from binaries.soliditylang.org if
# absent. The standard-json output is reshaped into foundry's
# `out/<File>.sol/<Contract>.json` shape so the Rust loader reads it
# uniformly with the V2/V3/V4/Curve harnesses.
#
# Artifact: out/BalancerSwapOracleHarness.sol/BalancerSwapOracleHarness.json
set -euo pipefail

TD="$(cd "$(dirname "$0")" && pwd)"   # absolute tier3-oracle/
cd "${TD}"
OUT_DIR="${OUT_DIR:-${TD}/out}"
PUBLISH="${PUBLISH:-1}"

# Ensure the canonical Balancer reference sources are present (idempotent).
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
  "sources": { "src-balancer/BalancerSwapOracleHarness.sol": { "urls": ["src-balancer/BalancerSwapOracleHarness.sol"] } },
  "settings": {
    "optimizer": { "enabled": true, "runs": 200 },
    "outputSelection": { "*": { "*": ["abi", "evm.bytecode.object", "evm.deployedBytecode.object"] } },
    "remappings": [
      "@balancer-labs/v2-solidity-utils/=lib/balancer-src/solidity-utils/",
      "@balancer-labs/v2-interfaces/=lib/balancer-src/interfaces/"
    ]
  }
}
JSON

RAW="$(mktemp)"
"${SOLC_BIN}" --base-path . --allow-paths . --standard-json < "${STD_JSON}" > "${RAW}"
rm -f "${STD_JSON}"

# Reshape standard-json into foundry's single-contract out/ shape.
mkdir -p "${OUT_DIR}/BalancerSwapOracleHarness.sol"
python3 - "${RAW}" "${OUT_DIR}/BalancerSwapOracleHarness.sol/BalancerSwapOracleHarness.json" <<'PY'
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
inner = raw["contracts"]["src-balancer/BalancerSwapOracleHarness.sol"]["BalancerSwapOracleHarness"]
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
echo "Tier-3 Balancer weighted/stable swap oracle harness built:"
echo "  Balancer (solc 0.7.6): ${TD}/out/BalancerSwapOracleHarness.sol/BalancerSwapOracleHarness.json"
