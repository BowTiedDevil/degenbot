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
   per family stays suffixed and stays paired: a V3 liquidity delta is `i128`
   while a V4 delta is `int256` (ADR-014 D4), so
   `apply_liquidity_update(_by_key)` is honest about which narrowing it applies.
   Faking a family-neutral payload there would hide a truncation.
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
