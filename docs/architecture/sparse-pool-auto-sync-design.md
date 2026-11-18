# Sparse-pool auto-sync and exactness design

**Status:** proposed design; no implementation. **Scope:** V3/V4 pools registered as
`PoolTickCoverage::Sparse` and solve results carrying `Caveats::SPARSE_COVERAGE`.

## Decision

Do not make sparse pools look exact by growing `known_bitmap_words` on the event
stream, and do not routinely scan every bitmap word. Neither approach establishes
what the optimizer needs at an acceptable per-block cost.

Use three different exactness levels, and do not let a cheaper level clear a
stronger level's caveat:

1. **Read-exact:** one naturally terminating swap evaluation whose complete
   traversal is checked at one canonical tick-map horizon. This is useful for
   finalist validation, but it is not automatically an exact solve.
2. **Solve-exact:** every evaluation needed to establish the optimum is
   read-exact, and the optimum is not an artifact of the partial map's truncated
   search space. A saturated optimum remains approximate until its frontier is
   expanded and the solve rerun.
3. **Pool-exact:** a complete snapshot at block `S`, followed by complete event
   delivery and reorg-safe maintenance after `S`. This is the existing meaning of
   `Tracked`, and it is the only state that generally clears the sparse solve
   caveat for every later walk.

For pools whose repeated solve value justifies it, the recommended route to exact
solves is **selective updater-backed promotion to `Tracked`**. The updater builds
the initial complete map from historical Mint/Burn/`ModifyLiquidity` logs, rather
than from bitmap-wide `eth_call` scans. A new journal-aware promotion transition
must install that snapshot and drain the live event tail before coverage changes.

Demand-driven exact reads are a useful separate optimization. Pump-only
incremental coverage and unconditional full scans are not recommended as the
answer to this task.

## What the current code actually does

### Sparse state is maintained; coverage is not

It is not accurate to say sparse pools receive no event maintenance.
`ConcentratedLiquidityOrchestration::apply_tick_mutation` applies V3/V4 events
without a coverage branch, and `state_mutation::apply_liquidity_update` is also
coverage-agnostic. Both `Tracked` and `Sparse` pools use the same live apply path
once `Live`.

The missing property is **complete coverage**. An event names two boundary ticks,
not the whole 256-bit word, so `state_mutation::mark_bitmap_words_known`
deliberately does not mark an event's word known. The pool can therefore have
current data for every tick it has seen while remaining unable to prove what
other initialized ticks occupy an unexamined word.

Sources:

- `degenbot-substrate/src/concentrated_liquidity_orchestration.rs`,
  `apply_tick_mutation`
- `degenbot-pools/src/families/concentrated_liquidity/state_mutation.rs`,
  `apply_liquidity_update`, `mark_bitmap_words_known`

### Registration establishes provenance, not a permanent truth claim

`tick_assembly` has two materially different arms:

| Arm | Initial state | Coverage |
|---|---|---|
| DB hit | Complete map reconstructed by the updater | `Tracked` |
| Chain hit | The one word containing the current tick | `Sparse` |
| Chain miss | Empty or unavailable map | `Sparse` |

`Tracked` registration enters quarantine, verifies the pinned snapshot, drains,
and becomes `Live`. `Sparse` registration becomes `Live` immediately and skips
verification (ADR-022 D5). Coverage is set by `V3PoolState::from_params`; there
is no post-registration promotion verb.

`known_bitmap_words` is not sufficient as a provenance record. It is a
`HashSet<i32>` with no establishment block or origin. It can be populated by a
whole-word read, but also from tick keys in caller-supplied or replacement maps.
Those alternatives do not establish that every initialized tick in the word is
present, and tick-free words are absent entirely.

Sources:

- `degenbot-substrate/src/tick_assembly.rs`, `resolve_tick_map_arm`,
  `liquidity_map_to_tick_info`
