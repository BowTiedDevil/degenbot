# Sim-vs-engine divergence probe — implementation + runbook (ergo 4C33DP)

> Implementation note for ergo task `4C33DP` (epic `TR6GWT` — stale-state
> elimination). The encoder artifacts + the per-tick reverse-map live in
> `rust/crates/degenbot-bot/src/bot_core/divergence_probe.rs` (the engine-side
> reverse-mapper + encoders) and `rust/crates/degenbot-simulation/src/sim/evm/
> divergence_probe.rs` (the env-gated observer + tally).

## What was built

A pure-observation probe wired into `BotStateDb::storage_ref` (the seam that
already holds both `&BotState` and the RPC fallback). On every revm SLOAD, when
`DEGENBOT_SIM_DIVERGENCE_LOG=1` is set, it:

1. reverse-maps `(address, index)` → a tracked pool's scalar storage slot
   (V2 reserves slot 8; V3 `slot0`/`liquidity`; V4 `Pool.State` `slot0`/
   `liquidity` at the derived `S_state`),
2. packs the engine's typed state into the on-chain word (untracked bits
   zeroed + a `tracked_bit_mask`),
3. compares the masked RPC value against the engine word,
4. logs a `[sim-divergence] pool=.. slot=.. kind=.. engine=.. rpc=..
   update_block=..` line on a mismatch + accumulates a `DivergenceTally`.

Env-gated (default OFF → single atomic load per SLOAD), observation-only
(returns the RPC value UNCHANGED — the reverted-bug `storage_ref` serve is
NOT re-introduced; zero LOK/K risk).

## How to use it

```bash
DEGENBOT_SIM_DIVERGENCE_LOG=1 \
DEGENBOT_MIN_PROFIT_MARGIN_BPS=0 \
uv run python examples/eth_backrun_v2_v3_v4_rust.py --permutation V4-V3-V3 \
  2>&1 | tee logs/debug/sim_divergence_V4-V3-V3.log
```

`DEGENBOT_MIN_PROFIT_MARGIN_BPS=0` disables the thin-margin filter so
candidates reach the simulator. The `[sim-divergence]` lines route through
`degenbot_simulation.sim.evm.divergence_probe` (the `degenbot_simulation`
crate-root was added to `RUST_BRIDGE_LOGGER_NAMES` so pyo3-log no longer drops
them — a latent bug fixed alongside this).

## What the data answers (the spike checkpoint)

Filter the log:

```bash
rg '^\[sim-divergence\] pool=' logs/debug/sim_divergence_V4-V3-V3.log | sort | uniq -c
```

- **If the engine's scalar slots MATCH the RPC** (no/minimal `[sim-divergence]`
  lines) → the divergence is in the slots the engine does NOT carry
  (`feeGrowthGlobal`, `tickBitmap`, per-tick `feeGrowthOutside`, V2 token
  balances) → **fix path A** (extend engine state to carry the full slot set).
- **If the engine LAGS the RPC** (divergence lines, `update_block < sim_block`)
  → the engine's event-applied state trails the RPC head during the serial
  fan-out → **fix path C** (gated serve: only when `update_block ==
  sim_block`) or **fix path B** (shadow-RPC batch at sim block, both solver +
  sim read the same fresh batch).
- **If the engine MATCHES but the RPC advanced mid-fanout** (divergence lines
  only on later-path candidates, `update_block == solve_block` but rpc is 1
  block ahead) → **fix path B** (the shadow-RPC-at-sim-block batch eliminates
  the fan-out skew).

`dump_divergence_summary()` logs the running tally (`slots_compared` /
`divergent_slots` / `divergent_pools`) — expose via a driver hook if a
per-batch summary is wanted.

## Scope + deferred

The scalar slots (slot0 / liquidity / reserves) are sufficient to ANSWER the
checkpoint (do the engine's price/liquidity/reserves lag the sim block?). The
per-tick `ticks(tick)` reverse-map (slot+0 word: `liquidityGross |
liquidityNet` — the engine DOES carry both) is **deferred**: it requires
`keccak256(sign_extend_24(tick) . base)` per tick per cold read. The encoder
for the tick word + the V4 `S_state+4` tick base land with the deferred encoder
tasks (V5W756 / H3M6AH / PXQAEY) — they reuse the `derive_v4_pool_state_base`
+ the V3/V4 scalar encoders built here.

## Status — CAPTURED (path A selected)

- ✅ Code: landed + unit-tested (13 engine-side incl. per-tick reverse-map +
  6 simulation-side tests; clippy clean; no-pyo3-in-cores invariant holds).
- ✅ Logger bridge: `degenbot_simulation` + `degenbot_backrun_strategy` crate-
  roots added to `RUST_BRIDGE_LOGGER_NAMES` (latent drop-on-unconfigured-root
  bug fixed alongside).
- ✅ **Mainnet capture (V4-V3-V3, 5 candidates across two runs)**:

  | run | candidates | slots_compared | divergent_slots | divergent_pools |
  |----:|-----------:|---------------:|----------------:|----------------:|
  | 1   | 2          | 8  (scalar)             | 0               | 0               |
  | 2   | 3          | 10 (scalar + per-tick)  | 0               | 0               |
  | **total** | **5** | **18**               | **0**           | **0**           |

  Every candidate reverted `CurrencyNotSettled`, yet the engine's tracked
  slots — V3/V4 `slot0` (sqrtPrice/tick), `liquidity`, AND each per-tick
  `ticks(tick)` slot+0 (`liquidityGross`/`liquidityNet`) — ALL MATCHED the
  RPC-served value at sim time (`update_block` caught up, zero divergence).

  Artifact: `logs/debug/sim_divergence_V4-V3-V3.log`.

### Decision: path A (extend engine state)

The data **eliminates path C** (gated serve when caught up): the engine is
NOT lagging on the slots it carries — `update_block` is current + the packed
words are byte-identical to RPC. It also **eliminates path B** (shadow-RPC at
sim block): the scalars already match at sim time, so the sim/solver divergence
is NOT a fan-out-skew artifact.

The `CurrencyNotSettled` revert's root cause is therefore the **untracked slot
classes** the engine does NOT serve: V3/V4 `feeGrowthGlobal0/1X128` (slots
+1/+2), per-tick `feeGrowthOutside0/1X128` (tick slot+1/+2), and the V3
`observations` array + V2 per-pair ERC-20 `balanceOf`. The on-chain V3/V4
`swap()` callback reads these alongside the matching slot0/liquidity/ticks;
the engine's CL swap math (which the solver used to derive `hop_outputs`)
does NOT model fee-growth accrual the way the on-chain code does, so on a
fee-accruing / cross-tick swap the solver's `hop_output` diverges from the
sim's actual on-chain swap result → the V4 `SETTLE`/`TAKE` exact-amount comes
up short → `CurrencyNotSettled`.

This is **path A**: extend the engine state (`V3PoolState`/`V4PoolState`/
`TickInfo`) to carry the full slot set the on-chain swap callback reads, +
apply `feeGrowthGlobal`/`feeGrowthOutside`/observation deltas from the
Mint/Burn/Swap events, so the solver's CL math + the sim's revm-served state
are consistent. The deferred encoder tasks (V5W756/H3M6AH/PXQAEY) REUSE the
encoders built here; the production `storage_ref` serving seam is gated on
the engine-state extension landing (ergo task `NQ3FPV`).
