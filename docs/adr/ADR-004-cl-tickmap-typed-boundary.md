# ADR-004: Typed TickMap boundary for CL verifier + liquidity-apply seam

**Status: accepted.** Follow-up to ADR-003 (which left the per-pool CL state struct flat).
Decided by survey (TODO-6177b602, Slice 0): a four-way classification of every Rust
read/write site of `V3PoolState`/`V4PoolState` slots found that the {Slot0 Head / Tick
Bookkeeping Map} deferral trigger condition was already met. Implementation lands via
TDD vertical slices; V3 first, mirror to V4.

## Context

ADR-003 consolidated V2/V3/V4 state into `BotCore` as flat `pub`-field structs
(`V3PoolState` / `V4PoolState`). It named a conceptual two-part split *within* each CL
pool's state — the **slot0 head** (`sqrt_price_x96`, `tick`, active `liquidity` — high
churn, every Swap) vs. the **tick bookkeeping map** (`tick_data: HashMap<i32, TickInfo>`
— low churn, only Mint/Burn V3 / `ModifyLiquidity` V4) — and recorded it in
`rust/CONTEXT.md` under the term {Slot0 Head / Tick Bookkeeping Map}, **held as a
non-structural distinction** until "the first caller that needs to verify or restore
*only* the map (or *only* the head) and resents passing the whole pool + recovering the
rule from a comment."

A clean-slate survey (`rg` over all of `rust/src/`, 148 V3 + 184 V4 field accesses → 22
function-level surfaces) found that trigger condition is met today:

| classification | count | sites |
|---|---|---|
| `both` (genuinely wants both halves) | 12 | `v3_simulate_swap`, `v4_simulate_swap`, `build_int_v3_sequence`, `build_int_v4_sequence`, `apply_v3_swap`/`apply_v4_swap`, `update_v3_pool`, `v3_restore_before_block`/`v4_restore_before_block`, `restore_all_pools_before_block`, `calculate_tokens_out`/`calculate_tokens_in`, the `get_v3_pool`/`get_v4_pool` accessors, the V3/V4 path resolver in `solver_dispatch.rs` |
| `ticks-only` (no slot0 read at all) | 3 | `verify_v3_liquidity_map`, `verify_v4_liquidity_map`, `debug_v3_tick_data` |
| `takes-whole-but-wants-one` ← the paying signal | 6 | `verify_v3_pool`, `verify_v4_pool`, `verify_v3_pools`, `verify_v4_pools`, `apply_v3_liquidity_update`, `apply_v4_liquidity_update` |
| `slot0-only` | **0** | — |

### Decisive facts

1. **Zero `slot0-only` consumers exist.** The slot0 head is never read in isolation; the
   closest is `v3_simulate_swap` reading slot0 + ticks in lockstep, which is `both`.
   Splitting `Slot0Head` into its own type would earn nothing — no caller wants only the
   head.

