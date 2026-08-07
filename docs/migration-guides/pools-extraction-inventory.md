# `degenbot-pools` extraction inventory

**Spike output (ergo `Z7HA3Y`, epic `USPN7M`).**
Audits `rust/crates/degenbot-bot/src/bot_core/` and classifies every public
symbol MOVES (to the new `degenbot-pools` crate) / STAYS (in `degenbot-bot`)
/ UNCLEAR. Feeds the detailed extraction tasks (created under epic `USPN7M`
after this spike).

The dividing test agreed with the user: *given all pool state already in
memory, can this compute deterministically with no chain access, no registry
lookup, no async, no tokio?* Yes → MOVES. No → STAYS.

---

## 1. Module / file-level disposition

`bot_core/mod.rs` is a 7500-line file mixing value types, the registry, I/O
methods, registration, and the V3/V4 swap shells. It `pub mod`s 22
submodules; the per-family state lives in separate files.

### Submodules — MOVES (value-only pool state + math)

| file | contents | notes |
|---|---|---|
| `aerodrome_v2_state.rs` | `AerodromeV2PoolIdentity`, `AerodromeV2PoolState`, `RegisterAerodromeV2PoolParams` | value types |
| `balancer_stable_state.rs` | `BalancerStablePoolIdentity`, `BalancerStablePoolState`, `BalancerStableBlockDelta`, `RegisterBalancerStablePoolParams` | **HOLDS `rate_provider: Option<Arc<dyn BalancerRateProvider>>` FIELD (I/O trait)** — see §5 |
| `balancer_weighted_state.rs` | `BalancerWeightedPoolIdentity`, `BalancerWeightedPoolState`, `BalancerWeightedBlockDelta`, `RegisterBalancerWeightedPoolParams` | value types; no rate-provider field |
| `curve_state.rs` | `CurvePoolIdentity`, `CurvePoolState`, `CurveBlockDelta`, `RegisterCurvePoolParams` | **HOLDS `data_provider: Option<Arc<dyn CurveDataProvider>>` FIELD (I/O trait)** — see §5 |
| `v3_state.rs` | `V3PoolIdentity`, `V3PoolState`, `RegisterV3PoolParams`, `RegisterV3PoolError`, `V3SwapUpdate`, `V3SwapOutcome`, `SimulateSwapError`, `BufferedV3LiquidityUpdate`, `PoolTickCoverage`, `v3_simulate_swap` | **HOLDS `fetcher: Option<Arc<dyn TickWordFetcher>>` FIELD (I/O trait)** — see §5. The value-only core `v3_simulate_swap` is the target of the §5 split |
| `v4_state.rs` | `V4PoolIdentity`, `V4PoolState`, `RegisterV4PoolParams`, `RegisterV4PoolError`, `V4PoolKey`, `V4SwapUpdate`, `V4StateSync`, `BufferedV4LiquidityUpdate`, `v4_simulate_swap` + consts `AMOUNT_MODIFYING_HOOK_MASK`, `V4_DYNAMIC_FEE_FLAG` | **HOLDS `fetcher: Option<Arc<dyn TickWordFetcher>>` FIELD (I/O trait)** — see §5 |
| `spec_bounds.rs` | `SpecViolation`, `SpecValue`, `UINT112_MAX`, `V3_FEE_MAX`, `V4_FEE_MAX`, `MIN_TICK_SPACING`, `MAX_TICK_SPACING`, `validate_v2_reserve`, `validate_sqrt_price`, `validate_tick`, `validate_v3_fee`, `validate_v4_fee` | pure value validators |
| `state_history.rs` | `BlockDelta` trait, `ReorgJournal<D>`, `V2BlockDelta`, `V3BlockDelta`, `ScalarPriors`, `TickBefore`, `V3RestoreResult`, `JournalError` | reorg-journal DATA (Vec of deltas + apply/rollback); the COORDINATOR (`reorg_coordinator.rs`) stays in bot |
| `tick_bitmap.rs` | `TickAlongPath`, `GenTicksError`, `V3TickRangeForSolver`, `gen_ticks`, `compute_tick_ranges`, `update_tick_liquidity`, `apply_liquidity_to_tick_range` | V3 tick-bitmap walk math (pure; used by `v3_simulate_swap`) |
| `tick_map.rs` | `TickMap`, `TickMapMut` traits | typed tick-map boundary (ADR-004) |

