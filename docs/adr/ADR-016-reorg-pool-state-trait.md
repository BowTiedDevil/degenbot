# ADR-016: ReorgPoolState — Pool-Owned Reorg Rollback (ADR-014 D3 Refinement)

**Status: accepted (decision).** The adoption itself is a candidate slice
(tracked under ergo epic `OCXSHQ`; see SEQUENCING). This ADR records the
*architectural* decision so future architecture reviews do not re-suggest
leaving the reorg dispatchers as per-family duplicated methods on
`BotState`, and so the open harder case (the CL family's `V3RestoreResult`)
resumes with this decision's framing rather than re-deriving it.

This decision **refines ADR-014 D3**, which declined a state-struct trait
for reserve-pair + balance-vector reorg. It does not revisit D1, D2, D4, D5,
or D6 — those stand.

## Context

`BotState` (`rust/crates/degenbot-bot/src/bot_core/mod.rs`, ~6558 lines,
one `impl BotState`) exposes reorg dispatchers duplicated seven ways:

- `<family>_journal_len`, `<family>_discard_before_block`,
  `<family>_restore_before_block` — one set per family
  (V2, Aerodrome, V3, V4, Curve, BalancerWeighted, BalancerStable).

ADR-014 D3 collapsed the *journal-layer* restore algorithm into a single
generic `impl<D: FullStateDelta> ReorgJournal<D>::restore_before_block`
returning `D::RestoreState`. But the per-family **`BotState` dispatchers
survived the slicing** — each variant-extracts the `&mut FamilyPoolState`,
calls the generic journal restore, then **writes the landed-at state into
the struct's own mutable fields** (`state.reserve0 = r0; …` /
`state.balances = balances; …` / `state.sqrt_price_x96 = …`). This
field-write is the residue D3 did not collapse.

A Red/Green spike validated this decision on the three balance-vector
structs (`CurvePoolState`, `BalancerWeightedPoolState`, `BalancerStablePoolState`):

- `rust/crates/degenbot-pools/src/state_history.rs` — the `ReorgPoolState` trait.
- byte-identical impls on all three structs (verified via `diff`;
  only the `impl … for X` line differs).
- `rust/crates/degenbot-pools/tests/reorg_pool_state_trait.rs` — 12 tests
  (landed-at restore, no-op branch, hard-error past genesis, discard)
  over all three siblings.

## Decision

### D1 — `ReorgPoolState` trait on pool state structs.

```rust
pub trait ReorgPoolState {
    fn restore_before_block(&mut self, block: u64) -> Result<(), JournalError>;
    fn discard_before_block(&mut self, block: u64) -> Result<(), JournalError>;
    fn journal_len(&self) -> usize;
}
```

Each pool state struct **owns the field-write** — the "write the landed-at
state into my own mutable fields" step that ADR-014 left on the per-family
`BotState` dispatchers. The trait returns `()` (or `Result<(), JournalError>`)
— **no family-specific restore-return type**. This is the lever ADR-014 D3
did not have: returning `()` dissolves the no-op trap that defeated the
cross-family `PoolFamilyReg`, because every family satisfies one identical
signature with no associated type.

The family-specific field-write (which reserve pair, which balances vector,
which slot0 scalars) lives *inside* each struct's impl — the same category
as ADR-014 D1's `apply_swap` / `apply_liquidity_update` on the state structs.
A caller needing the restored values (the PyO3 wrapper, which must marshal a
tuple to Python) reads the struct's current fields after restore rather than
receiving a typed return — see D3.

### D2 — `BotState` becomes a one-match dispatcher per op.

`BotState` exposes one `restore_pool_before_block` /
`discard_pool_before_block` / `pool_journal_len` instead of the per-family
set. The body matches the `PoolEntry` variant once, yields the `&mut dyn
ReorgPoolState` (or dispatches directly on the inherent impl), calls the
trait method. The seven per-family dispatcher methods delete. The bulk
`restore_all_pools_before_block` (the 7-arm inline match at
`bot_core/mod.rs:2145`) dispatches per-pool through the trait instead of
re-matching family inline.

### D3 — PyO3 wrappers read-after-restore.

The PyO3 wrappers that currently consume the family-specific restore
**return value** (e.g. `Option<Result<(Vec<U256>, u64), JournalError>>` →
Python tuple) instead call the now-`()`-returning restore, then read the
struct's current `balances`/`update_block` via the existing projections
while still holding the write lock, and marshal that to the Python tuple.
Python-visible behavior is preserved exactly (same tuple shape, same field
order), and no new Rust→Python crossing is added (the read happens under
the existing write lock, before release).

The "return the landed values" optimization survives as a **wrapper-level**
concern, not a core-semantics constraint — which is the right layer for it
(FFI convenience, not core behavior).

### D4 — CL family (`V3`/`V4`) adopts the trait (VERDICT: absorb).

`V3BlockDelta`'s restore is a genuinely different algorithm (pops + accumulates
`scalar_priors` and tick priors across the rolled-back range, returning
`V3RestoreResult`). The no-op objection does not bite here either (restore
can still return `()`), and the CL-feasibility spike (ergo `Z76ETG`)
resolved the open question: **Option A — absorb.**