- `degenbot-pools/src/families/concentrated_liquidity/state_seeding.rs`,
  `registration_lifecycle_for`, `seed_known_bitmap_words`
- `degenbot-pools/src/state/v3_state.rs`, `V3PoolState::from_params`
- `docs/adr/ADR-022-registration-verify-lifecycle-core-ownership.md`, D5/D6

### The optimizer and swap reader differ

The swap reader checks `known_bitmap_words` before entering an unknown word and
returns `MissingTickWord`. Its armed recovery can fetch and merge a word, then
retry. A successful read therefore carries useful traversal evidence.

The optimizer does not make that check. `capture_hop` and
`build_int_v3_sequence` consume `tick_data`, slot0 scalars, fee and spacing; they
do not consult `known_bitmap_words`. `compute_tick_ranges` also drops trailing
word-boundary ticks beyond the last known initialized tick. A sparse optimum can
therefore sit at an artificial sequence boundary rather than at the true optimum.

This is why a successful swap quote and an exact optimizer result are different
claims. `SPARSE_COVERAGE` is unioned from registration coverage into
`ResolvedMixedPath`, then copied to `SolvePathResult`; it is not derived from the
winning evaluation's traversal.

Sources:

- `degenbot-pools/src/state/v3_state.rs`, `v3_simulate_swap`
- `degenbot-pools/src/families/concentrated_liquidity/v3.rs`, `capture_hop`
- `degenbot-pools/src/families/concentrated_liquidity/state_walk.rs`,
  `build_int_v3_sequence`
- `degenbot-pools/src/state/tick_bitmap.rs`, `compute_tick_ranges`
- `degenbot-substrate/src/resolve/mod.rs`, `resolve_hops`
- `degenbot-pools/src/solve/mixed/solve.rs`, result construction

## Exactness invariants

### Read-exact

A sparse swap read is exact at anchor `A` only when all of these hold:

1. **Natural termination.** The walk ended because the requested amount or price
   limit was reached, not because `gen_ticks_iter` exhausted its 30,000-item
   work cap. The current `Ok`/`Computed` result does not distinguish these.
2. **Whole-word provenance.** Every entered word was established by a
   `TickWordSource` read or by a complete snapshot—not inferred from the keys of
   an arbitrary partial map.
3. **Scalar-anchored liquidity equality.** Every crossed tick in the traversal
   is present with its actual signed `liquidityNet`. The walk starts from the
   stored slot0 liquidity scalar; equality at every crossing, rather than an
   assumed monotonicity property of partial maps, is what makes the amounts
   exact.
4. **Canonical fetch horizon.** Each fetched word was read at the pool's
   `tick_data_block` at fetch time, and that horizon had not changed at install.
5. **Complete subsequent delivery.** Every later tick mutation through `A` was
   applied in order, with no unresolved delivery gap.
6. **Reorg validity.** No canonical rewind invalidated the fetched word or its
   establishment block.

The scalar-equality condition matters because signed crossings defeat the simple
claim that a partial map always moves price farther than the truth. A known
positive lower tick whose negative upper tick is unknown can leave the model
with too much liquidity above that upper tick. Exactness must come from equality
over the actual traversal, not from a sub-map inequality.

### Solve-exact

A solve result is exact only if the search itself is exact:

- every candidate evaluation needed to prove the argmax is read-exact;
- the sequence was not truncated at the last known initialized tick; and
- if the winning input is a range-saturation corner, the missing frontier is
  fetched and the optimizer rerun until the optimum is interior or the complete
  relevant range is checked.

Checking only the winning evaluation is insufficient: an unevaluated or
mispriced alternative beyond the partial sequence can be better. Consequently,
there is no safe zero-RPC change that removes the sparse solve caveat merely by
examining `known_bitmap_words` after the current solver returns.

### Pool-exact

A pool is exact at `A` when a complete initialized-tick set existed at `S <= A`,
the snapshot was verified at `S`, every event after `S` through `A` was applied
canonically, and every reorg was reverse-applied or invalidated correctly. An
absent word is then known-empty rather than indeterminate.

