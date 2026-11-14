#!/usr/bin/env bash
# Copy one publish group's artifact entries from the OUT_DIR build tree into the
# tracked `artifacts/` tree, then rewrite the source-hash manifest. Called at
# the end of each publishing build script (PUBLISH=1) so a single build refreshes
# both bytecode and manifest. `artifacts/` is the toolchain-free bytecode the
# Rust tests load at `cargo test` time (no solc/forge needed to RUN the suite);
# `out/` stays gitignored build output.
#
# The selection comes from `harness-artifacts.tsv` alone: the no-arg call
# republishes the ten oracle harnesses, an explicit `executor` argument
# republishes the five executor stubs. Only the SELECTED group's outputs must
# exist, so an oracle build never needs the executor stubs staged (and vice
# versa), while a missing selected entry fails loudly instead of being skipped.
set -euo pipefail

TD="$(cd "$(dirname "$0")" && pwd)"
cd "${TD}"

GROUP="${1:-oracle}"
OUT_DIR="${OUT_DIR:-${TD}/out}"

python3 "${TD}/harness-artifacts.py" publish --group "${GROUP}" --out-dir "${OUT_DIR}"

echo "published tier-3 ${GROUP} artifacts + manifest to artifacts/"