2. **Six `takes-whole-but-wants-one` sites pay the seam today.** They want only the tick
   map and recover the "don't read slot0" rule from comments:
   - `verify_v3_pool` / `verify_v4_pool` (`liquidity_verifier.rs:187`, `:410`) take
     `&V3PoolState`/`&V4PoolState` and read `pool.tick` (a slot0 scalar) *only* to seed
     the ±2-word bitmap scan around the current tick during bitmap discovery — it is not
     verification content. The module doc carries the rule as prose: *"Mutable scalar
     state like sqrtPriceX96, tick, and liquidity changes on every swap and is NOT
     verified here."*
   - `apply_v3_liquidity_update` / `apply_v4_liquidity_update` (`mod.rs:446`, `:1206`)
     write only `tick_data` via `apply_liquidity_to_tick_range` (the source comment
     literally says: *"Mint/Burn mutate tick_data only, NOT the active liquidity
     scalar"*) but the monolithic `V3BlockDelta` shape *forces* them to read slot0
     priors (`sqrt_price_x96`, `liquidity`, `tick`) to journal them unchanged. The
     journal shape, not the apply logic, induces the whole-pool read.

3. **A typed-boundary precedent already ships 200 lines above the offending sites.**
   `verify_v3_liquidity_map` / `verify_v4_liquidity_map` (`liquidity_verifier.rs:103`,
   `:136`) — the snapshot-block variants — already take `&HashMap<i32, TickInfo>` +
   `Address` + block. They take the tick map as a typed arg and do not take
   `&V3PoolState` at all. The candidate chosen here is not new architecture; it is
   completing a conversion that was started and paused.

### Bonus structural findings (carry into the doc pass)

The survey surfaced three stale-documentation issues, fixed in the same Slice 1
context.md pass:

- **S1 — `{LiquidityMap}` and `{PyBotCore}` CONTEXT.md terms describe a generic
  `LiquidityMap<V3PoolState>`/`LiquidityMap<V4PoolState>` that was never extracted.**
  ADR-003 and the `{BotCore}` term both say: *"The standalone `LiquidityMap` generic was
  NOT extracted: the inline-`PoolEntry` + `BotCore::apply_*` pattern suited V2/V3/V4."*
  But these two terms persisted unchanged. Reality: `BotCore` holds
  `pools: HashMap<u64, PoolEntry>` with `PoolEntry::V3(V3PoolState)` / `V4(V4PoolState)`,
  and verification is the free functions in `liquidity_verifier.rs`.

- **S2 — `{PyPoolCache}` describes deleted code.** `rust/CONTEXT.md` still documents a
  `parking_lot::Mutex<LruCache<u64, IntHopState>>` 10K-entry LRU keyed by pool ID, but
  exhaustive `rg` returns zero matches. ADR-003 "Legacy solver path retirement: delete,
  not migrate" explicitly deleted `RustPoolCache`/`RustIntHopState`/`RustArbResult` PyO3
  classes. **The actual memoization seam is the per-pool
  `cached_tick_ranges: parking_lot::Mutex<TickRangeCache>` field on
  `V3PoolState`/`V4PoolState`**, consumed by `build_int_v3_sequence`/`build_int_v4_sequence`
  and invalidated on every `apply_*`. `{PyPoolCache}` must be rewritten to name what
  was removed (matching the {V2Snapshot} / {Dual-Orientation Registration} removed-term
  pattern); `{IntHopState}` must drop the "stored in `PyPoolCache`" framing.

- **S3 — reasoned-analysis re-anchoring.** The TODO-6177b602 framing's reasoned-analysis
  argument was anchored to deleted `PyPoolCache`. The real argument is below (see
  "Reasoned analysis — re-anchored to the live mechanism").

## Decision

**Adopt candidate (β) — typed TickMap boundary.** State structs stay flat; a new trait
narrows the verifier + liquidity-apply views so the type — not a doc comment — carries
the "verify/mutate the tick map, do not touch the slot0 scalars" rule.

```rust
pub trait TickMap {
    fn address(&self) -> Address;          // immutable — RPC target
    fn tick_spacing(&self) -> i32;          // immutable — bitmap word compression
    fn active_tick(&self) -> i32;           // slot0 scalar, read-only — ±2 word scan only
    fn tick_data(&self) -> &HashMap<i32, TickInfo>;
}

impl TickMap for V3PoolState { /* blanket over existing fields */ }
impl TickMap for V4PoolState { /* blanket over existing fields */ }
```

- `verify_v3_pool(provider, pool: &impl TickMap, block_number)` and the V4 mirror.
- `apply_v3_liquidity_update` / `apply_v4_liquidity_update` take a `&mut` view (a
  `TickMapMut` trait exposing `tick_data_mut`); same shape, writeable variant.
- All 12 `both` consumers (`v3_simulate_swap`, `v4_simulate_swap`,
  `build_int_v3_sequence`, `build_int_v4_sequence`, `apply_v3_swap`, `apply_v4_swap`,
  restore, `calculate_tokens_*`, accessors, the path resolver) are **untouched** — they
  keep taking `&V3PoolState`/`&V4PoolState`, exactly as Python's `_v3_swap` and the
  simulator kept the flat state.

### Journal-shape alignment (the contained follow-on)

The trait conversion surfaces a latent `V3BlockDelta` wart: today every Mint/Burn/
`ModifyLiquidity` delta journals the slot0 priors *unchanged* because the delta shape
requires them. Under the `TickMap` trait the apply path cannot "accidentally" journal
slot0 priors it didn't change (the trait exposes no slot0 setters). The clean fix is to
make the scalar priors `Option<ScalarPriors>` in `V3BlockDelta` (`None` for tick-only
events), which also shrinks tick-event journal storage (currently redundant). This
refactor is **contained to `state_history.rs` + the four `apply_*` call sites** that
build `V3BlockDelta`s. If an implementation review finds it spills beyond that, fall
back to candidate (γ) (see Alternatives) and revisit the trait conversion as a follow-up.

## Consequences

- The rule "verify/mutate the tick map, do not touch the slot0 scalars" is now carried
  by the type system on the six paying sites, not by a module doc comment. A future
  implementer who tries to read `sqrt_price_x96`/`liquidity` from a `&impl TickMap`
  simply lacks the method.
- **`V3BlockDelta` becomes two-concept-precise:** scalar priors are `Option`-gated, tick
  priors stay as `Vec<(i32, TickBefore)>`. `restore_before_block` is unchanged in
  semantics (two distinct steps: scalars then tick priors) but the scalar step becomes a
  no-op for tick-only events (the prior was `None`).
- **V4 parity is trivial and mandatory.** `V3PoolState` and `V4PoolState` both impl
  `TickMap` with identical method bodies (modulo the immutable-source field — V4 reads
  `pool_key.tick_spacing`). The four V3/V4 mirror sites (`verify_v3/v4_pool`,
  `apply_v3/v4_liquidity_update`) convert line-for-line. No asymmetry lands.
- **The {Pool's authority over its own math} principle (ADR-003) is preserved.**
  `v3_simulate_swap`/`v4_simulate_swap`/`build_int_*_sequence`/`calculate_tokens_*` keep
  reading the whole state by reference; the trait only narrows the verifier + apply
  views. The pool still owns its swap math; the engine still reads by reference.
- **Optimizer-cache finding (resolves the Q6 deferral):** `PyPoolCache`/`RustPoolCache`
  do not exist — deleted under ADR-003. The {PyPoolCache} CONTEXT.md term is stale and
  must be rewritten to name what was removed (Slice 1 fixes this). The actual
  memoization seam — the per-pool `cached_tick_ranges: parking_lot::Mutex<TickRangeCache>`
  field — **survives unchanged.** It is not in scope for this ADR's code change; it lives
  inside `V3PoolState`/`V4PoolState` and reads both halves (`tick_data` + `liquidity` +
  `tick`), so it stays attached to the flat struct exactly as today.
- **Reasoned-analysis re-anchoring** (see below) replaces the deleted-`PyPoolCache`
  argument the survey found in the TODO-6177b602 framing.

## Reasoned analysis — re-anchored to the live mechanism (no benchmarking)

Per the project's ruling (Q5: reasoned analysis suffices, no criterion benches stand
up). The TODO-6177b602 framing's argument was anchored to the deleted `PyPoolCache`
("IntHopState / PyPoolCache pre-convert slot0 fields to U512 at construction, so the
inner solve loop does not dereference through the state struct"). That object is gone.
The real argument for "the typed boundary does not hurt the hot loop" is:

1. **The trait conversion only touches verifier + liquidity-apply views.** The
   lockstep hot-loop readers (`v3_simulate_swap` / `v4_simulate_swap`,
   `build_int_v3_sequence` / `build_int_v4_sequence`, the path resolver
   `solver_dispatch.rs:332-348`) keep taking `&V3PoolState`/`&V4PoolState` directly —
   the trait is not in their signature. There is no extra indirection on the hot path.

2. **The solve hot loop does not dereference `V3PoolState` at all.** `solver_dispatch.rs`
   calls `pool_state.build_int_v3_sequence(zfo, 10)` once per `solve_dirty` (per
   dirty-path burst, coalesced), producing an `IntV3TickRangeSequence`; the V3/V4 Mobius
   solver (`mobius_v3_int.rs`) then operates on `IntV3TickRangeHop` fields — never on
   `V3PoolState`/`V4PoolState`. The per-tick-crossing inner loop in the solver uses
   pre-built integer hop state, not state-struct dereferences.

3. **The only state-struct dereference in the simulation path is `v3_simulate_swap`/
   `v4_simulate_swap` themselves**, called from the single-pool-query surface
   (`calculate_tokens_out`/`calculate_tokens_in`, the PyO3 single-pool-query path) —
   **not** the solver hot loop. These are unchanged by the trait conversion (they take
   `&V3PoolState`/`&V4PoolState` flat, as today).

4. **The per-pool memoization seam (`cached_tick_ranges`)** keeps the `build_int_*`
   call O(1) on cache hits; invalidated on every `apply_*`. The trait conversion does
   not touch it (it lives inside the state struct, attached to whichever code reads
   both halves).

5. **Python precedent.** Python's `_v3_swap` already runs the tick-crossing loop through
   a typed snapshot (`LiquidityMapSnapshot`) + separate scalar args in production, and
   the survey confirmed it ships. The typed-boundary shape has a working existence
   proof; the Rust conversion is more conservative than Python's (Rust leaves the
   simulator signature flat and only narrows the verifier/apply views).

Benches only stand up if this reasoning is later shown wrong by a regression.

## Alternatives

### (α) Full split — `V3PoolState`/`V4PoolState` gain `slot0: Slot0Head` + `ticks: TickBookkeepingMap` sub-structs

**Rejected by survey evidence.** Every `slot0-only` consumer count is zero. Splitting
`Slot0Head` into its own type earns nothing — no caller reads the head without also
reading ticks. The full-split also forces the lockstep consumers (`v3_simulate_swap`,
`v4_simulate_swap`, `build_int_*_sequence`, `calculate_tokens_*`, the path resolver —
12 sites) to thread `state.slot0.sqrt_price_x96` + `state.ticks.tick_data` instead of
flat field access, at HIGH migration cost, for zero semantic gain at those sites. The
six `takes-whole-but-wants-one` paying sites would still need a trait/view to take "only
the tick half" (the full split alone doesn't give them a typed-narrow argument; they'd
take `&state.ticks` instead of `&state`, which is better than today but still couples the
verifier to the full sub-struct). The `Slot0Head` half of the split is dead weight with
no consumer.

