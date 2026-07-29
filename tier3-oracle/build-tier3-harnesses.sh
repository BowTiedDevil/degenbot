#!/usr/bin/env bash
# Build the Tier-3a SwapMath oracle harnesses (ergo task OZRQS6, epic UP5NH6).
#
# Two harnesses, two solc versions (v3-core pragmas `<0.8.0`, v4-core `^0.8.0`):
# - V4 harness (`src/SwapMathV4Harness.sol`) + the spike's `Echo` compile under
#   solc 0.8.26 via `forge build` (foundry auto-resolves 0.8.26).
# - V3 harness (`src-v3/SwapMathV3Harness.sol`) imports v3-core's SwapMath
#   which CANNOT compile under solc 0.8 (v3-core's `FullMath.sol` pragmas
#   `>=0.4.0 <0.8.0`, and v3-core's arithmetic relies on 0.7's wrapping
#   semantics). Foundry's built-in solc manager cannot resolve solc <0.8 in
#   this environment (its list endpoint is unreachable), so the V3 harness is
#   compiled DIRECTLY with a solc 0.7.6 binary fetched from
#   binaries.soliditylang.org (cached under the svm dir for reuse). The V3
#   standard-json output is reshaped into foundry's `out/<File>.sol/<Contract>.json`
#   shape so the Rust loader reads both V3 and V4 artifacts uniformly.
#
# Both artifacts land in `tier3-oracle/out/`:
#   out/SwapMathV3Harness.sol/SwapMathV3Harness.json  (direct solc 0.7.6)
#   out/SwapMathV4Harness.sol/SwapMathV4Harness.json  (forge 0.8.26)
set -euo pipefail

TD="$(cd "$(dirname "$0")" && pwd)"   # absolute tier3-oracle/
cd "${TD}"

# ── 1. V3 harness: direct solc 0.7.6 ──────────────────────────────────────
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

# Standard-json input: compile only the V3 harness; remap v3-core into lib/.
STD_JSON="$(mktemp)"
cat > "${STD_JSON}" <<'JSON'
{
  "language": "Solidity",
  "sources": { "src-v3/SwapMathV3Harness.sol": { "urls": ["src-v3/SwapMathV3Harness.sol"] } },
  "settings": {
    "outputSelection": { "*": { "*": ["abi", "evm.bytecode.object", "evm.deployedBytecode.object"] } },
    "remappings": ["v3-core/=lib/v3-core/"]
  }
}
JSON

RAW="$(mktemp)"
"${SOLC_BIN}" --base-path . --allow-paths . --standard-json < "${STD_JSON}" > "${RAW}"
rm -f "${STD_JSON}"

# Reshape standard-json (contracts["src-v3/...:SwapMathV3Harness"] under `evm`)
# into foundry's single-contract out/ shape so the Rust loader is uniform.
mkdir -p "${TD}/out/SwapMathV3Harness.sol"
python3 - "${RAW}" "${TD}/out/SwapMathV3Harness.sol/SwapMathV3Harness.json" <<'PY'
import json, sys
raw = json.load(open(sys.argv[1]))
errs = [e for e in raw.get("errors", []) if e.get("severity") == "error"]
if errs:
    for e in errs:
        print(f"solc error: {e['formattedMessage']}", file=sys.stderr)
    sys.exit(1)
inner = raw["contracts"]["src-v3/SwapMathV3Harness.sol"]["SwapMathV3Harness"]
shaped = {
    "abi": inner["abi"],
    "bytecode": {"object": inner["evm"]["bytecode"]["object"]},
    "deployedBytecode": {"object": inner["evm"]["deployedBytecode"]["object"]},
}
json.dump(shaped, open(sys.argv[2], "w"))
print(f"wrote {sys.argv[2]}")
PY
rm -f "${RAW}"

# ── 2. V4 harness + Echo: forge 0.8.26 ─────────────────────────────────────
(cd "${TD}" && forge build)

echo "Tier-3a harnesses built:"
echo "  V3 (solc 0.7.6): ${TD}/out/SwapMathV3Harness.sol/SwapMathV3Harness.json"
echo "  V4 (forge 0.8.26): ${TD}/out/SwapMathV4Harness.sol/SwapMathV4Harness.json"