### Submodules — STAYS (engine / I/O / registry)

| file | reason |
|---|---|
| `block_clock.rs` | block clock — engine/I/O |
| `block_pump.rs` | the pump — I/O |
| `engine.rs` | engine |
| `log_dispatcher.rs` | log dispatch — I/O |
| `reorg_coordinator.rs` | decides WHEN to roll back journals (the journal DATA moves, the coordinator stays) |
| `snapshot_verify.rs` | snapshot verify — I/O/registry |
| `solve_coordinator.rs` | solve coordination — engine |
| `tick_fetch.rs` | **TRAIT + impls — see §5 (the `TickWordFetcher` trait + RPC/db impls stay in bot; only the trait *definition* may move — recommendation §5)** |
| `drain_sink.rs` | engine/registry plumbing |
| `liquidity_verifier.rs` | verifier — I/O/registry |

### Submodules — UNCLEAR → resolved in §5

| file | disposition |
|---|---|
| `curve_data_provider.rs` | `CurveDataProvider` trait + `CurveDataProviderError`. **Trait *definition* MOVES to pools (§5); no impls live here** — impls are bot-side RPC |
| `rate_provider.rs` | `BalancerRateProvider` trait + `RateProviderError` + `StaticRateProvider` (value-only impl). **Trait *definition* MOVES to pools (§5); `StaticRateProvider` moves with it (value-only); RPC impls stay in bot** |

---

## 2. Inline definitions in `bot_core/mod.rs`

