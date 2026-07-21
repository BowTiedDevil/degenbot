# ADR-014: Pool-State Deepening — Where the Trait Seams Live

**Status: accepted (decision).** The deepening itself is a candidate
slice (sequenced per the CONSEQUENCES below — CL/journal/`PoolEntry` now,
V2/Aerodrome after `DBISWP`); slicing is tracked separately. This ADR
records the *architectural* decisions so future architecture reviews do
not re-suggest (a) a single uniform `trait PoolFamilyReg` across all
seven pool variants, or (b) state-struct traits for the reserve-pair and
balance-vector families.

## Context

`BotState` (`rust/crates/degenbot-bot/src/bot_core/mod.rs`) is a ~6 837-line
god-object holding a single `pools: HashMap<u64, PoolEntry>` registry and
~200 methods dispatching via `match` on the `PoolEntry` sum type. The
per-family value *structs* were already relocated to `degenbot-pools`
(relocation comments: "partially relocated… the registry stays in `bot`"),
but the field-mutating methods that operate on them — every `apply_*`
event replay, `*_journal_len` / `*_discard_before_block` /
`*_restore_before_block`, the V3/V4 liquidity buffers' drain loops, the
snapshot-seed take/pin, the `register_*` construction + genesis-delta push,
the `get_*` readers' variant-exhaustion arms — stayed flat on one
`impl BotState`. A V3 swap apply sits ~700 lines from the
`sqrt_price_x96` field it writes, in a different crate.

An architecture review proposed "split `BotState`'s governance into
per-family registry modules behind a `trait PoolFamilyReg<Identity, State,
Delta>`." Grilling the proposal surfaced three structural families with
genuinely different shapes:

- **Reserve-pair** (`reserve0/1: U112`, full-state `V2BlockDelta`) — V2,
  AerodromeV2 (the latter's `AerodromeV2PoolState.journal` is literally
  `ReorgJournal<V2BlockDelta>`).
- **Balance-vector** (`balances: Vec<U256>`, full-state
  `*BlockDelta`) — Curve, Balancer-weighted, Balancer-stable (three
  byte-identical delta structs).
- **Concentrated-liquidity (CL)** (slot0 scalars + `tick_data`, partial-prior
  `V3BlockDelta`) — V3, V4 (structurally near-identical; differ in identity
  shape and V4's `pool_key` nesting).

A uniform `PoolFamilyReg` was investigated and rejected: the families differ
in field shape, delta shape, and restore return type, so the trait either
no-ops methods on the simpler families (shallow) or splits into three
sub-traits that mirror these families (giving back the problem). Separately,
the V3/V4 liquidity buffers (`v3_buffer` keyed by `Address`, `v4_buffer`
keyed by `(Address, PoolId)`) are cross-pool mailboxes that accept events
*before the pool is registered* — they are a registry concern and cannot
live on the state struct (the `V3PoolState` doesn't exist yet to hold its
own buffer).

## Decision

### D1 — No uniform trait. Deepen the state structs with inherent impls.

Push the field-mutating mass onto `impl <Family>PoolState` in
`degenbot-pools` (the same structs that already own the fields and already
carry some inherent methods): `apply_*`, `journal_len` /
`discard_before_block` / `restore_before_block`, snapshot-seed take/pin,
`from_params` (construct + genesis delta). `BotState`'s per-arm bodies
collapse to one-line dispatch; the cross-pool concerns (address index, V3/V4
buffers, ingress routing, drain orchestration, cross-family dispatch like
`discard_v3_or_v4_before_block`, `set_snapshot_seed_block`) stay on the
holder. ADR-003 (single state owner) preserved — `BotState` still owns the
`HashMap<u64, PoolEntry>`, the buffers, the index.

### D2 — State-struct trait for the CL family only.

The existing read trait `V3FamilyPool` is renamed `ConcentratedLiquidityPool`
(the old name anchored on V3, misleading when V4 is an equal member) and
gains a mutable twin `ConcentratedLiquidityPoolMut` covering
`replace_tick_data` and future CL mutators over `tick_data` +
`update_block` + the tick-range cache. V3 and V4 are two adapters behind
the same per-pool interface (the two-adapter rule — a real seam, not a
hypothetical one), and Q1's move gives both structs identical method names
the trait abstracts. `BotState::sync_tick_data_by_pool_id` becomes a 1-call
or-pattern dispatch instead of two duplicated 4-line arms; `BotState` stores
`PoolEntry` as a sum type so the family trait still requires a `match` to
produce the `&dyn` — accepted, because the dedup is the *call-site body*
and the trait's latent value (future CL mutators join free, uniform CL
dispatch for the standalone consumer) is real.

