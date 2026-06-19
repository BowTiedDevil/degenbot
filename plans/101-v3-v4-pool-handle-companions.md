# Plan 101: V3/V4 pool companions over `PyLiquidityPool` (ADR-005 slices 8 + 9)

## Overview

Collapse the V3/V4 "two parallel state representations" pattern by rewriting
`UniswapV3Pool` and `UniswapV4Pool` as thin Python companions over the existing
`PyLiquidityPool` handle (the V2 template from ADR-005 slice 4/5). The Rust
`BotState` becomes the single source of truth for V3/V4 mutable state (scalars,
tick data, reorg journal) — exactly as it already is for V2. The backrun example
exercises the new topology end-to-end: pools register **once** via
`PyBot.register_v3/v4_pool` (which writes to the shared `BotState`), the engine
reads through that same state via `register_and_solve_path`, and the example
drops its ~10-field re-registration + double DB-snapshot load.

Encoding (`encode_cmd_stream`, `_encode_cmd_*`) stays Python — per ADR-006
"Consequences," the future-Rust-owned-encoding path is reachable *after* pool
companions exist; doing it now would have it reading throwaway Python pool
fields. Tracked as a follow-up.

Scope is **1a (full thin-handle rewrite)** per user direction; **encoding
deferred** per user direction.

## Problem

### Deletion test

If you deleted the Python-side `ConcentratedLiquidityStateManager` ownership on
`UniswapV3Pool`/`UniswapV4Pool`, the engine's own V3/V4 state (snapshot store +
buffers + apply path), the `_v3_keys`/`_v4_keys` address→pool_id maps in the
example, and the example's `engine.register_v3_pool`/`engine.register_v4_pool`
calls — would the system keep working? **Yes.** The engine already loads ticks
from its own snapshot store (`v3_snapshot.take(&addr)` in
`rust/src/optimizers/uniswap_engine/py_binding.rs:1069`), already applies buffered
events via `apply_backfill_buffer_v3`/`apply_pump_buffer_v3`, already verifies
snapshot+backfill state against on-chain RPC, and already solves via
`int_solve_cl_path` in pure Rust. The Python V3/V4 pool's own CL state is loaded,
used as a field source for re-registration + for the legacy `simulate_swap` API,
then unused by the hot loop. Deletion concentrates the *single* mutable-state
representation in `BotState` (matching V2 exactly) rather than keeping two.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| V3/V4 immutable fields fed to the engine *out of* the Python pool, then re-written into shared `BotState` | `examples/eth_backrun_v2_v3_v4_rust.py:678` (`engine.register_v3_pool(address=…, sqrt_price_x96=…, …)`) reads `pool.sqrt_price_x96`/`pool.liquidity`/`pool.tick` — fields the *builder already fetched and wrote into the Python pool's `_state_mgr`* | Two parallel V3/V4 state representations; the Python pool's CL state is loaded, never used by the hot loop, then `gc.collect()`'d |
| DB tick snapshot loaded twice — once into the Python pool (`V3BuilderBase.load_tick_snapshot` in `v3_pool_builder.py:188`), once into the engine (`stream_v3_snapshot_to_engine` in `main()`) | `src/degenbot/builders/v3_pool_builder.py:188`; `examples/eth_backrun_v2_v3_v4_rust.py` | Doubles snapshot-load time per pool; the engine copy is authoritative, the Python copy is throwaway |
| Python V3/V4 pools own a `ConcentratedLiquidityStateManager` with its own `StateCache`, journal, tick bitmap — a full second state machine | `src/degenbot/uniswap/v3_liquidity_pool.py:135` `self._state_mgr = ConcentratedLiquidityStateManager(…)`, `src/degenbot/uniswap/v4_liquidity_pool.py` mirror | Diverges from V2 design (which already delegates to `PyLiquidityPool`); reorg/discipline logic lives in two places |
| `_v3_keys`/`_v4_keys` address→pool_id maps maintained in the example, separate from V2's `_v2_keys` reading `pool._py_pool.pool_id` | `examples/eth_backrun_v2_v3_v4_rust.py:635-636` | V2 reads the canonical pool_id from the handle; V3/V4 reconstruct it via a redundant re-registration call |
| V4 hook + dynamic-fee filtering duplicated in Python; the Rust engine is permissive | `examples/eth_backrun_v2_v3_v4_rust.py` `register_v4_pool` AMOUNT_MODIFYING_HOOK_MASK + V4_DYNAMIC_FEE_FLAG | AGENTS.md V4 Hook Filtering section already specifies Python-side filtering; folding into `BotState::register_v4_pool`'s `Err(...)` path aligns with the per-pool construction-intent boundary |
| `UniswapV3Pool._tick_data_fetcher` / `UniswapV4Pool._tick_data_fetcher` — Python sparse-map on-chain fetch during simulation | `v3_liquidity_pool.py:238` | Couples Python simulation to Python I/O; in the new topology the engine owns tick data — the fetcher either routes to the engine's tick store or is dropped (simulation moves to engine solver) |