This is the strongest and simplest condition for routinely returning exact solve
results, and it is the condition selective promotion must establish.

## Fetch and reorg prerequisites

These are correctness prerequisites for any caveat lift, not optional cleanup.

### Fetch at exactly the tick-map horizon

The fetch context must be `tick_data_block`, not `event_horizon` and not an
unvalidated caller block:

- `event_horizon` is zero before the first observed event and is a monotone
  watermark, so it is not a reorg-correct snapshot anchor.
- `tick_data_block` is the per-pool liquidity clock. Chain-seeded sparse pools
  initialize it at head, and journal restore rewinds it.
- Reading **after** the map horizon can double-count a later event when its log
  is applied. Reading **before** it can resurrect a tick deleted by an
  intervening Burn; `overlay_fetched_ticks` protects resident newer rows, but it
  cannot defend a row that was deleted.
- Block zero is an actual genesis pin in `AlloyProvider`, not an alias for
  `latest`. Treat zero as unresolved and refuse to fetch or install it.

The install must recheck that `tick_data_block` is still the requested horizon,
in addition to the existing tick fingerprint check; otherwise return `Raced` and
retry against the new horizon.

Sources:

- `degenbot-pools/src/sim/tick_fetch/policy.rs`, `TickWordFingerprint`,
  `overlay_fetched_ticks`
- `degenbot-pools/src/families/concentrated_liquidity/state_mutation.rs`,
  `apply_liquidity_update`
- `degenbot-rpc/src/provider.rs`, `eth_call` block mapping

### Make fetched coverage rewindable

`merge_tick_word` currently adds known words and fetched rows without a journal
delta. `restore_before_block` rewinds event-sourced ticks and clocks but not that
checked set or those fetched rows. A dead-branch row can also block later
refetch because its establishment stamp is newer.

The design therefore needs a first-class per-pool coverage record, not another
untracked bit set:

```text
CheckedWord {
    established_at: u64,
    origin: WholeWordRead | CompleteSnapshot,
    rows: tick rows sourced by this read,
}
```

A rewind must remove or reverse every fetched row and checked word whose
canonical basis is invalid. A coarse implementation may drop all sparse fetched
coverage on rewind; a precise implementation may retain entries proven valid at
the surviving anchor. Either must make later refetch possible rather than
leaving a newer dead-branch stamp in place.

A complete-snapshot promotion has the same requirement. `replace_tick_data` is
journal-free today and cannot be followed by a safe `Tracked` flip until the
replacement is installed as a new journal genesis or given an equivalent
rollback transition.

Sources:

- `degenbot-pools/src/families/concentrated_liquidity/state_mutation.rs`,
  `merge_tick_word`, `replace_tick_data`
- `degenbot-pools/src/families/concentrated_liquidity/state_journal.rs`,
  `restore_before_block`

## Per-block cost

### Existing lazy and strict-apply paths

`fetch_tick_word_via` performs one bitmap `eth_call`, then one serial
`eth_call` for every set bit. For `U` distinct previously unknown words fetched
in a block:

```text
RPC calls = Σ over U words (1 + popcount(bitmap_word))
```

With the strict sparse-apply gate, `U` contains newly touched Mint/Burn or
`ModifyLiquidity` boundary words. With demand-driven reads, it contains newly
traversed words. With frontier prefetch, it contains whatever the prefetch
policy selects whether or not a solve needs them.

A read-only sample against the environment's reth endpoint measured the current
USDC/WETH V3 pool (`0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640`, spacing 10)
at block **26,439,955**, with every `eth_call` pinned to that block. These are local serial `eth_call` timings, not a
production-wide latency estimate:

| Word | Initialized ticks | RPC calls | Bitmap | Tick reads | Total |
|---|---:|---:|---:|---:|---:|
| 77 | 253 | 254 | 0.709 ms | 149.989 ms | 150.698 ms |
| 78 | 252 | 253 | 0.483 ms | 150.640 ms | 151.123 ms |
| 79 | 187 | 188 | 0.586 ms | 117.503 ms | 118.089 ms |