### D3 — Reserve-pair + balance-vector dedup at the journal/delta layer.

State-struct traits are **not** adopted for reserve-pair and
balance-vector. The duplication in those families sits one layer down —
on `ReorgJournal` and `BlockDelta` — and dedups there:

1. Unify the three byte-identical balance-vector delta structs
   (`CurveBlockDelta`, `BalancerWeightedBlockDelta`,
   `BalancerStableBlockDelta`) into one `BalancesBlockDelta`. Reserve-pair's
   `V2BlockDelta` is already shared (V2 + Aerodrome use it).
2. Extend `BlockDelta` with `type RestoreState` + a `landed()` accessor,
   collapsing five hand-duplicated `restore_*_before_block` impls into one
   generic `impl<D: BlockDelta> ReorgJournal<D>::restore_before_block`.

The V3 family keeps its own restore impl — `V3RestoreResult` and the
`scalar_priors` / `tick_priors` branches are a genuinely different
algorithm (partial-prior deltas, the `Option<ScalarPriors>: None` no-op
path), not a full-state delta. A state-struct trait on these families
would re-express, at the state-struct level, a dedup the journal can do
more directly at the delta level — and after the journal-layer dedup the
residual per-family apply bodies are too short to justify a trait + `dyn`.

### D4 — V4 buffer narrowing at the drain→apply seam.

`BufferedV4LiquidityUpdate.liquidity_delta` stays `I256` (matches the
on-chain `ModifyLiquidity` event's `int256` envelope); the CL apply method
takes `i128` (matches `Tick.Info.liquidityNet: int128`, the tick-layer
type V4 itself narrows to at `PoolManager.sol:666`
`params.liquidityDelta.toInt128()`). The one narrowing lives at the
drain→apply call site in `BotState`, mirroring the contract's own
boundary narrowing. An `int256` that doesn't fit `int128` is dropped at
the registry seam, not buried in the apply body.

### D5 — `PoolEntry` projections replace variant-exhaustion arms.

The seven `get_*_identity` / `get_*_pool` readers stay on `BotState`
(they're borrows out of `BotState`'s own map; a method returning
`&FamilyPoolIdentity` out of the map can't live on a borrowed
`FamilyPoolState` — the identity lives in `PoolEntry`, not on state). But
their 7× variant-exhaustion arms (`match … { V2(_,s) => Some(s), V3 | V4
| Curve | … => None }`) collapse onto `PoolEntry` itself, where the sum
type lives: add `PoolEntry::v2()` / `v2_mut()` / … (7 families) projection
methods in `degenbot-pools/src/registry.rs`; each reader becomes
`self.pools.get(&pool_id).and_then(PoolEntry::v2).map(|(i,_)| i)`. The
`_mut` projections serve Q1's dispatch (`entry.v3_mut()` instead of
re-matching). No trait, no `dyn` — projections on the sum type where they
structurally belong. The identity/state sibling split in `PoolEntry::V2(
V2PoolIdentity, V2PoolState)` is preserved.

### D6 — V2 conforms to `from_params`.

`register_v2_pool` is the sole outlier that inlines the `V2PoolState`
build + `ReorgJournal<V2BlockDelta>` construction + genesis delta on
`BotState`; the other six families delegate to
`<Family>PoolState::from_params(params, journal_depth)`. V2 predates the
`from_params` convention. Add `V2PoolState::from_params`;
`register_v2_pool` delegates, matching its six siblings. Spec-validation
free functions (`spec_bounds::validate_*`) stay where they are — already
correctly placed (pure validation in `degenbot-pools`, runnable before any
state exists).

## Consequences