## Solution

The migration mirrors V2's completed ADR-005 slice 4/5 template (`LiquidityPool`
in `src/degenbot/uniswap/liquidity_pool.py:88`). The three layers stay identical;
only the V3/V4 companions + their Rust handle surface change.

### Slice 8a — Rust: `PyLiquidityPool` V3 handle surface + `PyBot.register_v3_pool` tick bundle

The V2 `PyLiquidityPool` (`rust/src/bot_core/py_liquidity_pool.rs:37`) exposes:
getters (`reserve0`, `reserve1`, `update_block`), `snapshot`, mutations
(`sync_reserves`), and journal ops (`discard_before_block`,
`restore_before_block`). V3 needs the same shape, keyed by `pool_id`, delegating
to `BotState`'s existing `V3PoolState` surface.

**New `PyLiquidityPool` pymethods (V3 family, pool_id-keyed):**

```rust
#[getter] fn sqrt_price_x96(&self, py) -> Py<PyAny>          // reads V3PoolState.sqrt_price_x96
#[getter] fn liquidity(&self) -> u128                         // V3PoolState.liquidity
#[getter] fn tick(&self) -> i32                               // V3PoolState.tick
#[getter] fn update_block(&self) -> u64                       // V3PoolState.update_block (V3 variant)
#[getter] fn fee(&self) -> u32                                 // V3PoolState.fee (immutable)
#[getter] fn tick_spacing(&self) -> i32                        // immutable
#[pyo3(signature = ())] fn snapshot_v3(&self, py) -> Option<Py<PyAny>>  // (sqrt_price, liquidity, tick, block) atomically under one read guard

fn apply_swap(&self, sqrt_price_x96, liquidity, tick, block_number) -> PyResult<()>  // BotState.apply_v3_swap_by_pool_id
fn apply_liquidity_update(&self, tick_lower, tick_upper, liquidity_delta, block_number) -> PyResult<()>  // BotState.apply_v3_liquidity_update_by_pool_id
fn update_tick_data(&self, tick_bitmap: dict, tick_data: dict, block: u64) -> PyResult<()>  // replaces the tick_data HashMap; mirrors UniswapV3Pool.update_tick_data

#[pyo3(signature = (block))] fn discard_before_block(&self, block) -> PyResult<()>  // BotState.v3_discard_before_block(pool_id, block)
#[pyo3(signature = (block))] fn restore_before_block(&self, py, block) -> PyResult<Option<Py<PyAny>>>  // BotState.v3_restore_before_block(pool_id, block)

#[getter] fn tick_data(&self, py) -> Py<PyAny>                // snapshot of {tick: (liquidity_gross, liquidity_net, block)} as a Python dict (deep-copied under one read guard — mirrors how the V2 snapshot returns a tuple)
#[getter] fn tick_bitmap(&self, py) -> Py<PyAny>             // snapshot of {word: (bitmap, block)}
```

**New `PyBot.register_v3_pool` acceptance of a tick bundle** (today it passes
`tick_data: HashMap::new()` at `py_bot.rs:335`):

