# Migration guide: Verify-lifecycle core ownership (feasibility)

Spike delivery for task **A4YORC** (epic `Z5CNPB`). Assesses the feasibility of
owning the pool registration **verify-lifecycle** in the Rust core — the
`quarantine → apply_buffer → step1 verify → step2 verify → set_live` sequence
plus the sparse/tracked release policy and the state-tripwire final gate
(epic decision D4). Informs task `IKGQ6F`.

## Feasibility verdict: HIGH — the hard parts are already core

Two of the three layers of "verify-lifecycle" already live core-side; only the
orchestration gate and the RPC-provider hop remain Python. So this is an
absorption task, not a rewrite:

1. **On-chain comparison math** — already core and standalone-reachable:
   - `degenbot-bot/bot_core/liquidity_verifier.rs`:
     `verify_v3_liquidity_map`, `verify_v4_liquidity_map`, `verify_v3_pools`,
     `verify_v4_pools` (batch into 2 `eth_call`s; byte-identical gross/net
     mismatch surfacing, with unit tests).
   - `degenbot-bot/bot_core/snapshot_verify.rs`: `verify_v3_snapshot`,
     `verify_v3_backfill`, `verify_v4_snapshot`, `verify_v4_backfill`.
2. **Buffer gating + race-free on-block** — core owns the pump buffer and the
   pin (`pin_v3/v4_post_drain_snapshot` captured atomically with the final
   drain under a single `core.write()`), which is what makes step-2
   race-free. The `Quarantined → Live` lifecycle primitives exist core-side
   (`set_v3_pool_quarantined`, `set_*_pool_live`, `release_all_v3_v4_quarantined`).
3. **What remains Python** —
   - the **orchestration gate** in
     `src/degenbot/arbitrage/engine_registry.py::{register_v3,v4}_pool` (order,
     config gating, per-pool step sequencing, then `set_*_pool_live`);
   - the **verify RPC provider hop** in `degenbot-python/.../pump.rs`
     (`verify_v3_snapshot_seed`/`verify_v4_*` build an alloy provider from a
     `rpc_url` string per call, then drive the core verify). This hop is a
     choreography wrapper of the same kind T1 absorbs over `ConstructionIo`.

## What core ownership means (target shape)

A core registration-lifecycle that transitions a registered pool through the
D4 states, so Python registration becomes a thin driver and no live code
depends on Python sequencing:

```
registered (Quarantined if Tracked, Live-if-Sparse)
  ├─ Sparse  → Live immediately (no verification; DFQYM5 "Sparse stays Live")
  └─ Tracked → apply_buffer (drain)
               step1: verify pinned seed @ snapshot block
               step2: verify pinned post-drain pair @ its own block
               state tripwire (ADR-021) as the FINAL gate
               → Live
```

Enforcement points (keep in core):
- **Coverage branch** in the register path: `PoolTickCoverage::Sparse` skips
  verification and goes `Live`; `Tracked` must pass both verify steps + the
  tripwire before `Live`. Core already carries coverage on the pool.
- **Verify RPC via `ConstructionIo`**, not a per-call `rpc_url` provider built
  in pyo3. The verify comparison is separate from the construction provider
  (a state-view contract / separate `rpc_url` today), so `ConstructionIo`
  likely needs to carry an optional **second** RPC handle (or the verify
  provider) — a concrete design point for `IKGQ6F`.
- **Tripwire last**: the ADR-021 solve-stage tripwire / freshness guard must
  gate the `Live` transition (no solvable state on unverified or stale state).
  Reuse `liquidity_verifier.rs` + `snapshot_verify.rs` — do NOT re-invent the
  comparison; absorb the gate only.

## Risks / invariants to preserve (ordered)

1. **Rolling-start race (CBCH6H)** — step-1 compares the pinned *snapshot seed*
   @ snapshot block, NOT engine-current (the live pump applies onto
   engine-current during a rolling start). The core fns already encode this;
   the gate must keep passing the pinned block, never the engine current head.
2. **Post-drain self-consistency** — step-2 compares the pinned `(state,
   block)` captured atomically with the drain; the gate must not substitute a
   constant backfill block (the 2026-06-29 crash class).
3. **Quarantine-before-first-RPC-await** — the pool must be Quarantined before
   any RPC so a live event in the drain+pin+verify window cannot advance
   `update_block` past `last_complete_block` (the YLYJM2 gap). If the gate moves
   core-side, `register_*_pool` must still quarantine pre-await.
4. **Never auto-repair** — verification failure stays fail-fast (typed
   `VerificationMismatchError`, crash-loudly); the core gate inherits the
   ADR-021 "detect/classify/stop loudly" posture.

## Ordering / coupling

- T1 (`F2R2OC`) absorbs the generic verify-RPC-provider hop shape over
  `ConstructionIo`; `IKGQ6F` reuses it for the verify provider.
- `IKGQ6F` (this spike's implementing task) builds the core lifecycle on the
  existing pin/verify fns; `WSLCD2` (Part 2) then makes release **per-path**
  and demotes `release_all_v3_v4_quarantined` to an orphan sweep only.
- Python `EngineRegistry.register_v3/v4_pool` becomes a driver that only passes
  `(coverage, idents, verify config)` and lets the core sequence the lifecycle.

## Validation

- Core unit tests: sparse→Live-without-verify; tracked→verify-then-Live;
  tripwire-blocks-Live; verify-mismatch fails fast (no `Live`).
- Rolling-start race + post-drain self-consistency regressions reuse the
  existing pin/verify core tests (no new race surface introduced by the gate).
- Tier-2 dual-driver: `register_*_pool` through `BotState` (Rust) and `PyBot`
  (Python) produce identical lifecycle outcomes for a shared fixture.

## Files

- Core (new): `degenbot-bot/bot_core/pool_builder/` grows the lifecycle
  state machine (or a sibling `registration_lifecycle.rs`).
- Moved/re-pointed: `degenbot-python/.../pump.rs` verify fns (provider hop →
  `ConstructionIo`); `src/degenbot/arbitrage/engine_registry.py` register_* →
  thin driver.
- Unchanged: `liquidity_verifier.rs`, `snapshot_verify.rs` comparison math.