- `bot_core/mod.rs`'s `impl BotState` block shrinks across five structural
  reductions (D1–D6). `BotState` retains its genuine registry concerns: the
  `pools` map, the address index, the V3/V4 buffers (+ ingress routing +
  drain orchestration), cross-family dispatchers, spec-validation
  pre-checks, `set_snapshot_seed_block`.
- The buffer is **not** a "leftover flat spot" on `BotState` — it is
  correctly placed (registry concern, pre-registration mailbox). The
  deepening completes the separation the codebase already half-built: the
  pool state struct applies, `BotState` orchestrates, the mailbox holds.
- ADR-003 preserved throughout — `BotState` stays the single state owner;
  only the field-touching logic moves onto the structs that own those
  fields.

### Sequencing (interleaved, not gated on `USPN7M`)

An earlier framing of this candidate said "do after `USPN7M` lands so the
structs are settled." `USPN7M` is the *planned* pools-extraction epic
named in `docs/migration-guides/pools-extraction-inventory.md`, but it is
**not a tracked ergo task** — the inventory doc frames its sub-tasks as
"to be created." There is no single effort to wait on. The value-only
leaf sims (`v3_simulate_swap` etc.) are already in `degenbot-pools`; the
`simulate_*` retry shell on `BotState` (mod.rs:1536–2049) is intentionally
in `bot` per the lib.rs "compute with no I/O?" dividing line and is not
migrating.

The real in-flight work is `DBISWP` ("Reserve storage: U112 in
`degenbot-pools`"), the lone `doing` task, which retypes
`V2PoolState.reserve{0,1}` and `AerodromeV2PoolState.reserve{0,1}` (`U256
→ U112`) and cascades through every reserve-reading site. It collides
exactly with the V2/Aerodrome arms of D1 and all of D6.

**Take now (independent of `DBISWP`):** the CL half of D1
(`V3PoolState::apply_swap` / `apply_liquidity_update`, V4 twin), D2
(`ConcentratedLiquidityPool(Mut)` + rename), D3 (journal/delta dedup —
`BalancesBlockDelta` unification + `BlockDelta::RestoreState`), D4 (V4
narrowing seam), D5 (`PoolEntry` projections).

**Defer until `DBISWP` lands:** the V2 + Aerodrome halves of D1
(`apply_v2_sync` / `apply_aerodrome_sync` bodies onto the state structs),
D6 (`V2PoolState::from_params`).

## Why not the alternatives

- **A single `trait PoolFamilyReg<Identity, State, Delta>` across all
  seven variants** — rejected (D1 vs the trait): the three families
  genuinely differ in field shape (`U112×2` / `Vec<U256>` /
  slot0+`tick_data`), delta shape (full-state vs partial-prior), and
  restore return type (`(U112,U112,u64)` / balance vecs /
  `V3RestoreResult`). A uniform trait no-ops methods on the simpler
  families (shallow) or splits into three sub-traits mirroring the
  families (giving back the problem).
- **State-struct traits for reserve-pair / balance-vector, mirroring the
  CL trait** — rejected (D3): the duplication in those families is a
  `ReorgJournal` / `BlockDelta` concern, not a state-struct concern; the
  journal is already generic over `D: BlockDelta`, and deduping at the
  delta level (unify the three balance-vector deltas, add
  `type RestoreState`) collapses five hand-duplicated restore impls into
  one generic impl. A state-struct trait would re-express that dedup one
  layer up, at the cost of a trait + `dyn`; the residual per-family apply
  bodies are too short to justify it.
- **Merging the V3 and V4 state structs into one `ClPoolState`** —
  rejected: V3 and V4 are structurally near-identical in *per-pool state*
  but differ in identity shape (V4's `pool_key` nesting), pool manager
  keying (`(Address, PoolId)` vs `Address`), swap path (hooks, dynamic
  fees), and decode paths. The type system should not erase those. A trait
  at the seam (D2) lets `BotState` treat the CL family uniformly without
  pretending V3PoolState and V4PoolState are the same type.
- **Moving the V3/V4 buffer onto the state struct** — rejected (D1): the
  buffer accepts events *before the pool is registered* — the `V3PoolState`
  doesn't exist yet to hold its own buffer. The buffer is a registry concern
  by construction.
