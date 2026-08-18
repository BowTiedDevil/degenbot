#!/usr/bin/env bash
# Build the committed SolReference artifact consumed by the
# test_v3_library_fuzz.sol_ref fixture (original Solidity V3-math libraries,
# deployed next to the Vyper test_harness for cross-implementation fuzzing).
#
# The artifact is COMMITTED (byte-deterministic for the pinned solc 0.7.6),
# mirroring the tier3-oracle compile-vs-use pattern. Run this after editing
# contracts/sol_reference/*.sol and commit the result.
set -euo pipefail

cd "$(dirname "$0")/.."

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

SOLC_VERSION="$("${SOLC_BIN}" --version | awk 'NR==2 {sub(/^Version: /, ""); print}')"

SRC_DIR="contracts/sol_reference"
OUT_DIR="${SRC_DIR}/artifacts"
mkdir -p "${OUT_DIR}"

STD="$(mktemp)"; RAW="$(mktemp)"
trap 'rm -f "${STD}" "${RAW}"' EXIT

# standard-json over every .sol file in the source dir (hermetic: content
# embedded, no --allow-paths needed).
python3 - "${SRC_DIR}" > "${STD}" <<'PY'
import json, os, sys
src_dir = sys.argv[1]
sources = {}
for name in sorted(os.listdir(src_dir)):
    if name.endswith(".sol"):
        with open(os.path.join(src_dir, name), encoding="utf-8") as f:
            sources[f"{src_dir}/{name}"] = {"content": f.read()}
print(json.dumps({
    "language": "Solidity",
    "sources": sources,
    "settings": {
        "outputSelection": {
            f"{src_dir}/SolReference.sol": {
                "SolReference": ["abi", "evm.bytecode.object", "evm.deployedBytecode.object"]
            }
        }
    },
}))
PY

"${SOLC_BIN}" --standard-json < "${STD}" > "${RAW}"

python3 - "${SRC_DIR}" "${RAW}" "${OUT_DIR}/SolReference.json" "${SOLC_VERSION}" <<'PY'
import json, sys
src_dir, raw, out, version = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
o = json.load(open(raw, encoding="utf-8"))
errs = [e for e in o.get("errors", []) if e.get("severity") == "error"]
if errs:
    print(json.dumps(errs, indent=2))
    sys.exit(1)
c = o["contracts"][f"{src_dir}/SolReference.sol"]["SolReference"]
artifact = {
    "contractName": "SolReference",
    "source": f"{src_dir}/SolReference.sol",
    "solc": {"version": version},
    "abi": c["abi"],
    "bytecode": {"object": c["evm"]["bytecode"]["object"]},
    "deployedBytecode": {"object": c["evm"]["deployedBytecode"]["object"]},
}
with open(out, "w", encoding="utf-8") as f:
    json.dump(artifact, f, indent=2, sort_keys=True)
    f.write("\n")
print(f"wrote {out} (solc {version})")
PY
