#!/usr/bin/env bash
# Authoritative compile-vs-use check for the Vyper executor artifact
# (BHL2R2 / tier-3b). Recompiles the committed `src-executor/cmd_executor.vy`
# with the pinned vyper 0.5.0a3 and byte-compares creation + runtime against the
# committed artifacts — so a source edit without a rebuild+re-publish fails CI.
#
# REQUIRES the toolchain: `/workspaces/executor` uv project (vyper 0.5.0a3).
# Runs in the CI `tier3-oracle` job, NOT the default cargo-test path. The
# default cargo-test path is guarded toolchain-free by
# `rust/crates/degenbot-simulation/tests/tier3_executor_artifacts.rs`, which
# hashes the tracked source vs the committed manifest.
set -euo pipefail
TD="$(cd "$(dirname "$0")" && pwd)"
cd "${TD}"

SCRATCH="$(mktemp -d)"
trap 'rm -rf "${SCRATCH}"' EXIT

# Recompile into SCRATCH (PUBLISH=0 -> OUT_DIR=SCRATCH).
OUT_DIR="${SCRATCH}" PUBLISH=0 "${TD}/build-tier3-executor-harness.sh" >/dev/null

fail=0
check() {
    local rel="$1"
    if ! cmp -s "artifacts/executor/${rel}" "${SCRATCH}/executor/${rel}"; then
        echo "  ✗ executor/${rel}: committed ≠ fresh compile"
        fail=1
    else
        echo "  ✓ executor/${rel}: matches fresh vyper 0.5.0a3 compile"
    fi
}

echo "── recompiling executor via build-tier3-executor-harness.sh"
for f in cmd_executor.creation.hex cmd_executor.runtime.hex cmd_executor.abi.json \
         cmd_executor.error_map.json cmd_executor.immutables.json; do
    check "${f}"
done

if [ "${fail}" != "0" ]; then
    echo
    echo "ERROR: committed executor artifact does not match a fresh vyper 0.5.0a3 build."
    echo "Re-run tier3-oracle/build-tier3-executor-harness.sh (PUBLISH=1), then commit artifacts/executor/."
    exit 1
fi
echo
echo "OK: committed executor artifact matches a fresh vyper 0.5.0a3 compile."
