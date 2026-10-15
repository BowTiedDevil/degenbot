#!/usr/bin/env bash
# Build the Tier-3a SwapMath oracle harnesses (ergo task OZRQS6, epic UP5NH6).
#
# Three harnesses, two solc versions (v3-core pragmas `<0.8.0`, v4-core `^0.8.0`):
# - V4 harness (`src-v4/SwapMathV4Harness.sol`) compiles under solc 0.8.26
#   via `forge build` (foundry auto-resolves 0.8.26).
# - The V3 harnesses (`src-v3/SwapMathV3Harness.sol`,
#   `src-v3/V3CaptureHarness.sol`) import v3-core, which CANNOT compile under
#   solc 0.8 (v3-core's `FullMath.sol` pragmas `>=0.4.0 <0.8.0`, and v3-core's
#   arithmetic relies on 0.7's wrapping semantics). Foundry's built-in solc
#   manager cannot resolve solc <0.8 in this environment (its list endpoint is
#   unreachable), so both are compiled DIRECTLY with a solc 0.7.6 binary
#   fetched from binaries.soliditylang.org (cached under the svm dir for
#   reuse). The standard-json output is reshaped into foundry's
#   `out/<File>.sol/<Contract>.json` shape so the Rust loader reads all
#   artifacts uniformly.
#
# V3CaptureHarness compiles here because publish-artifacts.sh copies its
# artifact unconditionally: with no other build step producing it, every
# publishing family failed closed at that cp.
#
# All artifacts land in `tier3-oracle/out/`:
#   out/SwapMathV3Harness.sol/SwapMathV3Harness.json  (direct solc 0.7.6)
#   out/V3CaptureHarness.sol/V3CaptureHarness.json    (direct solc 0.7.6)
#   out/SwapMathV4Harness.sol/SwapMathV4Harness.json  (forge 0.8.26)
set -euo pipefail

TD="$(cd "$(dirname "$0")" && pwd)"   # absolute tier3-oracle/
cd "${TD}"
OUT_DIR="${OUT_DIR:-${TD}/out}"
PUBLISH="${PUBLISH:-1}"

# Ensure the canonical v3-core/v4-core reference sources are present
# (idempotent; no-op if already cloned).
"${TD}/bootstrap-libs.sh"

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
mkdir -p "${OUT_DIR}/SwapMathV3Harness.sol"
python3 - "${RAW}" "${OUT_DIR}/SwapMathV3Harness.sol/SwapMathV3Harness.json" <<'PY'
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

# ── 2. V3 capture harness: direct solc 0.7.6 ────────────────────────────────
# Same toolchain path as section 1 (v3-core pragmas <0.8); optimizer runs=800
# matches the settings that produced the committed capture artifact.
CAP_JSON="$(mktemp)"
cat > "${CAP_JSON}" <<'JSON'
{
  "language": "Solidity",
  "sources": { "src-v3/V3CaptureHarness.sol": { "urls": ["src-v3/V3CaptureHarness.sol"] } },
  "settings": {
    "optimizer": { "enabled": true, "runs": 800 },
    "outputSelection": { "*": { "*": ["abi", "evm.bytecode.object", "evm.deployedBytecode.object"] } },
    "remappings": ["v3-core/=lib/v3-core/"]
  }
}
JSON

CAP_RAW="$(mktemp)"
"${SOLC_BIN}" --base-path . --allow-paths . --standard-json < "${CAP_JSON}" > "${CAP_RAW}"
rm -f "${CAP_JSON}"

mkdir -p "${OUT_DIR}/V3CaptureHarness.sol"
python3 - "${CAP_RAW}" "${OUT_DIR}/V3CaptureHarness.sol/V3CaptureHarness.json" <<'PY'
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
inner = raw["contracts"]["src-v3/V3CaptureHarness.sol"]["V3CaptureHarness"]
shaped = {
    "abi": inner["abi"],
    "bytecode": {"object": inner["evm"]["bytecode"]["object"]},
    "deployedBytecode": {"object": inner["evm"]["deployedBytecode"]["object"]},
}
json.dump(shaped, open(sys.argv[2], "w"))
print(f"wrote {sys.argv[2]}")
PY
rm -f "${CAP_RAW}"

# ── 3. V4 harness: forge 0.8.26 ─────────────────────────────────────────────
# forge compiles into a scratch dir, never the shared out/: its default
# profile has the optimizer off and emits full foundry-shape JSON, so a direct
# out/ build would clobber the committed-shape artifacts the per-family
# direct-solc scripts own — and publish-artifacts.sh copies ALL of out/.
FORGE_OUT="$(mktemp -d)"
(cd "${TD}" && forge build --out "${FORGE_OUT}")
mkdir -p "${OUT_DIR}/SwapMathV4Harness.sol"
cp "${FORGE_OUT}/SwapMathV4Harness.sol/SwapMathV4Harness.json" "${OUT_DIR}/SwapMathV4Harness.sol/SwapMathV4Harness.json"
rm -rf "${FORGE_OUT}"


# Publish committed bytecode + drift manifest (toolchain-free `cargo test` path).
if [ "${PUBLISH}" != "0" ]; then
  "${TD}/publish-artifacts.sh"
fi
echo "Tier-3a harnesses built:"
echo "  V3 (solc 0.7.6): ${TD}/out/SwapMathV3Harness.sol/SwapMathV3Harness.json"
echo "  V3 capture (solc 0.7.6): ${TD}/out/V3CaptureHarness.sol/V3CaptureHarness.json"
echo "  V4 (forge 0.8.26): ${TD}/out/SwapMathV4Harness.sol/SwapMathV4Harness.json"
