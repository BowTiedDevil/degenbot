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
3. **Family-typed payloads keep their suffix.** A verb whose *payload* narrows
   per family stays suffixed and stays paired, so
   `apply_liquidity_update(_by_key)` is honest about which conversion it applies.
   Faking a family-neutral payload there would hide a narrowing.

   The contract types are not the same, which is the reason:

   - **v3-core** (`contracts/UniswapV3Pool.sol`) takes liquidity as an
     **unsigned** `uint128 amount` on `mint` and `burn` — direction is which
     event fired, not the sign — and its internal
     `ModifyPositionParams.liquidityDelta` is `int128`.
   - **v4-core** (`src/types/PoolOperation.sol`, `src/interfaces/IPoolManager.sol`)
     carries a **signed** `int256 liquidityDelta` through both
     `modifyLiquidity` and the `ModifyLiquidity` event, and narrows to `int128`
     only inside `src/libraries/Pool.sol`.

   So the wire types differ (`uint128` vs `int256`) and converge on `int128`
   internally. Our representation follows each wire type: the V3 decoder keeps
   `u128` and the dispatcher converts with `i128::try_from(..).ok()?` (checked —
   a value above `i128::MAX` drops the event rather than truncating), while the
   V4 path keeps `I256` until the drain→apply seam (ADR-014 D4).
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
- `log_dispatcher` keeps its own event enum and resolves through the capability
  directly. Unifying it is a hot-path change that deserves its own commit and a
  bench comparison, not a rider on a rename.
- Three doors that looked like twins were left suffixed *on purpose*
  (`apply_liquidity_update_by_key`, `buffer_backfill_liquidity_update_by_key`),
  and this ADR is the record of why.