The consequence is not that every word is this expensive; it is that dense
words can cost hundreds of calls and hundreds of milliseconds. A small number
of cold words can consume a meaningful fraction of a block budget. Per-block
fetch work therefore needs a measured budget and must not be the default route
to whole-map completeness.

Source for the call shape: `degenbot-rpc/src/tick_bootstrap.rs`,
`fetch_tick_word_via`. Measurement used `slot0()`, `tickBitmap(int16)` and
`ticks(int24)` at the stated block, with no state writes.

### Whole-range scanning

The valid tick bounds imply these bitmap-word counts before any initialized
tick reads:

| Tick spacing | Words over the valid range |
|---:|---:|
| 1 | 6,932 |
| 10 | 694 |
| 50 | 140 |
| 60 | 116 |
| 100 | 70 |
| 200 | 36 |

Every non-empty word adds its initialized-tick reads. This may be tolerable as
an explicitly scheduled one-time job for a small allowlist, but it is not a
standing per-block synchronization mechanism.

### Updater-backed promotion

The updater already has the appropriate bulk mechanism:

- V3 fetches Mint/Burn logs over block ranges, optionally scoped to one pool;
- V4 fetches `ModifyLiquidity` logs by `PoolManager`, with pool-id selection
  above the transport;
- `apply_event_loop` reconstructs the complete tick map and incremental deltas;
- compute → verify → persist commits only a verified update.

After promotion, the bot uses the same event-application path it already uses
for Sparse pools. **Steady-state `TrackedLive` adds no per-block tick-word
`eth_call`.** The costs are the one-time historical log backfill, subsequent
ordinary event processing, and DB delta writes. Those need a soak measurement
before setting rollout budgets, but they do not have the bitmap-plus-popcount
shape above.

Sources:

- `degenbot-pool-updater/src/fetch.rs`, `fetch_v3_liquidity_logs`,
  `fetch_v4_liquidity_logs`
- `degenbot-db/src/liquidity_updater.rs`, `apply_event_loop`,
  `compute_v3_liquidity_update_on_conn`, `persist_v3_liquidity_update_on_conn`

## Recommended promotion design

### Trigger selectively

Do not promote every sparse registration. Candidates should come from an
operator allowlist or repeated solve demand—for example, a pool crossing a
configured count of candidate evaluations over a rolling window. The trigger
should include the value at stake where available, since promotion buys both
better ranking and fewer wasted simulations.

Instrument before choosing a threshold: sparse evaluations by pool, saturated
optima, cold words fetched, fetch latency, and approximate candidates that later
fail simulation. The existing sparse caveat currently has no downstream policy
consumer, so these measurements are also necessary to quantify the benefit.

### Make promotion an explicit state machine

Use one per-pool state owner along these lines:

```text
SparseLive
  → SnapshotBuilding { anchor S, attempt }
  → SnapshotVerifying { anchor S, complete map }
  → TrackedLive

SnapshotBuilding / SnapshotVerifying
  → SparseLive                    (retryable failure, stale anchor, or rewind)
```

Rules:

1. `SparseLive` continues to produce **approximate** results while the updater
   builds the map. Never clear the caveat in anticipation of promotion.
2. Pin `S` and retain a canonical event tail from `S+1`. The existing pool may
   remain approximate-live during the bulk build, but the transition must not
   lose events while the snapshot is being assembled.
3. When the complete snapshot is ready, pause new solves for that pool, verify
   the snapshot, and apply the retained tail under the existing
   quarantine/drain semantics.
4. Install the snapshot as a journal-aware state transition at one anchor.
   `TrackedLive` starts only after installation and drain complete. Keep the
   numeric pool id stable so path registrations do not silently change identity.
