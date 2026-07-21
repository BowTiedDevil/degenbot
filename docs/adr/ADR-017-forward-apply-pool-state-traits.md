# ADR-017: Forward-Apply Pool-State Traits (ADR-016 Forward-Apply Twin)

**Status: accepted (decision).** The adoption itself is a candidate slice
(tracked under ergo epic `FORWARD_APPLY`; see SEQUENCING). This ADR records
the *architectural* decision so future architecture reviews do not re-suggest
leaving the forward-apply field-writes as per-family duplicated methods on
`BotState`, and so the CL `apply_swap`/`apply_liquidity_update` twins resume
with this decision's framing rather than re-deriving it.

This decision is the **forward-apply mirror of ADR-016**: where ADR-016
collapsed the *restore* (rollback) field-write into a `ReorgPoolState` trait,
this ADR collapses the *forward-apply* (event-replay) field-write into the
matching apply traits. It does not revisit ADR-016, ADR-014 D1, D2, D3, D5,
or D6 — those stand (D3's "too short to justify a trait" finding is
superseded below where the reserve-pair family is concerned, for the reason
ADR-016 already overturned it on the restore twin).

## Context

After ADR-016 landed `ReorgPoolState` on all seven pool state structs and
collapsed `BotState`'s per-family reorg dispatchers, the *restore* layer is
fully trait-covered. The *forward-apply* layer is not. The two halves of a
pool's field-write lifecycle — apply an event forward, restore on reorg —
are now asymmetric: the restore half lives in the struct's trait impl, the
forward-apply half lives in `BotState` (or as byte-identical inherent twins).
This is the exact residue ADR-016 cleaned up for restore, still sitting for
apply.

Three families, three shapes of the same gap:

### Reserve-pair family (`V2PoolState`, `AerodromeV2PoolState`)

`V2PoolState::apply_sync` (`rust/crates/degenbot-pools/src/v2_state.rs`) and
`AerodromeV2PoolState::apply_sync` (`aerodrome_v2_state.rs`) are
**byte-identical inherent methods** — both push a `V2BlockDelta` (`before`
= pre-sync reserves, `after` = new reserves, at `block_number`), overwrite
`reserve0`/`reserve1`, advance `update_block`. Both already return `()`.
`BotState` still carries two dispatchers (`apply_v2_sync_by_pool_id`,
`apply_aerodrome_sync_by_pool_id`).

### Balance-vector family (`CurvePoolState`, `BalancerWeightedPoolState`, `BalancerStablePoolState`)

There is **no `apply_balance_update` inherent method on the structs at all**.
The three byte-identical bodies live inline in `BotState`:
`apply_curve_balance_update_by_pool_id`, `apply_balancer_weighted_*`, and
`apply_balancer_stable_*` (each: assert arity, push `BalancesBlockDelta`,
overwrite `balances`, advance `update_block`). ADR-014 D1's relocation of
`apply_*` onto the state structs never reached this family's forward apply;
the same three structs the other agent just gave `ReorgPoolState` impls to
have their forward-apply field-write stranded in `BotState`.

### Concentrated-liquidity family (`V3PoolState`, `V4PoolState`)

