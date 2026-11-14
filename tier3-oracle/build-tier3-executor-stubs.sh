#!/usr/bin/env bash
# Build the executor's five fake Uniswap fixtures (`Pair`, `Token`, `PoolV3`,
# `TestV3SwapDriver`, `PoolManager`) from `src-harness/` with the same Foundry
# toolchain as the V4 oracle, using the artifact contract the oracle harness
# builds follow:
#
#   OUT_DIR (default tier3-oracle/out) receives all five artifacts at their
#   `harness/*.json` entry paths from `harness-artifacts.tsv`.
#   PUBLISH=1 (default) republishes the executor group from OUT_DIR into the
#   tracked `artifacts/` tree and refreshes their source hashes.
#   PUBLISH=0 stops after OUT_DIR, so `verify-tier3-artifacts.sh` can
#   byte-compare a scratch build without touching tracked files.
set -euo pipefail

TD="$(cd "$(dirname "$0")" && pwd)"
cd "${TD}"

OUT_DIR="${OUT_DIR:-${TD}/out}"
PUBLISH="${PUBLISH:-1}"

# forge's own output tree is a throwaway: the staged layout under OUT_DIR is the
# shared contract every publisher and verifier reads.
RAW="$(mktemp -d)"
trap 'rm -rf "${RAW}"' EXIT

forge build src-harness/V2ExecutorStub.sol src-harness/V3ExecutorStub.sol src-harness/V4ExecutorStub.sol --use 0.8.26 --via-ir --optimize --out "${RAW}"

python3 "${TD}/harness-artifacts.py" stage --raw-dir "${RAW}" --group executor --out-dir "${OUT_DIR}"

if [ "${PUBLISH}" != "0" ]; then
  "${TD}/publish-artifacts.sh" executor
fi

echo "Tier-3 executor stub fixtures built:"
while IFS= read -r entry; do
  echo "  ${OUT_DIR}/${entry}"
done < <(python3 "${TD}/harness-artifacts.py" rows --group executor)
