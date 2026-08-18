#!/usr/bin/env bash
# Build the committed Vyper executor artifact for the BHL2R2 tier-3b oracle
# (deterministic revm replay of the V3->V4->V3 sim-Halt against real bytecode).
#
# Compiles the vendored `executor/contracts/cmd_executor.vy` with the PINNED
# vyper 0.5.0a3 (the only version proven to compile it — ./AGENTS.md + task
# 2ISTMX), emits creation + runtime bytecode + ABI + method identifiers +
# error map + immutable-deploy layout, and writes the artifact + manifest under
# `artifacts/executor/`. The compiled output is byte-for-byte deterministic
# (verified: a fresh build of the committed source equals the committed
# artifact). The V3 topology harness (`src-executor/ExecutorV3Harness.sol`)
# is compiled alongside with solc 0.7.6.
#
# Toolchain: the in-repo `executor/` uv project (`uv run vyper` executes
# inside it; vyper pinned ==0.5.0a3, requires-python >=3.14). The executor
# contracts were pulled in from the sibling /workspaces/executor repo
# (epic SRMMM7) — degenbot is now self-contained.
#
# EXECUTOR_DIR / VYPROOT env vars override the in-repo defaults for one-off
# builds. This mirrors `build-tier3-*-swap-harness.sh` and is wired into the
# CI `tier3-oracle` job (NOT the default cargo-test path, which is
# toolchain-free).
#
# PUBLISH=1 (default) writes into `artifacts/executor/`.
# PUBLISH=0 writes into `$OUT_DIR/executor/` (used by verify-tier3-executor-artifact.sh).
set -euo pipefail
TD="$(cd "$(dirname "$0")" && pwd)"
cd "${TD}"

REPO_ROOT="$(cd "${TD}/.." && pwd)"
EXECUTOR_DIR="${EXECUTOR_DIR:-${REPO_ROOT}/executor}"
VYPROOT="${VYPROOT:-${EXECUTOR_DIR}}"
VYPER_SRC="${EXECUTOR_DIR}/contracts/cmd_executor.vy"
OUT_ROOT="${OUT_DIR:-${TD}/artifacts}"
OUT="${OUT_ROOT}/executor"

# The V3 topology harness (solc 0.7.6) stays oracle-side; the Vyper executor
# source + interfaces live in the vendored executor/ project.
SRC_DIR="src-executor"

echo "▸ compiling ${VYPER_SRC} with pinned vyper 0.5.0a3"
mkdir -p "${OUT}"
# `uv run vyper` must run inside the executor project (its venv + pyproject own
# the pinned vyper 0.5.0a3 and the `ethereum` builtin package). Vyper resolves
# the `.interfaces` relative import from the input file's own directory, so
# passing the absolute contracts path works regardless of cwd.
(
  cd "${VYPROOT}"
  uv run vyper -f combined_json "${VYPER_SRC}" > /tmp/tier3_executor_combined.json
)

# ── V3 topology harness (solc 0.7.6) ─────────────────────────────────────
# Deploys two real UniswapV3Pools with caller-supplied (shared) tokens. v3-core
# pragmas are <0.8.0 and need 0.7.x wrapping semantics, so — like the tier-3a
# V3 harness — compile directly with the cached solc 0.7.6 binary.
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
V3_SOL="ExecutorV3Harness.sol"
STD_JSON="$(mktemp)"
cat > "${STD_JSON}" <<JSON
{
  "language": "Solidity",
  "sources": { "src-executor/${V3_SOL}": { "urls": ["src-executor/${V3_SOL}"] } },
  "settings": {
    "outputSelection": { "*": { "*": ["abi", "evm.bytecode.object", "evm.deployedBytecode.object"] } },
    "remappings": ["v3-core/=lib/v3-core/"]
  }
}
JSON
RAW="$(mktemp)"
"${SOLC_BIN}" --base-path . --allow-paths . --standard-json < "${STD_JSON}" > "${RAW}"
rm -f "${STD_JSON}"
mkdir -p "${OUT}/${V3_SOL}"
V3JSON="${OUT}/${V3_SOL}/${V3_SOL%.sol}.json"
python3 - "${RAW}" "${V3JSON}" <<'PYV3'
import json, sys
raw = json.load(open(sys.argv[1]))
errs = [e for e in raw.get("errors", []) if e.get("severity") == "error"]
if errs:
    raise SystemExit("\n".join(e["formattedMessage"] for e in errs))
