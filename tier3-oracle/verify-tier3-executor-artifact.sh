#!/usr/bin/env bash
# Authoritative compile-vs-use check for the tier-3b executor oracle artifacts
# (BHL2R2). Recompiles the committed sources with the real toolchain and
# byte-compares against the committed artifacts — so a source edit without a
# rebuild+re-publish fails CI.
#
# Covered:
#   - src-executor/cmd_executor.vy            (vyper 0.5.0a3)   -> artifacts/executor/cmd_executor.*
#   - src-executor/ExecutorV3Harness.sol      (solc 0.7.6)      -> artifacts/executor/ExecutorV3Harness.sol/ExecutorV3Harness.json
#
# REQUIRES the toolchain: `/workspaces/executor` uv project (vyper 0.5.0a3) +
# solc 0.7.6 (svm cache). Runs in the CI `tier3-oracle` job, NOT the default
# cargo-test path. The default cargo-test path is guarded toolchain-free by
# `rust/crates/degenbot-simulation/tests/tier3_executor_artifacts.rs`.
set -euo pipefail
TD="$(cd "$(dirname "$0")" && pwd)"
cd "${TD}"

SCRATCH="$(mktemp -d)"
trap 'rm -rf "${SCRATCH}"' EXIT

# Recompile into SCRATCH (PUBLISH=0 -> OUT_DIR=SCRATCH).
OUT_DIR="${SCRATCH}" PUBLISH=0 "${TD}/build-tier3-executor-harness.sh" >/dev/null

fail=0
check() {
    local rel="$1" how="$2"
    if ! cmp -s "artifacts/executor/${rel}" "${SCRATCH}/executor/${rel}"; then
        echo "  ✗ executor/${rel}: committed ≠ fresh ${how} compile"
        fail=1
    else
        echo "  ✓ executor/${rel}: matches fresh ${how} compile"
    fi
}

echo "── recompiling executor + V3 topology harness via build-tier3-executor-harness.sh"
for f in cmd_executor.creation.hex cmd_executor.runtime.hex cmd_executor.abi.json \
         cmd_executor.error_map.json cmd_executor.immutables.json; do
    check "${f}" "vyper 0.5.0a3"
done
check "ExecutorV3Harness.sol/ExecutorV3Harness.json" "solc 0.7.6"

if [ "${fail}" != "0" ]; then
    echo
    echo "ERROR: a committed tier-3b artifact does not match a fresh toolchain build."
    echo "Re-run tier3-oracle/build-tier3-executor-harness.sh (PUBLISH=1), then commit artifacts/executor/."
    exit 1
fi
echo
echo "OK: committed tier-3b artifacts match fresh vyper 0.5.0a3 + solc 0.7.6 compiles."
