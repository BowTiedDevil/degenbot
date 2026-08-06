#!/usr/bin/env bash
# Copy the committed tier-3 harness bytecode artifacts from the forge/solc
# `out/` tree into the tracked `artifacts/` tree, then rewrite the drift
# manifest. Called at the end of each `build-tier3-*-swap-harness.sh` so a
# harness rebuild refreshes both the bytecode and the source-hash manifest in
# one step. `artifacts/` is the toolchain-free bytecode the Rust tests load at
# `cargo test` time (no solc/forge needed to RUN the suite); `out/` stays
# gitignored build output.
set -euo pipefail
TD="$(cd "$(dirname "$0")" && pwd)"
cd "${TD}"
OUT_DIR="${OUT_DIR:-${TD}/out}"

# <out-path> <artifact-path> pairs (out shape → artifacts shape, same layout).
cp "${OUT_DIR}"/V2SwapOracleHarness.sol/V2SwapOracleHarness.json   artifacts/V2SwapOracleHarness.sol/V2SwapOracleHarness.json
cp "${OUT_DIR}"/V3SwapOracleHarness.sol/V3SwapOracleHarness.json   artifacts/V3SwapOracleHarness.sol/V3SwapOracleHarness.json
cp "${OUT_DIR}"/V4SwapOracleHarness.sol/V4SwapOracleHarness.json   artifacts/V4SwapOracleHarness.sol/V4SwapOracleHarness.json
cp "${OUT_DIR}"/SwapMathV3Harness.sol/SwapMathV3Harness.json       artifacts/SwapMathV3Harness.sol/SwapMathV3Harness.json
cp "${OUT_DIR}"/SwapMathV4Harness.sol/SwapMathV4Harness.json       artifacts/SwapMathV4Harness.sol/SwapMathV4Harness.json
cp "${OUT_DIR}"/CurveSwapOracleHarness.sol/CurveSwapOracleHarness.json   artifacts/CurveSwapOracleHarness.sol/CurveSwapOracleHarness.json
cp "${OUT_DIR}"/BalancerSwapOracleHarness.sol/BalancerSwapOracleHarness.json artifacts/BalancerSwapOracleHarness.sol/BalancerSwapOracleHarness.json
cp "${OUT_DIR}"/Echo.sol/Echo.json                                 artifacts/Echo.sol/Echo.json
cp "${OUT_DIR}"/PancakeV3SwapOracleHarness.sol/PancakeV3SwapOracleHarness.json artifacts/PancakeV3SwapOracleHarness.sol/PancakeV3SwapOracleHarness.json
cp "${OUT_DIR}"/PancakeV2SwapOracleHarness.sol/PancakeV2SwapOracleHarness.json artifacts/PancakeV2SwapOracleHarness.sol/PancakeV2SwapOracleHarness.json

"${TD}/write-harness-manifest.sh"
echo "published tier-3 harness artifacts + manifest to artifacts/"
