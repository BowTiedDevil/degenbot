# Vyper executor artifact (BHL2R2 / tier-3b)

Committed binary artifact for the deterministic evm2-replay oracle that
reproduces the V3->V4->V3 executor sim-Halt against real bytecode (tasks
2ISTMX [this pipeline], 4O7BPZ [RED harness], 72YZXI [fix]).

## What is here
- `cmd_executor.creation.hex` — the production creation bytecode
  (`vyper -f bytecode` / `combined_json.bytecode`), no `0x`.
- `cmd_executor.runtime.hex` — the production runtime bytecode.
- `cmd_executor.abi.json`, `cmd_executor.method_identifiers.json`.
- `cmd_executor.error_map.json` — PC -> revert-reason label for the
  compiler-emitted error checks (e.g. `safediv`, `safemul`, `user revert with
  custom error`). The ONLY per-PC source attribution vyper provides.
- `cmd_executor.immutables.json` — `code_layout` (the immutable values and their
  offsets), needed to deploy.
- `ExecutorV3Harness.sol/ExecutorV3Harness.json` — deployment artifact
  (solc 0.7.6) for the shared-token topology harness that deploys two real
  `UniswapV3Pool`s routing through a single shared WETH.
- `manifest.json` — artifact -> tracked-source sha256 + `vyper_version`.

## Deploy contract (immutables are deploy-time constructor args)
The constructor takes four role addresses: WETH, PoolManager, USDC, and USDT.
The `immutables.json` `code_layout` lists seven entries; `OWNER_ADDR` is
endogenous and the two delta slots are computed in `__init__`. Deploy with
`create(creation.hex ++ abi.encode(weth, pool_manager, usdc, usdt))` (128
bytes), not 7*32 bytes. Immutables live in runtime code, not storage.
`tier3_executor_deploy.rs` verifies deployment and the computed delta slots.

## CRITICAL finding: no per-PC line attribution from vyper's source map
Both the venom (experimental-codegen) and legacy
`-f source_map` outputs emit ONLY a single whole-file span — the
`pc_pos_map_compressed` is ~15k segments that all inherit `[0, <file len>, ...]`,
and the structured `pc_pos_map` collapses to a single `{"0": [1,0,2077,103]}`.
**Vyper's source map cannot map a halt PC to a cmd_executor.vy line.** S2 must
attribute the halt via (a) `cmd_executor.error_map.json` when the halt is an
arithmetic-revert PC, (b) the executor's method delegation (`unlockCallback` is
the V4 path), and (c) direct source inspection of the V4 custody flow
(`V4_TAKE_DELTA` / `V4_SETTLE_ALL` ordering). Recorded on 4O7BPZ.

## Regenerate / verify
- Rebuild + re-publish: `tier3-oracle/build-tier3-executor-harness.sh` (PUBLISH=1).
- Authoritative compile-vs-use (real vyper 0.5.0b2, CI tier3-oracle job):
  `just verify-tier3-executor-artifact` (= `verify-tier3-executor-artifact.sh`).
- Toolchain-free drift guard (default cargo-test path):
  `rust/crates/engine/degenbot-simulation/tests/tier3_executor_artifacts.rs`.
