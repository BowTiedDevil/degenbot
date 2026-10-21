# ADR-082: Family lives in the value, not the name

## Status

Accepted.

## Context

A concentrated-liquidity pool is two designs behind one word. The state owner
grew a verb per address shape, and each rename round left another convention
behind: `_v3`/`_v4` suffixes, `_by_key` suffixes, `pool_id`-keyed twins, and
family-free names — often for the same effect. `PoolLookup` (ADR-081) gave the
verbs one key; this ADR fixes the naming rule so the surface cannot grow a
second convention again.

## Decision

**An effect exists once per effect, and the family travels inside the value.**
Concretely:

1. **Effects are family-free.** A verb that acts on a pool takes `PoolLookup`
   (or the pool-typed payload) and is named for what it does:
   `sync_pool_state`, `unregister_pool`, `route_event`,
   `begin_registration_quarantine`, `mark_registration_live`, `snapshot_seed`.
2. **Views may keep a family word.** A read that cannot exist for the other
   family — a V4 registry iterator, a V4-only door — may say so in its name,
   because there is no counterpart to be confused with.
3. **A payload unifies where the wire bounds coincide, and the narrowing happens
   once.** `apply_liquidity_update` and `buffer_backfill_liquidity_update` take a
   family-neutral `PoolLiquidityUpdate` (`i128`) for both key shapes. The
   licence is a *behavioural* bound that both contracts impose, not a shared ABI
   type:

   - **v3-core** (`contracts/UniswapV3Pool.sol`) takes liquidity as an
     **unsigned** `uint128 amount` on `mint` and `burn` — direction is which
     event fired, not the sign — and its internal
     `ModifyPositionParams.liquidityDelta` is `int128`.
   - **v4-core** (`src/types/PoolOperation.sol`, `src/interfaces/IPoolManager.sol`)
     carries a **signed** `int256 liquidityDelta` through both
     `modifyLiquidity` and the `ModifyLiquidity` event, and narrows to `int128`
     only inside `src/libraries/Pool.sol`.

   So the wire types differ (`uint128` vs `int256`) and converge on `int128`
   internally. Both call `toInt128()` before acting and **revert** outside that
   range, so no event from a canonical pool or manager can carry a larger delta —
   which is what makes `int128` the honest shared payload. A forged log, or a
   patched emitter, still can: the V3 decoder keeps its checked
   `i128::try_from(..).ok()?` and the V4 wire constructor
   (`PoolLiquidityUpdate::from_v4_wire`) is fallible for the same reason.

   The family-typed payloads (`BufferedV3PoolEvent::Liquidity` /
   `BufferedV4PoolEvent::Liquidity`) are built inside the door from the
   family-free value: one narrowing site, and family knowledge stays where the
   signatures genuinely differ. `BufferedV4LiquidityUpdate` carries that same
   `i128` bound (ADR-014 D4), so the door widens nothing and the buffer holds
   the contract's own width; the only fallible narrowing is the decoder's,
   where the `int256` wire value arrives.
4. **Refusals are typed, not dropped.** Where a payload carries a family
   (`BufferedPoolEvent`, `PoolStateUpdate`) and the key names another, the door
   returns `PoolRefusal::FamilyMismatch`. It must not borrow `NoOpReason`,
   whose vocabulary documents *sanctioned* drops with stated trust assumptions.
5. **Identities are per lifecycle stage**, and none is a mirror of another:
   `PoolLookup` (a registered/addressed pool), `PreBuildIdentity` (what a
   negative memo keys on before a build resolves the manager), `db::PoolKey`
   (the persistence contract, V4 keyed by hex text for Python's dict keys), and
   `session_registry::PoolIdentity` (a session's object, including families
   `PoolLookup` cannot express). Every one projects to `RegisteredPoolFamily`,
   the single tag vocabulary.

## Consequences

- The naming rule is enforced, not documented: the pool-key census fails on any
  surviving `_by_key` twin and on any retired twin whose door vanished, over
  both the capability source and the state owner.
- `pool_id_by_key` survives as the one public V4 resolver beside
  `PoolLookup::resolve`: the Python shell and the standalone example hold
  `(manager, id)` without a lookup at hand, and wrapping one there would add a
  type without removing a name.
- `log_dispatcher` was unified after a bench comparison: its four V3/V4 arms
  build `(PoolLookup, BufferedPoolEvent)` and go through `route_event` like
  every other caller (`V2Sync` keeps its own scalar door — V2 has no
  `PoolLookup` and no table verdict). The route-event census in
  `tests/route_event_census.rs` pins one asker per capability method, with
  negative probes for a second caller, a formatter-split call, and a silent
  door.
- The two liquidity doors that were left suffixed here were unified once the
  contract bound was verified: `apply_liquidity_update(PoolLookup, PoolLiquidityUpdate)`
  and `buffer_backfill_liquidity_update(PoolLookup, PoolLiquidityUpdate)`, both
  routing through `route_event` so the phase gate is not optional for them.
- `apply_liquidity_update_by_pool_id` stays a **direct-state** door and does not
  delegate to that one. Its callers (the Python companion applying an event it
  already gated, and the projection-memoization seam) assert that a registered
  pool's state moves immediately; the routing table has no "apply now while
  quarantined" cell, so delegating would stage the delta into a buffer those
  callers never drain. Two contracts, two doors — the divergence is documented
  at the door rather than implied by which one a caller happened to find.