**Survey result.** Every consumer of `V3RestoreResult` is either (a)
absorbable into the struct impl (the core `v3_restore_before_block` /
`v4_restore_before_block` field-write), (b) absorbable into PyO3
read-after-restore (the two CL restore wrappers marshal
`(sqrt_price_x96_before, liquidity_before, tick_before, block)` — all
equivalent to the post-restore struct fields, since the restore writes the
before-values *into* the fields; `tick_priors` is never marshalled across
the FFI), or a discard/count use. The `tick_priors` field is consumed only
internally (the core restore writes `state.tick_data`). No category-(c)
consumer (a caller genuinely needing the typed result) exists.

**Decision.** `V3`/`V4` adopt `ReorgPoolState`; `V3RestoreResult` becomes a
private internal transient the V3/V4 impls consume during the field-write,
then discard; restore returns `()`. The two PyO3 CL restore wrappers
read-after-restore (post-restore fields == the before-values they currently
read off the result). The equivalence is exact.

### D5 — Reserve-pair family (`V2`/`Aerodrome`) gated on `DBISWP`.

`V2PoolState` + `AerodromeV2PoolState` adopt `ReorgPoolState` on the same
pattern (the field-write is `state.reserve0 = r0; state.reserve1 = r1;
state.update_block = blk;`). This is gated on `DBISWP` ("Reserve storage:
U112 in degenbot-pools" — the in-flight `U256 → U112` retype on exactly
`V2PoolState.reserve{0,1}` + `AerodromeV2PoolState.reserve{0,1}`), which is
NOT an ergo task. ADR-014's sequencing already defers the V2/Aerodrome
halves of D1 + D6 to exactly this gate.

## Consequences

- `bot_core/mod.rs`'s `impl BotState` shrinks: the seven sets of
  per-family reorg dispatchers (21 methods) collapse to 3 trait-dispatching
  methods; the bulk `restore_all_pools_before_block` 7-arm inline match
  dispatches through the trait per-pool.
- ADR-003 (single state owner) preserved — `BotState` still owns the
  `HashMap<u64, PoolEntry>`, the buffers, the address index; the trait impls
  operate on `&mut self` over the struct's own fields.
- ADR-014 D1's "push the field-mutating mass onto `impl <Family>PoolState`"
  spirit extends from `apply_swap`/`apply_liquidity_update` to reorg restore.
- The journal-layer generic (`impl<D: FullStateDelta> ReorgJournal<D>`) is
  unchanged — it remains the correct dedup site for the restore *algorithm*;
  D1 here dedups the *field-write dispatch*, one layer up, which the journal
  cannot own (the journal returns `D::RestoreState`; it cannot reach into
  the state struct's fields).
- The `()`-return dissolves the cross-family no-op trap ADR-014 used to
  reject `PoolFamilyReg`. The trait is no-op-free: a Red/Green spike proved
  the three balance-vector impls are byte-identical modulo the struct name.

## Sequencing

Tracking under ergo epic `OCXSHQ` ("ADR-016: ReorgPoolState"):

1. **Balance-vector family** (done, spike + 3-impl witness): `Curve`, `BalancerWeighted`,
   `BalancerStable` — `ReorgPoolState` impl landed, 12 tests pass.
2. **CL-feasibility spike** (`Z76ETG`, DONE): verdict **Option A — absorb**.
   `V3RestoreResult` becomes a private internal transient; the two CL PyO3
   restore wrappers read-after-restore (exact equivalence — post-restore
   fields == the before-values they currently marshal). `tick_priors` never
   crosses the FFI. See D4.
3. **Collapse `BotState` balance-vector dispatchers** (`LDTEMF`): replace the
   nine per-family balance-vector methods with three trait-dispatching ones.
4. **PyO3 FFI read-after-restore** (`5XGSYG`): update the balance-vector
   restore wrappers to read-after-restore.
5. **Reserve-pair family** (`O3AHUW`, gated on `DBISWP`): `V2` + `Aerodrome`
   adopt the trait.
6. **CL family adoption** (post-spike-`Z76ETG`, verdict=A): `V3` + `V4` adopt
   the trait; `V3RestoreResult` goes private.
7. **Final `BotState` collapse** (`YTGXBJ`): the remaining per-family
   dispatchers + the bulk restore dispatch through the trait.

## Why not the alternatives

- **Reopen ADR-014's rejection of the cross-family `PoolFamilyReg` trait** —
  rejected. The cross-family return-type difference (`(U112, U112, u64)` /
  `(Vec<U256>, u64)` / `V3RestoreResult`) is genuine; a uniform trait would
  force no-op stubs on the families whose shape differs. ADR-014's rejection
  stands. This refinement attacks the *within-family* residue, where the
  no-op objection does not reach (the three balance-vector bodies are already
  byte-identical).

- **A state-struct trait with an associated `type RestoreState` (to keep the
  typed return)** — rejected. The associated type cannot be erased across the
  `PoolEntry` sum type, so a `&dyn Reorgable<State=?>` over the variants is
  not formable; the variant match just moves to the getter call site.
  Returning `()` sidesteps this entirely — `&mut dyn ReorgPoolState` is
  object-safe (no associated types, all `&self`/`&mut self` + concrete args).

- **Keep the field-write on `BotState` (status quo, ADR-014 D3 as-planned)** —
  rejected for the within-family case. D3's "residual per-family apply bodies
  too short to justify trait + dyn" was about the `apply` bodies (which D1
  moved onto the structs) and presumed the family-specific return type. With
  the `()`-return lemma (which D3 did not have), the bodies are no longer
  "too short" — they're the pool's own field-mutation logic, the same
  category D1 accepted on the structs. The cross-family rejection still holds.