The CONTEXT.md hold reason ("`v3_simulate_swap` reads slot0 + tick_data in lockstep so
they are one deep module, not two shallow ones") was correct as a *rejection of the
full-split reading*. It was incorrect as a *rejection of any typed boundary* — the
survey found the paying boundary is one-directional (tick-side only), and a typed
boundary on that side does not rupture the lockstep simulator.

### (α-snapshot) `LiquidityMapSnapshot`-style frozen value threaded into `v3_simulate_swap` / `v4_simulate_swap` (Python's `_v3_swap` shape)

**Rejected by survey evidence.** A snapshot is the wrong shape for the six paying sites:
the verifier needs live references (it compares against on-chain RPC, not a frozen
copy), and the apply path *mutates* the tick map. Snapshot-per-call only satisfies the
three simulator-call sites (#2, #4, #20) that genuinely take both halves, where it
trades one arg for three with no semantic gain. Rejected.

### (γ) Hold — strengthen the {Slot0 Head} term to a firm ruling; no code change

**Demoted to fallback.** The survey found a paying consumer and a shipping precedent,
so holding is no longer the neutral default the CONTEXT.md term framed it as — it is a
positive decision to leave six `takes-whole-but-wants-one` sites recovering the rule
from comments, when a 1-trait fix exists with a shipping precedent 200 lines away. Hold
is only justifiable if the `V3BlockDelta` `Option<ScalarPriors>` refactor (see
"Journal-shape alignment") is judged too risky for the per-pool seam before the
migration; the argument against is that the refactor is independently desirable (shrinks
Mint/Burn/`ModifyLiquidity` journals — currently redundant slot0 priors) and contained
to `state_history.rs` + four call sites. Recorded as the fallback if a Slice-2
implementation review finds the journal refactor spills beyond that.

## Python prior art (study only — not classified in the survey)

Inspected `src/degenbot/uniswap/v3_liquidity_pool.py`, `concentrated/liquidity_map.py`,
`concentrated/state_manager.py`, `v3_pool_state.py`. Findings:

- **Python did NOT solve the seam by splitting its state struct.** `UniswapV3PoolState`
  (built via `__value__`) carries `liquidity, sqrt_price_x96, tick, tick_bitmap,
  tick_data, block` as flat sibling fields — identical to Rust's flat layout. This ADR
  mirrors that conservative choice on the Rust side (state structs stay flat).
- **The tick half IS its own type in Python**, via `LiquidityMapSnapshot`
  (`concentrated/liquidity_map.py`, frozen/slots dataclass: `tick_data, tick_bitmap,
  tick_spacing, sparse`), duck-typed for V3 *and* V4 through the `_HasTickData` /
  `_HasPoolLiquidityMap` protocols. The Rust `TickMap` trait is the direct Rust
  counterpart of Python's `_HasTickData` protocol — same move, static-typed idiom.
- **The simulation seam consumes the two halves separately.** Python's
  `_v3_swap(snapshot: LiquidityMapSnapshot, *, liquidity_start, sqrt_price_x96_start,
  tick_start, …)` takes the tick half as a typed snapshot and the scalars as separate
  args. The Rust Rust code *did not make this move* on the simulator (candidate α-
  snapshot was rejected — see Alternatives); it makes the narrower move (candidate β) on
  the verifier/apply seam only.
- **Mutation paths split at the method level via `dataclasses.replace`:**
  `external_update()` → slot0 scalars; `update_liquidity_map()` / `update_tick_data()` →
  tick map. The Rust `apply_v3_swap` (slot0) vs. `apply_v3_liquidity_update` (ticks) split
  is the direct mirror — and the latter is exactly the site the `TickMap` trait narrows.
- **Temporal navigation is orthogonal.** `ConcentratedLiquidityStateManager[StateT:
  _StateLike]` treats the state as opaque and only does deque/lock/discard/restore —
  mirroring Rust's `ReorgJournal<V3BlockDelta>`. Neither cares about the slot0/ticks
  split; the Rust journal stays on `V3BlockDelta` (with the `Option<ScalarPriors>`
  refinement above).

## Related

- **ADR-003** (BotCore as state layer) — left the per-pool CL state struct flat; this
  ADR resolves the seam it explicitly deferred (the {Slot0 Head / Tick Bookkeeping Map}
  CONTEXT.md term). ADR-003's "Legacy solver path retirement: delete, not migrate"
  deleted `PyPoolCache`/`RustPoolCache`; this ADR's doc pass cleans up the {PyPoolCache}
  and {LiquidityMap}/{PyBotCore} stale CONTEXT.md terms that outlived that retirement.
- **ADR-001** (I/O-free pools) — the `TickMap` trait is the Rust-core analogue of
  Python's `_HasTickData` protocol; both carry the "this consumer only touches the tick
  map" rule in types rather than comments.
- `rust/CONTEXT.md` terms touched by the Slice 1 doc pass: {Slot0 Head / Tick Bookkeeping
  Map} (flipped to "structural — see ADR-004"), {LiquidityMap} (corrected: generic
  never extracted), {PyBotCore} (corrected: no `LiquidityMap::verify_against_onchain`),
  {PyPoolCache} (rewritten as a removed-term entry matching {V2Snapshot}), {IntHopState}
  (corrected: drops "stored in `PyPoolCache`"; actual cache is the per-pool
  `cached_tick_ranges` field).
- `rust/src/bot_core/liquidity_verifier.rs` module doc — updated in Slice 1 to reference
  the named {Slot0 Head / Tick Bookkeeping Map} term and ADR-004 so the verbal rule and
  the term stay linked (whether or not the code change lands yet).
- Candidate (β) `TickMap` trait implementation (Slice 2+) lands TDD: V3 first
  (`V3PoolState` + `verify_v3_pool` + `apply_v3_liquidity_update` + the `Option` journal
  refinement in `state_history.rs`), then mirrored to V4 against `V4PoolState` /
  `verify_v4_pool` / `apply_v4_liquidity_update`. V4 parity is mandatory.