| line | symbol | kind | disposition |
|---|---|---|---|
| 89 | `PoolEntry` | enum | MOVES (value tag: `V2`/`V3`/`V4`/`Curve`/`BalancerWeighted`/`BalancerStable`/`AerodromeV2`) |
| 120 | `V3FamilyPool` | trait | MOVES (V3/V4 shared mutable-reader surface; dyn-dispatched over borrowed state) |
| 178 | `V2PoolIdentity` | struct | MOVES |
| 218 | `V2PoolState` | struct | MOVES |
| 233 | `RegisterV2PoolParams` | struct | MOVES (DTO) |
| 283 | `RegisterV2PoolError` | enum | MOVES (value error) |
| 304 | `TickInfo` | struct | MOVES (value: tick data) |
| 327 | `TokenEntry` | struct | MOVES (token registry identity; value) |
| 348 | `BotState` | struct | **STAYS** (the registry `HashMap<u64, PoolEntry>` owner; the I/O umbrella's state) |
| 4317 | `Bot` | struct | **STAYS** (chain-driven pool retrieval + the pump) |
| 4339 | `BlockMetadata` | struct | STAYS (block I/O metadata) |

---

## 3. `BotState` method classification

`impl BotState` has ~110 `pub fn`s. Almost all are **registry-coupled** (they
look up `self.pools.get(&pool_id)` then compute). The recommendation is NOT
to move these methods to `degenbot-pools` wholesale — it is to **extract the
value-only COMPUTATION they call into pools as free functions over
`&PoolEntry` / the state, and leave `BotState::get_*` / `calculate_*` / etc.
in bot as thin registry-lookup wrappers** that delegate to the value layer.
This preserves the `BotState` public API (which the PyO3 layer + Python
companion depend on) and keeps the registry-coupled lookup in the engine.

### Method categories

**STAYS as thin registry-lookup wrappers** (computes via the pools value layer):
- `calculate_tokens_out`, `calculate_tokens_out_miss_aware`, `calculate_tokens_in`
  (registry lookup → dispatch over `PoolEntry` → call `degenbot_pools::simulate_*`)
- `simulate_exact_input_swap_miss_aware`, `simulate_exact_input_swap_with_fetch`,
  `simulate_exact_output_swap_miss_aware`, `simulate_exact_output_swap_with_fetch`,
  `simulate_swap_with_override`, `calculate_tokens_out_with_fetch`
  (these are the **I/O fetch-retry shells** around the value-only core; Pattern B
  — they catch returned `MissingTickWord(word)` and call the fetcher. Stay in bot
  unchanged; only the value core they call moves.)
- `encode_swap` (registry lookup + encoding dispatch — encoding logic itself is
  value-only in `degenbot-uniswap`; the *dispatch over PoolEntry* is value and
  could move, but the `self.pools.get` is registry)

**STAYS (registry mutation / I/O / pump / lifecycle)** — do NOT move:
- `register_*_pool` (register_v2/v3/v4/curve/balancer_weighted/balancer_stable/aerodrome) —
  constructs state from params, validates spec bounds, inserts into registry
- `apply_*_sync`, `apply_*_swap`, `apply_*_liquidity_update`, `update_*_pool`,
  `sync_*_pool_state`, `apply_*_balance_update_by_pool_id`, `apply_*_swap_by_pool_id`,
  `process_backfill_logs`, `merge_tick_word`, `buffer_*`, `flush_*_buffer`,
  `expire_*_buffered`, `apply_backfill_buffer_*`, `apply_pump_buffer_*`, `pin_*_snapshot`
- `load_snapshot_from_db`, `dispatch_log`, `try_decode_log`, `attach_engine`,
  `subscribe_pool_state_change`, `start`, `set_*`
- `v2/curve/balancer/v3/v4_discard_before_block`, `*_restore_before_block`,
  `restore_all_pools_before_block`, `restore_pool_before_block`,
  `restore_v3_or_v4_before_block` (reorg COORDINATION; the journal DATA moved but
  the rollback decision/driver stays)

**STAYS (registry reads — thin accessors that read `self.pools.get`)**:
`get_v2_pool_state`, `get_v2_identity`, `get_v3_pool`, `get_v3_identity`,
`get_v4_pool`, `get_v4_identity`, `get_curve_pool`, `get_curve_identity`,
`get_balancer_weighted_pool`, `get_balancer_weighted_identity`,
`get_balancer_stable_pool`, `get_balancer_stable_identity`, `get_aerodrome_pool`,
`get_aerodrome_identity`, `get_v3_or_v4_pool`, `token_entry`, `pool_id_by_address`,
`pool_family`, `pool_count`, `v2_pool_count`, `v3_pool_count`, `v4_pool_count`,
`has_pool`, `has_token`, `has_state_prior_to`, `v2/curve/balancer/v3/v4_journal_len`,
`v2/curve/balancer/aerodrome/v3/v4_snapshot*`, `take_*_snapshot_seed`,
`v3/v4_pools_snapshot`, `v4_pool_id_by_key`, `v4_registered_pool_managers`,
`v3_snapshot_seed`, `v3_snapshot_store`, `v4_snapshot_store`,
`buffered_v3_event_count`, `v4_snapshot_seed`, `v3/v4_pool_id_*`

These read the registry; they stay. A subset that is *pure projection over a
`PoolEntry`* (e.g. `pool_family(&self, pool_id) -> &'static str`) could be a
pools free function `pool_family(&PoolEntry) -> &'static str` with the BotState
method delegating — low value, do opportunistically if it simplifies the
PyO3 surface, not as a goal.

### `impl Bot` (~13 methods) — ALL STAY
`new`, `with_core`, `state_arc`, `load_snapshot_from_db`, `dispatch_log`,
`try_decode_log`, `resolve_pool_id`, `restore_pool_before_block`,
`has_state_prior_to`, `notify_pool_state_updated`, `attach_engine`,
`subscribe_pool_state_change`, `start`. These are the I/O umbrella; none move.

---

## 4. `SimulateSwapError` / `MissingTickWord` ownership

**Confirmed:** `v3_simulate_swap` (`v3_state.rs:594`) returns
`Result<V3SwapOutcome, SimulateSwapError>` where `SimulateSwapError` is
(`v3_state.rs:560`):

```rust
pub enum SimulateSwapError {
    NotComputable,
    MissingTickWord(i32),
}
```

`MissingTickWord(i32)` is a **pure value variant** — "the tick walk entered
bitmap word `i32` that has not been fetched." The simulator does NOT take a
fetcher parameter, does NOT fetch. The fetch+retry loop is in
`BotState::simulate_swap_with_override` (`mod.rs:2286`), which catches
`MissingTickWord(word)` and calls `state.fetcher.fetch_missing_tick_word(...)`.

**Disposition:** `SimulateSwapError` + `V3SwapOutcome` + `v3_simulate_swap`
(and the V4 analog `v4_simulate_swap` + `V3SwapOutcome` reuse) MOVE to
`degenbot-pools` as the value-only core. No I/O leak — the missing-data signal
travels as data. **Pattern B confirmed across V3/V4.** The `*_with_fetch` /
`*_with_override` shells stay in `degenbot-bot`.

(Curve/Balancer swap sims do not have a sparse-fetch equivalent — they compute
directly from stored balances/rates. They are value-only and move outright;
no retry loop shadow.)

---

## 5. The cross-cutting risk: I/O trait-object fields on state structs

**This is the highest-risk decision in the whole extraction.** Four pool-state
structs hold an `Option<Arc<dyn <I/O trait>>>` field:

| state struct | field | trait | trait file |
|---|---|---|---|
| `V3PoolState` (`v3_state.rs:114, 283`) | `fetcher: Option<Arc<dyn TickWordFetcher>>` | `TickWordFetcher` | `tick_fetch.rs:57` |
| `V4PoolState` (`v4_state.rs:117, 258`) | `fetcher: Option<Arc<dyn TickWordFetcher>>` | `TickWordFetcher` | `tick_fetch.rs:57` |
| `CurvePoolState` (`curve_state.rs:197, 298`) | `data_provider: Option<Arc<dyn CurveDataProvider>>` | `CurveDataProvider` | `curve_data_provider.rs:56` |
| `BalancerStablePoolState` (`balancer_stable_state.rs:183, 241`) | `rate_provider: Option<Arc<dyn BalancerRateProvider>>` | `BalancerRateProvider` | `rate_provider.rs:41` |

The state structs MOVE to `degenbot-pools`, but their field types reference
traits currently in `degenbot-bot`. Moving the structs without moving the
traits creates a cyclic dep (`degenbot-pools` → `degenbot-bot`).

### Recommendation: move the three trait *definitions* + value-only impls to `degenbot-pools`; RPC/DB impls stay in `degenbot-bot`

- `TickWordFetcher` trait + `FetchTickWordError` + `FetchedTickWord` → MOVE to
  `degenbot-pools` (it's an *interface* describing "produce a tick word," like
  `std::io::Read` — defining it pulls no I/O). The RPC/db impls in
  `tick_fetch.rs` STAY in `degenbot-bot`.
- `CurveDataProvider` trait + `CurveDataProviderError` → MOVE to pools.
  Impl(s) stay in bot.
- `BalancerRateProvider` trait + `RateProviderError` → MOVE to pools.
  `StaticRateProvider` (value-only impl — always returns construction-time
  rates, `is_static() == true`, no I/O) → MOVES to pools. RPC impl(s) stay in bot.

This is the `std::io::Read` precedent precisely: `Read` is *defined* in
`std::io` (a low layer), and `File` (an I/O impl) is a higher layer that
impl's it. Defining the capability does not pull the implementation. The
value-only simulators (`v3_simulate_swap` returning `MissingTickWord`) are
untouched — Pattern B holds; pools never invokes a fetcher, it only names
the capability that a state's erased field might hold.

### Rejected alternative A — move the fetch loop into pools (Pattern A)
The simulator would take `&dyn TickWordFetcher` and fetch internally. This
pulls the I/O-loop SHAPE into the value layer, blurring the dividing test.
**Rejected.**

### Alternative B (purer, more churn) — remove the trait FIELDS from the state structs
Bot maintains parallel `HashMap<pool_id, Arc<dyn TickWordFetcher>>` /
`HashMap<pool_id, Arc<dyn CurveDataProvider>>` /
`HashMap<pool_id, Arc<dyn BalancerRateProvider>>` maps alongside the
registry. `V3PoolState`/`V4PoolState`/`CurvePoolState`/`BalancerStablePoolState`
become pure value (no trait-object fields). 

Touches every `state.fetcher` / `state.data_provider` / `state.rate_provider`
access: registration (`params.fetcher` → state field, `:383`/`:334`/`:361`/`:287`),
the override loop (`state.fetcher.clone()` at `mod.rs:2286` area),
snapshot/clone paths (`fetcher: self.fetcher.clone()` at `:304`/`:278`), and
the sentinel constructions (`fetcher: None` at `:869`/`:408`/`:561`/`:321`).

This produces the *cleanest* value-only state but is a larger refactor
(driver-bot-side parallel registry + threaded access from the override loop).
**Flag as the long-term ideal; NOT for the first pools extraction pass** — the
trait-definition-move recommendation above unblocks the move with far less
churn and is faithful to the standalone-constraint spirit.

---

## 6. Dependency set for `degenbot-pools`

```
[dependencies]
alloy = { version = "^2.0" }                       # U256/U512/Address/B256/I256
thiserror = { version = "^2.0" }                   # SimulateSwapError, JournalError, SpecViolation, Register*PoolError, FetchTickWordError, CurveDataProviderError, RateProviderError
parking_lot = { ... }                               # if ReorgJournal uses it (verify in state_history.rs during impl)
degenbot-core = { path = "../degenbot-core" }      # if any core types referenced (verify; may be droppable)
degenbot-uniswap = { path = "../degenbot-uniswap" } # DexVariant (referenced by V2PoolIdentity)
degenbot-v2-math = { path = "../degenbot-v2-math" } # IntHopState/swap (V2 CP swap primitive — landed in YS5SMI)
degenbot-cl-math = { path = "../degenbot-cl-math" } # compute_swap_step_v3 + tick math used by v3_simulate_swap
degenbot-curve-math = { path = "../degenbot-curve-math" } # stableswap math used by Curve swap sim
degenbot-balancer-math = { path = "../degenbot-balancer-math" } # Balancer weighted/stable math
degenbot-solidly-math = { path = "../degenbot-solidly-math" } # Camelot solidly-stable math (Aerodrome stable)
```

NO `tokio`, NO `pyo3`, NO rpc client. Verify `parking_lot`/`degenbot-core`
during the scaffold task — drop if unused. The trait-definition move (§5) makes
these traits self-contained in pools (no `degenbot-rpc` dep — the RPC impls
that need `degenbot-rpc` stay in bot).

---

## 7. Cross-crate rewire map (consumers of `degenbot_bot::bot_core::*`)

When MOVES symbols relocate to `degenbot_pools`, these files update imports:

| file | context |
|---|---|
| `rust/crates/degenbot/src/lib.rs` | umbrella — replace `degenbot_bot::bot_core::{...}` re-exports for moved symbols with `degenbot_pools::{...}`; add `pub use degenbot_pools;` |
| `rust/crates/degenbot-python/src/lib.rs` | PyO3 cdylib root — `BotState`/`PoolEntry`/`Register*PoolParams` etc. re-exports |
| `rust/crates/degenbot-python/src/bot/mod.rs` | bot module root |
| `rust/crates/degenbot-python/src/bot/pool.rs` | PyLiquidityPool handle — reads `V2PoolState`/identity, `PoolEntry` (lines ~919, ~1291) |
| `rust/crates/degenbot-python/src/bot/pump.rs` | pump wrapper |
| `rust/crates/degenbot-python/src/bot/subscriber.rs` | subscriber |
| `rust/crates/degenbot-python/src/bot/token.rs` | `TokenEntry` |
| `rust/crates/degenbot-python/src/bot/engine/mod.rs` | engine module root |
| `rust/crates/degenbot-python/src/bot/engine/register.rs` | register_*_pool wrappers |
| `rust/crates/degenbot-python/src/bot/engine/snapshot.rs` | snapshot load |
| `rust/crates/degenbot-python/src/bot/engine/verify.rs` | verify |

Also `rust/crates/degenbot-bot/Cargo.toml` adds `degenbot-pools` dep; the
umbrella `rust/crates/degenbot/Cargo.toml` adds it too. `BotState`'s methods
that delegate to the value layer get `use degenbot_pools::...` internally.

---

## 8. Risk callouts (1-3 highest-risk)

1. **I/O trait-object fields on state structs (§5).** Recommendation: move the
   3 trait *definitions* + `StaticRateProvider` to pools, keep RPC/DB impls in
   bot. This is the single decision that gates whether the state structs can
   move at all. If rejected (cyclic dep avoided some other way), the whole
   extraction reshapes. **Resolve first**, before any state struct moves.
   Fallback (Alternative B — remove the fields, parallel bot-side registry) is
   the long-term-cleanest but ~10+ extra touch points; defer.

2. **`BotState` API preservation.** The PyO3 layer (`degenbot-python`) and the
   Python companion depend on the ~110-method `BotState` surface. The extraction
   must NOT change `BotState`'s public method signatures — it only changes what
   they internally delegate to. Risk: a moved value function's error type
   (`SimulateSwapError`) is now in `degenbot_pools`, so `BotState::calculate_*_miss_aware`
   must re-import it; if the PyO3 mapper pattern-matches `SimulateSwapError`
   variants, those imports update too. Verify the PyO3 error mapping in
   `degenbot-python/src/bot/engine/*.rs` still compiles after the move.

3. **`PoolEntry` location.** `PoolEntry` (the tagged enum) is referenced by
   `BotState`'s registry (`HashMap<u64, PoolEntry>`) AND by the value-only
   simulate dispatch. It MOVES to pools, with `BotState` becoming
   `HashMap<u64, degenbot_pools::PoolEntry>`. This is mechanical but touches
   every `match self.pools.get(&pool_id)? { PoolEntry::V2(..) => ... }` arm in
   bot (~dozens). Verify the `#[derive(Clone, Debug)]` shape and that no
   `PoolEntry` variant carries a bot-only type after the trait move (§5) — once
   the fetcher/provider traits are in pools, all variants are pools-resident.

---

## 9. Next (tasks to create under epic `USPN7M`)

From this inventory, the detailed extraction tasks:

1. *Scaffold `degenbot-pools` crate* — `Cargo.toml` (deps §6), empty `lib.rs`,
   workspace + no-pyro3 loop + release profile. Mirror `degenbot-v2-math` scaffold.
2. *Move the 3 trait definitions + `StaticRateProvider` to pools* (§5) — UNBLOCKS
   the state-struct moves. The RPC/DB impls stay in bot; verify they still impl
   the now-pools-defined traits (trait visibility: `pub` → fine cross-crate).
3. *Move `spec_bounds` + `state_history` + `tick_bitmap` + `tick_map` + `tick_fetch` value types* to pools — the leaf-ish value/data modules.
4. *Move the per-family state files* (`v2`/`v3`/`v4`/`curve`/`balancer_*`/`aerodrome_v2`)
   + `PoolEntry` + `V3FamilyPool` + `TokenEntry` + `TickInfo` + `Register*PoolParams`/`*Error` — the bulk of the value surface.
5. *Move value-only swap sims* (`v3_simulate_swap`, `v4_simulate_swap`, V2 CP
   dispatch, Curve/Balancer sims, `SimulateSwapError`, `V3SwapOutcome`) to pools.
6. *Rewire `degenbot-bot` + `degenbot-python` + umbrella* (§7) — import paths;
   `BotState` methods delegate to `degenbot_pools::simulate_*`. Verify `just
   test` + `just check-no-pyo3-in-cores` green.

Sequence 2 → 3 → 4 → 5 → 6 as a dependency chain (each gates the next). Task 1
independent. The `degenbot-solvers` epic (later) consumes the moved
`CpHopState`/`IntV3TickRangeHop` from pools.