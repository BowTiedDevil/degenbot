# Migration guide: PyPool companion seam — scope & migration ordering

Spike delivery for task **C5BAZ3** (epic `Z5CNPB`). Scopes the seam between
the Python pool classes ("companions") and the Rust structural `Pool` handle,
and orders the per-family migration. Informs task `DWOE67` (convert Python pool
companions to delegate through the structural Pool handle), the prerequisite
for the builder port.

## The seam (already partly built)

- **Structural `Pool` handle** — `degenbot-pools/src/pool.rs`. A single handle
  projecting a registered `PoolEntry` into one of three structures
  (`ReservePair`, `ConcentratedLiquidity`, `BalanceVector`), value- and
  protocol-agnostic (identity resolution to DEX name is long-term
  `degenbot-uniswap::deployments`). This is the canonical, standalone-shared
  core shape.
- **`PyLiquidityPool`** — the pyo3 wrapper (`degenbot-python/src/bot/pool.rs`)
  that exposes a registered core pool to Python as a handle. Produced only by
  the bot/builders, which register the pool in `BotState` and hand back the
  handle.
- **Python companions** — the user-facing pool classes
  (`src/degenbot/uniswap/{v2,v3,v4}_liquidity_pool.py`, curve, balancer…) that
  wrap the handle and add domain API.
  **The V3 companion is already the target shape** (ADR-005 slice 8b): a thin
  companion over `self._py_pool` reading state atomically via
  `snapshot_v3()` — no Python-cached pool state of its own.

## Scope of the companion migration (DWOE67)

Convert the remaining companions to delegate through the structural table so
they hold **no** Python-cached primitive pool state; the Rust core
(`BotState` / structural `Pool`) is the single source of truth. Investigation
scope:

1. **V2** (`UniswapV2Pool`) — confirm whether reserve/state reads go through a
   `PyLiquidityPool` handle (`snapshot_v2`) or still cache balances Python-side;
   convert to pure delegation if not.
2. **V4** (`UniswapV4Pool`) — likewise over the `PyLiquidityPool` handle
   (`pool_id`, slot0/liquidity reads).
3. **Curve / Balancer** — currently the least-migrated (their math + fetch is
   still heavily Python / `PyBotIo`); the seam conversion here is coupled to
   the deferred builder families (epic decision D-C) and should NOT block the
   V2/V3/V4 path.

Delete or re-point any `PyBotIo`/choreography calls the companions make once
their data is core-owned, so no standalone-usable logic is stranded Python-side
(AGENTS.md constraint).

## Migration ordering

Aligned with the epic's decision D-C (V2/V3/V4 first; Curve/Balancer follow):

1. **V3** — already a companion over the handle (slice 8b); treat as `done`,
   the template for the rest.
2. **V2 + V4** — convert to full delegation over `PyLiquidityPool` (gated on
   the structural `Pool` + `BotState` state existing, which the builder port
   `3FVZF4` guarantees). These are prerequisites for `4GQWZ4` (retire the
   Python builder orchestration) because they remove the last Python-cached
   state that construction reads.
3. **Curve / Balancer** — deferred to the builder-follow-up (`SSSXG6`); their
   companions stay as-is until the Rust builder covers them, then convert
   using the V2/V3/V4 pattern.

## Constraints

- No Python mirror of Rust-owned state: once a field is core-owned, the
  companion must read it through the handle, not cache a copy.
- The handle is value-agnostic; Dex-name identity resolution stays out of the
  structural `Pool` (long-term `degenbot-uniswap::deployments`) — a companion
  may still present DEX-specific API without duplicating state.
- Tier-1 reachability: companions reach core only through already-reachable
  `PyLiquidityPool` exports; no new stranded logic.

## Validation

- Companion unit tests: reading a field through the handle returns the
  core-owned value (no stale Python copy); a state mutation core-side is
  visible to the companion.
- `just test-python` green for all families after each conversion step.
- Tier-2 dual-driver where a companion crosses the seam (extend
  `rust/crates/degenbot/tests/parity_*.rs` + `tests/standalone_parity/` with
  the shared-fixture shape per ADR-005).

## Files

- `src/degenbot/uniswap/v2_liquidity_pool.py`, `v4_liquidity_pool.py` (convert)
- `src/degenbot/uniswap/v3_liquidity_pool.py` (template, unchanged)
- curve/balancer companions (deferred, unchanged)
- `degenbot-python/src/bot/pool.rs` (`PyLiquidityPool`) as the reached seam