5. On verification failure, a stale head, or a rewind across `S`, discard the
   candidate snapshot and return to `SparseLive` with backoff. Never leave a
   half-installed map labeled `Tracked`.
6. After promotion, ordinary Mint/Burn/`ModifyLiquidity` application maintains
   exactness. A delivery hole or rollback beyond the journaled basis must refuse
   exactness loudly rather than silently retaining a permanent `Tracked`
   promise.

The updater's persisted map then lets a later restart register the pool through
the existing DB/`Tracked` arm without repeating the historical backfill.

### Use strict sparse apply selectively

`pump.strict_sparse_apply` protects reorg priors by fetching an unchecked
boundary word before applying a tick mutation. It is a useful safety posture
for pools undergoing exactness work, and its `event_block - 1` fetch discipline
avoids applying an event over a future word.

It does not cover old words that no event touches, so it cannot establish
pool-exactness. Enabling it globally is a separate cost decision governed by the
per-word measurements above, not a prerequisite for declaring every solve exact.

### Keep demand-driven exact reads as a separate lane

`FetchMode::Discovery`, `swap_missing_words`, and `stage_missing_words` already
provide the right lock-free staged-fetch choreography. They can support an
exact finalist read or prefetch the frontier of a saturated solve, subject to
the provenance, natural-termination, fetch-horizon, and rewind rules above.

Do not use a successful read alone to clear `SPARSE_COVERAGE` from the solve
result. For a non-saturated optimum, a future implementation may establish the
stronger search proof; a saturated optimum must expand and rerun. Until that
proof exists, the solve remains approximate even if its final quote is exact.

## Alternatives rejected

- **Mark words known from events.** An event supplies two boundary ticks, not
  the word's complete initialized set.
- **Always-on frontier prefetch.** It pays for words no solve may enter and
  still cannot promise coverage for a deep walk.
- **Routine whole-range bitmap scans.** The call count and dense-word latency
  are disproportionate to the steady-state benefit.
- **Clear the caveat from `known_bitmap_words` alone.** The set lacks origin,
  establishment time, natural-termination and search-space-completeness proofs.
- **Flip `Tracked` after `replace_tick_data`.** The current replacement is
  journal-free and has no safe rollback semantics.
- **Treat an exact finalist quote as an exact optimizer result.** The optimizer
  may have stopped at an artificial partial-map boundary.

## Implementation sequence, if approved

1. **Safety and measurement first.** Add the fetch-horizon guard, coverage
   provenance and rewind invalidation; expose natural termination and saturation
   facts; instrument sparse demand and per-block word cost. Do not lift caveats
   in this stage.
2. **Read-level exactness.** Let naturally terminating, fully provenanced reads
   report exact amounts. Keep solve caveats intact unless the search proof is
   also established.
3. **Selective promotion.** Implement the updater-backed state machine for a
   small allowlist, including the journal-aware snapshot install and retry/
   demotion behavior.
4. **Demand expansion only where measured.** Use the staged discovery/fetch
   machinery for saturated high-value solves or exact finalist validation, with
   an explicit per-block word and latency budget.

Required behavioral tests include: stale Burn cannot resurrect a deleted tick;
block zero is refused; iteration-cap exhaustion is not exact; saturated optima
stay approximate; rewind drops dead-branch coverage and permits refetch;
incomplete promotion never clears the caveat; and `TrackedLive` performs zero
tick-word RPC in a steady-state block.

## Bottom line

Sparse pools already receive event updates. The missing capability is a
canonical, rewindable proof of coverage. Incremental word fetching can build
that proof only for the words it touches, at a potentially substantial serial
RPC cost, and a checked swap walk still does not prove an exact optimum.

For repeated high-value solves, **selective complete-snapshot promotion through
the updater, followed by ordinary event maintenance**, is the sound route to
exact solves with no additional per-block tick-word RPC. Everything cheaper
should be presented as read-level or candidate-level exactness, not as a blanket
retirement of the sparse solve caveat.