`V3PoolState::apply_swap` / `V4PoolState::apply_swap` are byte-identical
twins (V4's doc-string still says "the CL mut trait (ADR-014 D2) will dedup
these twins"); same for `V3PoolState::apply_liquidity_update` /
`V4PoolState::apply_liquidity_update`. Both return `()`. The
`ConcentratedLiquidityPoolMut` trait (ADR-014 D2b) exists
(`rust/crates/degenbot-pools/src/registry.rs`) but currently carries only
`replace_tick_data` — the apply twins are not members and remain as
inherent methods.

Separately, `BotState::merge_tick_word` has a byte-identical V3/V4 twin arm
(insert ticks, insert word, invalidate cache, return `true`) — a missed
sibling of `replace_tick_data` on the same trait, simpler (no
`tick_spacing` lookup) and already returning `bool` uniformly.

And `apply_backfill_buffer_v3` / `apply_pump_buffer_v3` /
`apply_backfill_buffer_v4` / `apply_pump_buffer_v4` — four `BotState` drain
loops — **re-inline the full journaled `apply_liquidity_update` body**
(tick-prior capture → `apply_liquidity_to_tick_range` →
`push_delta(V3BlockDelta{scalar_priors:None,..})` → advance `update_block` →
`invalidate_tick_range_cache`) instead of calling the existing inherent
method. The `apply_*_liquidity_update_by_pool_id` dispatchers *do* delegate;
the drains don't. That is six total copies of the same tick-prior-and-delta
logic (2 inherent + 4 drain-inlined).

## Decision

### D1 — `BalanceVectorPoolState` trait on the three balance-vector structs.

```rust
pub trait BalanceVectorPoolState: ReorgPoolState {
    /// Apply a balances-vector update (a Curve `Exchange` or a Balancer Vault
    /// `PoolBalanceChanged` event), capturing the prior balances into the
    /// reorg journal and overwriting the live `balances` + `update_block`.
    fn apply_balance_update(&mut self, balances: Vec<U256>, block_number: u64);
}
```

Returns `()` — the `Option<u64>` is a `BotState`-level variant-dispatch
concern (wrong-family ⇒ `None`), not a trait concern; identical to how
ADR-016 D2 keeps `restore_pool_before_block` returning `Option<Result<(),_>>`
on `BotState` while the trait returns `Result<(),_>`. The arity `assert!`
moves into the impl body. The three impls are byte-identical modulo the
struct name — verifiable by `diff`, same as ADR-016 certified the three
restore impls.

`BotState` gains one `apply_balance_update_by_pool_id` dispatcher matching
`Curve | BalancerWeighted | BalancerStable`; the three per-family methods
delete.

### D2 — Extend `ConcentratedLiquidityPoolMut` with the two apply twins + `merge_tick_word`.

```rust
pub trait ConcentratedLiquidityPoolMut: ConcentratedLiquidityPool {
    fn replace_tick_data(/* existing */) -> bool;                                   // existing
    fn apply_swap(&mut self, sqrt_price_x96: U256, liquidity: u128, tick: i32,
                  block_number: u64, tick_priors: &[(i32, TickInfo)]);
    fn apply_liquidity_update(&mut self, tick_lower: i32, tick_upper: i32,
                               liquidity_delta: i128, block_number: u64);
    fn merge_tick_word(&mut self, fetched: TickWordFetcher::FetchedTickWord) -> bool;
}
```

All already return `()` / `bool`. The V3 and V4 impls of all three are
byte-identical identical to one another (no identity-agnostic massage needed
beyond what `replace_tick_data` already established: the trait takes no
`tick_spacing`, `fee`/`tick_spacing` live on the identity slice and are read
by the caller before dispatch — and notably none of these three methods needs
even that). ADR-014 D2's two-adapter rule is preserved; V3 and V4 stay
distinct types, two adapters behind one interface.

### D3 — `ReservePairPoolState` trait on V2 + Aerodrome.

```rust
pub trait ReservePairPoolState: ReorgPoolState {
    /// Apply a `Sync(uint112, uint112)` event, capturing the prior reserves
    /// into the reorg journal and overwriting `reserve0`/`reserve1`/`update_block`.
    fn apply_sync(&mut self, reserve0: U112, reserve1: U112, block_number: u64);
}
```

Returns `()`. The two impls are byte-identical modulo the struct name.
`BotState` gains one `apply_sync_by_pool_id` dispatcher matching
`V2 | AerodromeV2`; the two per-family methods delete.

### D4 — Reserve-pair "too short" (ADR-014 D3) is overturned, same as ADR-016 overturned it on restore.

ADR-014 D3 rejected a reserve-pair + balance-vector *restore* state-struct
trait as "too short to justify a trait + `dyn`." ADR-016 overturned that for
the restore twin (the `()`-return dissolves the no-op trap; the bodies are
the pool's own field-mutation logic; the `BotState` dispatcher collapse is a
real gain). The same reasoning applies identically to the forward-apply
twin: `V2PoolState::apply_sync` and `AerodromeV2PoolState::apply_sync` are
byte-identical, already return `()`, and the `BotState` dispatcher collapse
(2→1) is the forward-apply mirror of ADR-016 D2. D3's "too short" finding is
overturned for the reserve-pair *forward apply* for the same reason ADR-016
overturned it for the reserve-pair *restore*. (The balance-vector forward
apply was never rejected — D3's discussion predates the relocation of the
apply bodies, so there was nothing to reject; D1 here is greenfield.)

### D5 — CL buffer-drain methods delegate to the trait.

Once `apply_liquidity_update` is on `ConcentratedLiquidityPoolMut` (D2),
the four drain loops (`apply_backfill_buffer_v3`, `apply_pump_buffer_v3`,
`apply_backfill_buffer_v4`, `apply_pump_buffer_v4`) collapse to a
`state.apply_liquidity_update(...)` call in their loop body. The V4
`I256→i128` narrowing stays at the drain site (ADR-014 D4). This deletes four
inlined copies of the tick-prior + delta body.

### D6 — `BotState` apply dispatchers collapse.

`BotState` gets three trait-dispatching apply methods (one per structural
family) — `apply_sync_by_pool_id` (matching `V2 | AerodromeV2`), the existing
`apply_balance_update_by_pool_id` (matching
`Curve | BalancerWeighted | BalancerStable`, replacing the three per-family
methods), and the CL apply dispatch routes through
`PoolEntry::as_cl_mut()` (`apply_*_v3/v4_swap_by_pool_id` + the family
dispatchers `apply_swap_by_pool_id` / `apply_liquidity_update_by_pool_id`
already exist; they gain a one-line `&dyn ConcentratedLiquidityPoolMut`
delegation body in their V3/V4 branches instead of two separate family
methods).

### D7 — `PoolEntry` CL projections.

Add `PoolEntry::as_cl_mut(&mut self) -> Option<&mut dyn ConcentratedLiquidityPoolMut>`
matching V3/V4 — the apply-twin of `as_reorg_state_mut` (ADR-016) and the
mut counterpart of the existing `as_cl` projection used by
`get_v3_or_v4_pool`. The balance-vector and reserve-pair apply dispatchers
dispatch directly on the variant match (the projected `&dyn` is formable but
not load-bearing, exactly as ADR-014 D2 accepted for the CL read trait);
the trait is the dedup site, the match is the call-site seam.

## Consequences

- `bot_core/mod.rs`'s `impl BotState` shrinks further: three per-family
  balance-vector apply methods + two per-family reserve-pair apply methods +
  the V3/V4 `apply_*_swap_by_pool_id` / `apply_*_liquidity_update_by_pool_id`
  family pair collapse to trait-dispatching methods; the four drain loops
  lose ~25 lines each of inlined body.
- The forward-apply + restore layers are now symmetric: each struct owns
  both halves of its field-write lifecycle behind a within-family trait.
  `BotState` holds the registry, the buffers, the cross-family dispatch; it
  no longer holds any pool field-write.
- ADR-003 (single state owner) preserved — `BotState` still owns the
  `HashMap<u64, PoolEntry>`, the buffers, the address index; the trait impls
  operate on `&mut self` over the struct's own fields.
- ADR-014 D2's latent value lands: future CL mutators join free, and the
  standalone consumer reaches a uniform `ConcentratedLiquidityPoolMut` apply
  surface for both V3 and V4.
- The `ReorgPoolState` supertrait bound on `BalanceVectorPoolState` and
  `ReservePairPoolState` ties the two halves together at the type level: a
  pool that owns its restore owns its apply, and vice versa.

## Sequencing

Tracking under ergo epic `FORWARD_APPLY` ("ADR-017: Forward-apply pool-state
traits"):

1. **`merge_tick_word` → `ConcentratedLiquidityPoolMut`** — the simplest
   member (no `tick_spacing` lookup, already returns `bool`); goes first as
   a no-risk warm-up that proves the trait-extension seam.
2. **CL `apply_swap` / `apply_liquidity_update` → `ConcentratedLiquidityPoolMut`**
   (ADR-014 D2 open slice) — move the two byte-identical inherent twins onto
   the trait; V3 and V4 are two adapters behind one interface.
3. **CL buffer-drain delegation** (D5) — collapses the four inlined drain
   bodies to `state.apply_liquidity_update(...)`; gates on (2).
4. **Balance-vector `BalanceVectorPoolState` trait + dispatcher collapse**
   (D1) — the headline: the three byte-identical bodies finally leave
   `BotState`; the struct owns its forward-apply write. Independent of (2)/(3).
5. **Reserve-pair `ReservePairPoolState` trait + dispatcher collapse**
   (D3 + D4) — V2 + Aerodrome adopt the trait; gate cleared (`DBISWP` landed:
   both structs carry `reserve{0,1}: U112`). Independent of (2)/(3)/(4).

(1)/(2)/(3) form a CL chain; (4) and (5) are independent and can proceed in
parallel with the CL chain.

## Why not the alternatives

- **Leave the forward-apply as inherent methods / inline bodies (status quo)**
  — rejected. The restore twin just collapsed (ADR-016); leaving apply as the
  mirror-residue keeps the two halves asymmetric and leaves the
  highest-value dedup (the three balance-vector bodies, byte-identical, in
  `BotState`) undone. The `()`-return is already in place on 5 of the 6
  twins; the lemma bites cleanly.

- **A single cross-family `trait ApplyPoolState`** — rejected. ADR-014's
  rejection of cross-family `PoolFamilyReg` stands and the `()`-return lemma
  does not rescue it: the three families differ in *field shape*
  (`U112×2` / `Vec<U256>` / slot0+`tick_data`), *delta shape* (full-state
  `V2BlockDelta` / full-state `BalancesBlockDelta` / partial-prior
  `V3BlockDelta`), and *apply algorithm* (two reserves / N balances /
  scalar+tick-prior capture). The lemma dissolves the *within-family*
  no-op trap; it cannot paper over families whose field-writes have nothing
  in common. Each trait here is within-family, mirroring ADR-016's
  within-family `ReorgPoolState` boundary.

- **Fold the apply into `ReorgPoolState` itself** — rejected. Restore and
  apply are opposite directions of the same lifecycle but have genuinely
  different signatures (restore takes a `block: u64` and returns
  `Result<(), JournalError>`; apply takes family-specific event args and
  returns `()`). Merging them would re-introduce the cross-family
  signature-mismatch ADR-016 dissolved. Separate within-family traits, with
  `ReorgPoolState` as the shared supertrait bound, keeps each seam honest.

- **Defer the forward-apply traits behind the `PyPool` /
  `proposed-pool-interface.md` redesign** — rejected. That track is a
  *cross-family* `Structure`-enumerated handle redesign (a `Pool` wrapper over
  `PoolEntry` + structural views); it is orthogonal to the within-family
  return-type lemma. The two tracks are independent and the forward-apply
  dedup should not be gated on the cross-family handle landing. The
  `PyBalanceVectorView` / `PyReservePairView` wrappers will *consume* the
  trait impls cleanly; landing the traits first is a strict prerequisite
  for the handle rewrite, not a competitor.