```python
# Before — caller hands immutables only; tick_data resolves via the
# engine's separate load_v3_snapshot/stream_v3_snapshot_to_engine path
core.register_v3_pool(address=…, token0=…, token1=…, fee=…, tick_spacing=…,
                      factory=…, sqrt_price_x96=…, liquidity=…, tick=…, block=0)

# After — same signature (tick_data stays None on the builder path, the
# engine snapshot store remains the source of tick data) BUT register_v3_pool
# now returns a PyLiquidityPool handle (not just a u64)
handle: PyLiquidityPool = core.register_v3_pool(...)
handle.pool_id  # the u64
```

Decision: the engine's existing `load_v3_snapshot` / `v3_snapshot.take(&addr)`
snapshot store stays the *authority* for V3 tick data (it has the streamed binary
snapshot + buffer staged apply + verification plumbing). The Python V3 companion
reads tick data through `PyLiquidityPool.tick_data` (a snapshot copy under one
read lock — same atomicity contract as V2's `snapshot()`). The builder's
`load_tick_snapshot` Python-side DB call is **dropped** in slice 8c — the builder
fetches only immutables + slot0 + liquidity; ticks come from the engine snapshot
the example already loads.

### Slice 8b — Python: rewrite `UniswapV3Pool` over `PyLiquidityPool`

Constructor changes shape to match `LiquidityPool`'s companion pattern:

```python
# Before: standalone state owner
class UniswapV3Pool(PublisherMixin, V3PoolState, UniswapV2PoolCalc, AbstractLiquidityPool):
    def __init__(self, address, *, tick_bitmap=…, tick_data=…, sqrt_price_x96=…, tick=…, liquidity=…, state_block=…, …):
        self._state_mgr = ConcentratedLiquidityStateManager(initial_state=…, state_cache_depth=…)

# After: thin handle over PyLiquidityPool (matches LiquidityPool at liquidity_pool.py:88)
class UniswapV3Pool(PublisherMixin, V3PoolState, UniswapV3PoolCalc, AbstractLiquidityPool):
    def __init__(self, py_pool: PyLiquidityPool, *, address, token0, token1, factory, fee, tick_spacing,
                 chain_id=None, deployer_address=None, init_hash=None, tick_data_fetcher=None):
        self._py_pool = py_pool
        # immutables stay Python-side (matches V2: factory/fees/tokens stay on companion)
```

Property delegations (mirror V2's `state`/`reserves_token0`/`update_block`):

| Python property | Before | After |
|---|---|---|
| `liquidity` | `self._state_mgr.liquidity` | `self._py_pool.liquidity` |
| `sqrt_price_x96` | `self._state_mgr.sqrt_price_x96` | `self._py_pool.sqrt_price_x96` |
| `tick` | `self._state_mgr.tick` | `self._py_pool.tick` |
| `state` | `self._state_mgr.state` | builds `UniswapV3PoolState` from one `snapshot_v3()` atomic tuple |
| `tick_bitmap` / `tick_data` | `self._state_mgr.tick_*` | `self._py_pool.tick_bitmap` / `self._py_pool.tick_data` (snapshots) |
| `update_block` | `self.state.block` | `self._py_pool.update_block` |

Mutation delegations:

| Method | Before | After |
|---|---|---|
| `external_update` | `self._state_mgr.push_state(...)` | `self._py_pool.apply_swap(...)` (scalars only; tick data unchanged by Swap) |
| `update_liquidity_map` | `self._state_mgr.push_state(...)` + Python tick bitmap ops | `self._py_pool.apply_liquidity_update(tick_lower, tick_upper, liquidity_delta, block)` — let Rust do the tick bitmap mutation (it already does, in `BotState::apply_v3_liquidity_update`) |
| `update_tick_data` | `self._state_mgr.push_state(...)` | `self._py_pool.update_tick_data(tick_bitmap, tick_data, block)` |
| `discard_states_before_block` | `self._state_mgr.discard_states_before_block(...)` | `self._py_pool.discard_before_block(block)` |
| `restore_state_before_block` | `self._state_mgr.restore_state_before_block(...)` | `self._py_pool.restore_before_block(block)` |

Simulation (`simulate_exact_input_swap` / `_calculate_swap`): unchanged logic —
reads `self.state` / `self.tick_data` / `self.tick_bitmap` (now handle-backed
snapshots). `_state_cache` property + `state_cache_depth` parameter: **dropped**
(journal lives in Rust now; matches V2 which has no `_state_cache`).

`_tick_data_fetcher` for sparse maps: kept as a Python-callable for simulation
backfill (rare path) — but in slice 8c the example moves entirely to the engine's
tracked coverage, so this path collapses.

### Slice 8c — Python: V3 builder routes through PyBot; example drops double registration

V3 builder's `build()` (`src/degenbot/builders/v3_pool_builder.py`) — change the
pool construction block at line ~280:

```python
# Before
pool = pool_class(address=…, tick_bitmap=tick_bitmap_arg, tick_data=tick_data_arg,
                  sqrt_price_x96=…, liquidity=…, tick=…, state_block=…, …)
self._pools.add(pool=pool, chain_id=chain_id, pool_address=pool.address)
# (Python pool owns its own _state_mgr; engine.register_v3_pool re-writes fields later)

# After
handle = io.py_bot.register_v3_pool(
    address=pool_address, token0=token0.address, token1=token1.address,
    fee=fee, tick_spacing=tick_spacing_for_pool, factory=factory,
    sqrt_price_x96=int(sqrt_price_x96), tick=int(tick), liquidity=int(liquidity),
    block=state_block,
)
pool = pool_class(handle, address=pool_address, token0=token0, token1=token1,
                  factory=factory, fee=fee, tick_spacing=tick_spacing_for_pool,
                  chain_id=chain_id, deployer_address=deployer, init_hash=init_hash,
                  tick_data_fetcher=self._make_tick_data_fetcher(pool_address, chain_id, io=io))
self._pools.add(pool=pool, chain_id=chain_id, pool_address=pool.address)
```

The DB tick snapshot load (`V3BuilderBase.load_tick_snapshot`) — **dropped** from
the builder; the engine's `load_v3_snapshot` + `stream_v3_snapshot_to_engine`
(loaded by `main()`) remains the sole tick source. The on-chain RPC fallback
(`tickBitmap(int16)` block at `v3_pool_builder.py:215`) stays — it's a sparse-map
fallback, not the snapshot path.

Example `EngineRegistry.register_v3_pool` (`examples/eth_backrun_v2_v3_v4_rust.py:663`)
— collapses to mirror V2's `register_v2_pool`:

```python
# Before
key = self.engine.register_v3_pool(address=pool.address, token0=…, fee=…,
                                   sqrt_price_x96=pool.sqrt_price_x96, …)
self._v3_keys[pool.address] = key

# After — the V3 pool is ALREADY registered in shared BotState by the builder
key = pool._py_pool.pool_id
self._v3_keys[pool.address] = key
```

### Slice 9a — Rust: `PyLiquidityPool` V4 handle surface + hook/dynamic-fee filtering in `BotState::register_v4_pool`

Mirror of 8a for V4 + fold the AMOUNT_MODIFYING_HOOK_MASK (0xCC) +
V4_DYNAMIC_FEE_FLAG (0x100000) filtering into `BotState::register_v4_pool`'s
`Result<u64, String>` `Err` path (today `register_v4_pool` already returns
`Result`; we make it `Err` for filtered pools). The V4 scalar getters
(`sqrt_price_x96`, `liquidity`, `tick`, `update_block`) + `snapshot_v4` +
mutations `apply_swap` / `apply_liquidity_update` + journal ops land on
`PyLiquidityPool` (V4 family, keyed by `pool_id`).

### Slice 9b — Python: rewrite `UniswapV4Pool` over `PyLiquidityPool`

Mirror of 8b. `UniswapV4Pool._state_mgr` drops; `hook_address` / `pool_id` /
`fee` / `tick_spacing` stay immutable on the Python companion; mutable scalars +
tick data + journal delegate to `PyLiquidityPool`.

### Slice 9c — Python: V4 builder routes through PyBot + example drops Python-side hook filter

Mirror of 8c. The example's `register_v4_pool` Python `AMOUNT_MODIFYING_HOOK_MASK`
+ `V4_DYNAMIC_FEE_FLAG` blocks **delete** (Rust rejects in slice 9a). The
`engine.register_v4_pool` call collapses to `pool._py_pool.pool_id`.

### Slice 10 — Example cleanup

- Drop `_v3_keys` address→pool_id map (read `pool._py_pool.pool_id` directly,
  matching V2's already-migrated shape)
- Drop `_v4_keys` (read `pool._py_pool.pool_id` — keyed by pool_id_hex for
  event routing, but the value is just the handle's pool_id)
- Drop the redundant `gc.collect()` after pool construction (no Python-side CL
  state to GC)
- Drop the `_v2/_v3/_v4_keys` comment block at line 639 (V3/V4 now match V2)
- `resolve_directions` and `find_paths_async` stay Python (I/O boundaries, per
  ADR-006)
- Simulation orchestration, tx lifecycle, monitoring stay Python

### Design decisions

- **Full thin-handle rewrite (1a) not registration-descriptor (1b)** — per user
  direction; the on-title ADR slice. Highest correctness; the encoding path
  becomes a later PR (slice 10+ would be the natural seam).
- **Tick data stays on the engine snapshot store, not the builder path** — the
  engine already has `load_v3_snapshot` + `stream_v3_snapshot_to_engine` +
  staged buffer apply + on-chain verify plumbing in
  `rust/src/optimizers/uniswap_engine/py_binding.rs:279`. Moving tick data
  loading onto the builder would duplicate this. The V3/V4 companion reads tick
  data through `PyLiquidityPool.tick_data` (snapshot copy under one read lock).
- **`tick_data` / `tick_bitmap` getters return Python-dict snapshots** — matches
  the V2 `snapshot()` atomicity contract (one read guard); simulation code
  (`_calculate_swap`) gets a deep-copy it can freely mutate without corrupting
  Rust state. Same pattern as V2's `state` property building from `snapshot()`.
- **`_state_cache` / `state_cache_depth` dropped on V3/V4 companions** — the
  `StateCache` temporal-navigation layer lives in Rust now (journal +
  discard/restore). V2 already has no `_state_cache`; V3/V4 follow.
- **Hook/dynamic-fee filtering folds into `BotState::register_v4_pool`'s `Err`
  path** — AGENTS.md V4 Hook Filtering section already specifies "Python before
  `register_v4_pool`"; folding lifts it to the construction-intent boundary per
  ADR-006 D3 ("two intents want two methods"). The Rust engine is permissive
  today; the filter becomes the BotState-level reject.
- **`_tick_data_fetcher` (Python callable) stays** for the rare sparse-map
  simulation backfill — existing tests use it. Slice 8c's example migration
  moves the example to the engine's tracked coverage, collapsing the path there.

## Files Involved

**Primary:**
- `rust/src/bot_core/py_liquidity_pool.rs` — add V3 + V4 family of pymethods
  (getters, snapshot, mutations, journal ops)
- `rust/src/bot_core/py_bot.rs` — `register_v3_pool`/`register_v4_pool` return
  `PyLiquidityPool` handles; V4 hook/dynamic-fee filtering in `register_v4_pool`
- `rust/src/bot_core/mod.rs` — add `apply_v3_swap_by_pool_id`,
  `apply_v3_liquidity_update_by_pool_id` (pool_id-keyed variants of the existing
  address-keyed applys), V4 mirror, tick_data getters by pool_id
- `src/degenbot/uniswap/v3_liquidity_pool.py` — rewrite constructor + property
  delegations + mutation methods over `PyLiquidityPool`
- `src/degenbot/uniswap/v4_liquidity_pool.py` — mirror rewrite
- `src/degenbot/builders/v3_pool_builder.py` — drop DB tick snapshot load; call
  `py_bot.register_v3_pool` → hand `PyLiquidityPool` to constructor
- `src/degenbot/builders/v4_pool_builder.py` — mirror
- `examples/eth_backrun_v2_v3_v4_rust.py` — collapse
  `EngineRegistry.register_v3/v4_pool`, drop `_v3_keys`/`_v4_keys` Python-side
  hook filter, drop `gc.collect()`

**Secondary:**
- `tests/uniswap/v3/test_v3_pool_io_free.py` — update construction (now
  `PyLiquidityPool`-handed); the existing "I/O-free constructor" assertion stays
- `tests/uniswap/v4/test_v4_pool_io_free.py` — mirror
- `tests/arbitrage/test_optimizers/test_shared_state_topology.py` — extend with
  a V3 (and V4) round-trip test mirroring the existing V2 test (full §17 closure
  on the V3 path)
- `rust/CONTEXT.md` / `src/degenbot/uniswap/CONTEXT.md` — update where the V3/V4
  companion now reads state from

**No change needed:**
- `src/degenbot/uniswap/concentrated/v3_simulator.py` — pure-math
  (`calculate_swap(snapshot, zero_for_one, …)`); reads from `LiquidityMapSnapshot`
  that the Python companion builds from `snapshot_v3()` + `tick_data` (now
  handle-backed) — interface unchanged
- `src/degenbot/arbitrage/encoding/eth_backrun_helpers.py` + the `_encode_cmd_*`
  — encoding stays Python (deferred per user direction; the encoders will read
  from thin handles in a follow-up, no state copy)
- `find_paths_async` — DB DFS, I/O boundary, per ADR-006 stays Python

## Implementation Order

Numbered vertical slices. Each slice leaves the test suite green.

### Slice 8a: Rust `PyLiquidityPool` V3 surface + `PyBot.register_v3_pool` return-handle

1. Add V3 family of pymethods to `PyLiquidityPool` (getters, `snapshot_v3`,
   `apply_swap`, `apply_liquidity_update`, `update_tick_data`, journal ops,
   `tick_data`/`tick_bitmap` snapshot getters) delegating to `BotState`
2. Add `BotState::apply_v3_swap_by_pool_id` / `apply_v3_liquidity_update_by_pool_id`
   (pool_id-keyed — V3 doesn't change the address; pool_id is the canonical key)
3. (No change to `register_v3_pool` return type — V2's pattern holds: it returns
   `u64`; `PyBot.get_pool(pool_id)` returns the family-agnostic `PyLiquidityPool`
   handle. The V2 builder uses this two-step pattern at `v2_pool_builder.py:146-160`.
   The V3 builder mirrors in slice 8c.)
4. **RED→GREEN**: extend `test_shared_state_topology.py` with a V3 round-trip:
   register V3 via `core.register_v3_pool` → `core.get_pool(pool_id)` →
   `engine.register_and_solve_path` → live `apply_swap` via
   `pool_handle.apply_swap(...)` → re-solve reads updated scalars (structurally
   identical to the existing V2 `test_live_state_write...` test, V3 family)
5. Run: `just test-rust` + `just test-python` — expect green

### Slice 8b: Python `UniswapV3Pool` rewrite over `PyLiquidityPool`

1. Rewrite constructor — `py_pool: PyLiquidityPool` first positional, immutables
   as kwargs (mirror `LiquidityPool.__init__`)
2. Replace property bodies + mutation methods per the table above; drop
   `_state_mgr` / `_state_cache` / `state_cache_depth`
3. Update existing V3 tests (`test_v3_pool_io_free.py`,
   `test_uniswap_v3_liquidity_pool.py`) — construction now requires a
   `PyLiquidityPool`; `make_v3_pool` helper added
4. **RED→GREEN** per behavior, vertical: one property delegation at a time
5. Run: `just test-python -- tests/uniswap/v3/` — expect green

### Slice 8c: V3 builder routes through PyBot + example drops double registration

1. V3 builder `build()` calls `py_bot.register_v3_pool` → hand `PyLiquidityPool`
   to the pool constructor; drop the `load_tick_snapshot` DB path
2. Example `EngineRegistry.register_v3_pool` collapses to
   `self._v3_keys[pool.address] = pool._py_pool.pool_id` (mirror V2)
3. Run: `just test-python` + smoke `uv run python examples/eth_backrun_v2_v3_v4_rust.py`
   — expect green, end-to-end run reaches `solve_all_paths`

### Slice 9a: Rust V4 surface + hook/dynamic-fee filtering in `BotState::register_v4_pool`

1. Mirror 8a for V4 (`snapshot_v4`, `apply_swap`, `apply_liquidity_update`, etc.)
2. Fold `AMOUNT_MODIFYING_HOOK_MASK` (0xCC) + `V4_DYNAMIC_FEE_FLAG` (0x100000)
   check into `BotState::register_v4_pool`'s `Err` path
3. **RED→GREEN**: V4 hook-filter test (rejected pool → `ValueError` from
   `register_v4_pool`) + V4 round-trip topology test
4. Run: `just test-rust` + `just test-python`

### Slice 9b: Python `UniswapV4Pool` rewrite

1. Mirror 8b for V4
2. Run: `just test-python -- tests/uniswap/v4/`

### Slice 9c: V4 builder + example drop Python hook filter

1. V4 builder routes through `py_bot.register_v4_pool`
2. Example's Python `AMOUNT_MODIFYING_HOOK_MASK` + `V4_DYNAMIC_FEE_FLAG` blocks
   delete
3. Run: `just test-python` + smoke run example → expect end-to-end green

### Slice 10: Example cleanup + validate

1. Drop `_v3_keys`/`_v4_keys` maps (read `pool._py_pool.pool_id` directly)
2. Drop the redundant `gc.collect()` after pool construction
3. Run `just lint` + `just test-all`
4. Update `rust/CONTEXT.md` + `src/degenbot/uniswap/CONTEXT.md` if terminology
   changed (V3/V4 companion state-owner language)
5. Mark ergo `R3ZAUV` (slice 8) + `DULSSU` (slice 9) done; partial-close
   `RRXSNZ` (Python mutable state eliminated for engine-managed pools on the
   V3/V4 backrun path)

## Testing

### Per-slice test runs

Each slice runs `just test-rust` + `just test-python`. Rust surface lands
behind Python tests that exercise the new `PyLiquidityPool` methods.

### New unit tests

```python
# tests/arbitrage/test_optimizers/test_shared_state_topology.py
# (extend existing file — V2 acceptance tests already there)

class TestSharedStateTopologyV3:
    """UniswapV3Pool over PyLiquidityPool — V3-specific §17 closure."""

    def test_engine_adopts_shared_bot_state_v3(self) -> None:
        """core.register_v3_pool writes to shared BotState; engine reads it."""

    def test_live_state_write_is_visible_to_engine_re_solve_v3(self) -> None:
        """pool_handle.apply_swap(...) → engine.solve_all_paths reads new scalars."""

    def test_dispatch_log_drives_full_pump_to_solve_loop_v3(self) -> None:
        """dispatch_log of a V3 Swap → engine re-solve reads new state."""

# Mirror for V4 (with a hook-filter rejection test)
```

### Integration tests

- `tests/uniswap/v3/test_v3_pool_io_free.py` — already verifies I/O-free
  construction; updated to the `PyLiquidityPool`-handed constructor
- `tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py` — simulation behavior
  tests stay green after property delegations rewrite
- `tests/uniswap/v4/test_v4_pool_io_free.py` + `test_uniswap_v4_liquidity_pool.py`
  — V4 mirrors

### End-to-end

`uv run python examples/eth_backrun_v2_v3_v4_rust.py` reaches startup →
backfill → `build_paths` → simulation, matching the current end-to-end state —
without the duplicate V3/V4 state load.

## Benefits

- **Locality**: V3/V4 mutable state (scalars, tick data, journal) lives in one
  place — `BotState` — matching V2 exactly. Reorg, discard, restore discipline
  no longer smeared across Python CL state manager + Rust journal.
- **Depth**: `PyLiquidityPool` becomes the deep V2/V3/V4-over-CL-state seam;
  the Python companions are uniformly thin.
- **Leverage**: encoding layer (deferred) gains a clean read surface — when it
  migrates to Rust, it reads the same `BotState` the pump wrote (ADR-006
  "Consequences" — the §17 closure the encoding migration needs).
- **Closure of `RRXSNZ`** on the V3/V4 backrun path: "Eliminate Python pool
  mutable state for engine-managed pools" — pool state lives in Rust, the
  Python companion holds only immutables + the handle.

## Risks

- **Tick data snapshot-copy cost on every `simulate_swap`**: `tick_data` getter
  deep-copies the HashMap under a read lock. For an active V3 pool with ~thousands of initialized ticks, this is a Python-dict build cost per simulation.
  *Mitigation*: the hot loop uses the engine's Rust solver
  (`int_solve_cl_path`), not `UniswapV3Pool.simulate_swap`; the snapshot copy
  only fires on explicit Python simulation (rare). If a hot Python simulation
  path emerges, expose a tick-range iterator instead of a full dict.
- **V3 builder's `update()` staticmethod reads Python pool fields then writes
  them back** — the re-fetch (slot0 + liquidity) is unchanged, but
  `pool.external_update` becomes `pool._py_pool.apply_swap`. Verified: V2's
  `V2PoolBuilder.update` already does this pattern; low risk.
- **Engine's `engine.register_v3_pool` keeps existing for back-compat?** —
  slice 8c leaves the method present (`UniswapArbEngine` PyO3 surface) since
  tests may use it; the example simply stops calling it. Decision: do *not*
  delete `engine.register_v3_pool` in this plan — ADR-006 D3's deletion
  already landed; the wrapper method delegates to `BotState::register_v3_pool`
  (writes to the shared core). Slice 8c just routes the example through
  `PyBot.register_v3_pool` instead. (A future slice can delete the engine
  wrapper if it has no callers.)
- **V4 hook filtering move could regress error messages** — the engine raises
  `String` errors; Python's `register_v4_pool` raises `ValueError(msg)`.
  *Mitigation*: map the `Err(String)` to a `pyo3::exceptions::PyValueError`
  with the same message format.

## Relationship to Other Plans

- **ADR-005 epic `XQ5UX6`** ("Polars three-layer migration — master plan & slice
  index"): this plan implements slices 8 (`R3ZAUV`) and 9 (`DULSSU`).
- **ADR-006 epic `B4Y5GN`** ("Bot as per-chain orchestrator"): this plan extends
  the backrun example migration (`56AJHG`, completed) to the V3/V4 family — the
  V2 path was already done; V3/V4 close the §17 stale-state caveat on those
  pool types.
- **Plan 100 (completed)** "BotCore State Layer & Engine Dissolution (ADR-003)":
  established `BotState` as the V3/V4 state owner peer to `UniswapEngine`. This
  plan is the *Python-companion* consequence — making `UniswapV3Pool`/
  `UniswapV4Pool` actually read that state.
- **`RRXSNZ` "Eliminate Python pool mutable state for engine-managed pools"**:
  this plan largely satisfies it on the V3/V4 backrun path; `6BPRAH` (slice 10:
  UniswapEngine lock unification) is unaffected (already true post-ADR-006 D2).

## Status

[ ] Slice 8a: Rust `PyLiquidityPool` V3 surface + `PyBot.register_v3_pool` return-handle
[ ] Slice 8b: Python `UniswapV3Pool` rewrite over `PyLiquidityPool`
[ ] Slice 8c: V3 builder routes through PyBot + example drops double registration
[ ] Slice 9a: Rust V4 surface + hook/dynamic-fee filtering in `BotState::register_v4_pool`
[ ] Slice 9b: Python `UniswapV4Pool` rewrite
[ ] Slice 9c: V4 builder + example drop Python hook filter
[ ] Slice 10: example cleanup + validate + CONTEXT.md updates
