#!/usr/bin/env bash
# Rebuild the executor's fake Uniswap fixtures and publish their pinned
# bytecode + source hashes. Uses the same Foundry toolchain as the V4 oracle.
set -euo pipefail
TD="$(cd "$(dirname "$0")" && pwd)"
cd "${TD}"
OUT="$(mktemp -d)"
trap 'rm -rf "${OUT}"' EXIT
forge build src-harness/V2ExecutorStub.sol src-harness/V3ExecutorStub.sol src-harness/V4ExecutorStub.sol --use 0.8.26 --via-ir --optimize --out "${OUT}"
mkdir -p artifacts/harness
for contract in Pair Token PoolV3 TestV3SwapDriver PoolManager; do
  case "${contract}" in
    Pair) source=V2ExecutorStub ;;
    Token) source=StubToken ;;
    PoolV3|TestV3SwapDriver) source=V3ExecutorStub ;;
    PoolManager) source=V4ExecutorStub ;;
  esac
  cp "${OUT}/${source}.sol/${contract}.json" "artifacts/harness/${contract}.json"
done
./write-harness-manifest.sh
