#!/usr/bin/env bash
# Build the committed Vyper executor artifact for the BHL2R2 tier-3b oracle
# (deterministic revm replay of the V3->V4->V3 sim-Halt against real bytecode).
#
# Compiles `/workspaces/executor` contracts/cmd_executor.vy with the PINNED
# vyper 0.5.0a3 (the only version proven to compile it — ./AGENTS.md + task
# 2ISTMX), emits creation + runtime bytecode + ABI + method identifiers +
# error map + immutable-deploy layout, and writes the artifact + manifest under
# `artifacts/executor/`. The compiled output is byte-for-byte deterministic
# (verified: a fresh build of the committed `src-executor` tree equals the
# committed artifact).
#
# Toolchain: requires the `/workspaces/executor` uv project (vyper 0.5.0a3).
# This mirrors `build-tier3-*-swap-harness.sh` and is wired into the CI
# `tier3-oracle` job (NOT the default cargo-test path, which is toolchain-free).
#
# PUBLISH=1 (default) writes into `artifacts/executor/`.
# PUBLISH=0 writes into `$OUT_DIR/executor/` (used by verify-tier3-executor-artifact.sh).
set -euo pipefail
TD="$(cd "$(dirname "$0")" && pwd)"
cd "${TD}"

EXECUTOR_SRC="${EXECUTOR_SRC:-/workspaces/executor}"
VYPROOT="${VYPROOT:-${EXECUTOR_SRC}}"
OUT_ROOT="${OUT_DIR:-${TD}/artifacts}"
OUT="${OUT_ROOT}/executor"

SRC_DIR="src-executor"
VY="cmd_executor.vy"

echo "▸ compiling ${SRC_DIR}/${VY} with pinned vyper 0.5.0a3"
mkdir -p "${OUT}"
# `uv run vyper` must run inside the executor project (its venv + pyproject own
# vyper 0.5.0a3 and the `ethereum` builtin package). Vyper resolves the
# `.interfaces` relative import from the input file's own directory, so passing
# the absolute src-executor path works regardless of cwd.
(
  cd "${VYPROOT}"
  uv run vyper -f combined_json "${TD}/${SRC_DIR}/${VY}" > /tmp/tier3_executor_combined.json
)

python3 - "$OUT" "${SRC_DIR}" "/tmp/tier3_executor_combined.json" "$VY" <<'PY'
import json, hashlib, os, sys
out, src_dir, combined_path, vy_name = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
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
# mirroring write-harness-manifest.sh.
def sha(p):
    return hashlib.sha256(open(p, "rb").read()).hexdigest()
src_hash = sha(f"{src_dir}/{vy_name}")
art = {
    "executor/cmd_executor.creation.hex":    {"source": f"{src_dir}/{vy_name}", "sha256": src_hash},
    "executor/cmd_executor.runtime.hex":     {"source": f"{src_dir}/{vy_name}", "sha256": src_hash},
    "executor/cmd_executor.abi.json":        {"source": f"{src_dir}/{vy_name}", "sha256": src_hash},
    "executor/cmd_executor.error_map.json":  {"source": f"{src_dir}/{vy_name}", "sha256": src_hash},
    "executor/cmd_executor.immutables.json": {"source": f"{src_dir}/{vy_name}", "sha256": src_hash},
}
with open(f"{out}/manifest.json", "w") as fh:
    json.dump({"vyper_version": d.get("version"), "artifacts": art}, fh, indent=2, sort_keys=True)
    fh.write("\n")
print(f"wrote {out}/: creation {len(creation)//2}B, runtime {len(runtime)//2}B, "
      f"error_map {len(err)} PCs, immutables {list(c['layout']['code_layout'].keys())}")
PY
