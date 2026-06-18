{
  "id": "ad70caeb",
  "title": "Slice 12: Balancer family port — BalancerV2Pool/StablePool over PyLiquidityPool",
  "tags": [
    "polars-three-layer",
    "adr-005",
    "balancer",
    "third-family",
    "rust",
    "large",
    "slice-12"
  ],
  "status": "open",
  "created_at": "2026-06-17T19:27:06.217Z"
}

**Slice 12 of the Polars three-layer migration (ADR-005; ADR-003 "third family").** Master: `TODO-7e24d695`. Deps: slice 10 (engine unified). Large — two sub-invariants; expect sub-slicing (weighted → stable).

**Goal.** Extend `PoolEntry` with Balancer weighted + stable variants + Rust state + make `BalancerV2Pool`/`BalancerV2StablePool` companions over `PyLiquidityPool`. Port `WeightedMath`/`StableMath` to Rust; handle `PowVersion`; `CacheAwareRateProvider`/`StaleRateResult`.

**Rust (new).**
- Balancer weighted state struct + `PoolEntry::BalancerWeighted(...)`.
- Balancer stable state struct + `PoolEntry::BalancerStable(...)` (two invariant versions V1/V2 — `INVARIANT_V1` always-roundDown-with-D_P vs `INVARIANT_V2` roundUp-param-with-P_D, per AGENTS.md). 
- `Bot::register_balancer_pool` + `swap` calc delegating to Rust `WeightedMath`/`StableMath`. `PowVersion` (V1/V2) controls `FixedPoint.pow` fast paths.
- `PyBot.register_balancer_pool` + Balancer read getters on `PyLiquidityPool` (vault tokens, weights/amp, swap fee, rate providers, rates).
- `CacheAwareRateProvider`-in-Rust replicating `_cacheTokenRateIfNecessary` exactly (read `getTokenRateCache()`, check expiry, call `getRate()` only if expired). `StaleRateResult` when no rate provider.

**Python.** `BalancerV2Pool` (`src/degenbot/balancer/pools.py`, 435 lines) + `BalancerV2StablePool` (`balancer/stable_pools.py`, 759 lines) wrap `PyLiquidityPool`; `external_update` (with `_state_lock`) + rate providers delegate; `to_hop_state()` returns `BalancerWeightedHop`/`BalancerStableHop` with `swap_fn`; `build_swap_amount()` for N>2 raises without explicit pair (`BalancerPairView`). Math stays Python initially if not ported (`balancer/libraries/` source of truth).

**Builders.** `BalancerBuilder` (collapses `BalancerBuilderBase` helpers) constructs via `PyBot.register_balancer_pool`, wraps.

**Tests.** Balancer weighted/stable calc tests (cross-check vs `contract_reference/`); rate-provider/stale-rate tests; `PowVersion` V1/V2 tests; `_truncated_div` (Solidity truncation-toward-zero) tests; V2 invariant systematic-1-wei-error guard (the AGENTS.md "Using V2 when V1 is needed" ruling).

**Consistency at boundary.** Balancer calc byte-identical. Two new `PoolEntry` variants. `StaleRateResult` preserved. All Balancer tests pass.

**Acceptance.** `cargo`/`ruff`/`ty` green; Balancer weighted + stable tests pass; rate-cache-expiry test passes; `PoolEntry::Balancer*` round-trips.