inner = raw["contracts"]["src-executor/ExecutorV3Harness.sol"]["ExecutorV3Harness"]
shaped = {
    "abi": inner["abi"],
    "bytecode": {"object": inner["evm"]["bytecode"]["object"]},
    "deployedBytecode": {"object": inner["evm"]["deployedBytecode"]["object"]},
}
json.dump(shaped, open(sys.argv[2], "w"))
print(f"wrote {sys.argv[2]}")
PYV3
rm -f "${RAW}"

python3 - "$OUT" "$REPO_ROOT" "/tmp/tier3_executor_combined.json" "$VYPER_SRC" <<'PY'
import json, hashlib, os, sys
out, repo_root, combined_path, vy_abs = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
vy_name = os.path.basename(vy_abs)
d = json.load(open(combined_path))
key = [k for k in d if vy_name in k]
if not key:
    raise SystemExit(f"no {vy_name} key in combined_json: {list(d)[:8]}")
c = d[key[0]]
os.makedirs(out, exist_ok=True)

def strip0x(s):
    return s[2:] if s.startswith("0x") else s

# creation = production bytecode; immutables are supplied as appended
# 32-byte constructor args in code_layout order (see immutables.json).
creation = strip0x(c["bytecode"]).strip()
runtime = strip0x(c["bytecode_runtime"]).strip()
open(f"{out}/cmd_executor.creation.hex", "w").write(creation)
open(f"{out}/cmd_executor.runtime.hex", "w").write(runtime)
open(f"{out}/cmd_executor.abi.json", "w").write(json.dumps(c["abi"], indent=0))
open(f"{out}/cmd_executor.method_identifiers.json", "w").write(json.dumps(c["method_identifiers"], indent=0))
err = c["source_map_runtime"].get("error_map", {})
open(f"{out}/cmd_executor.error_map.json", "w").write(json.dumps(err, indent=0))
open(f"{out}/cmd_executor.immutables.json", "w").write(json.dumps(c["layout"]["code_layout"], indent=0))

# manifest: artifacts -> tracked-source sha256, plus toolchain pin. Every
# artifact maps to the sha256 of the SAME git-tracked source (cmd_executor.vy),
# mirroring write-harness-manifest.sh. The V3 topology harness (solc 0.7.6)
# maps to its own tracked .sol source. `source` paths are REPO-ROOT-relative
# (the guard test tier3_executor_artifacts.rs joins them against the repo root).
def sha(p):
    return hashlib.sha256(open(p, "rb").read()).hexdigest()
vy_src = os.path.relpath(vy_abs, repo_root)
src_hash = sha(vy_abs)
v3sol_abs = os.path.join(repo_root, "tier3-oracle/src-executor/ExecutorV3Harness.sol")
v3_hash = sha(v3sol_abs)
art = {
    "executor/cmd_executor.creation.hex":    {"source": vy_src, "sha256": src_hash},
    "executor/cmd_executor.runtime.hex":     {"source": vy_src, "sha256": src_hash},
    "executor/cmd_executor.abi.json":        {"source": vy_src, "sha256": src_hash},
    "executor/cmd_executor.error_map.json":  {"source": vy_src, "sha256": src_hash},
    "executor/cmd_executor.immutables.json": {"source": vy_src, "sha256": src_hash},
    "executor/ExecutorV3Harness.sol/ExecutorV3Harness.json": {"source": "tier3-oracle/src-executor/ExecutorV3Harness.sol", "sha256": v3_hash},
}
with open(f"{out}/manifest.json", "w") as fh:
    json.dump({"vyper_version": d.get("version"), "artifacts": art}, fh, indent=2, sort_keys=True)
    fh.write("\n")
print(f"wrote {out}/: creation {len(creation)//2}B, runtime {len(runtime)//2}B, "
      f"error_map {len(err)} PCs, immutables {list(c['layout']['code_layout'].keys())}")
PY
