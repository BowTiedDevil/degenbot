# Rust Extension Context

## Language

**GIL Discipline**: The set of rules governing when the Global Interpreter Lock is held vs released in PyO3-wrapped functions. Sub-μs pure-compute functions hold the GIL; I/O-bound functions release it via `py.detach()`.
_Avoid_: GIL management, thread safety, locking strategy

**SAFETY Comment**: A `// SAFETY:` comment on every `Python::attach()` call site documenting why no circular wait (deadlock) can occur — specifically that the sync path releases the GIL before `block_on()` and the async path's `attach()` runs on Tokio worker threads while Python releases the GIL during I/O polling.
_Avoid_: thread safety comment, lock comment

**Type Cache Key**: `Arc<[Arc<str>]>` — the lookup key for the two-level `CachedAbiTypes` intern. `Arc<str>` elements are interned via `TYPE_STR_INTERNER`; the outer `Arc<[...]>` is a thin pointer for cheap comparison and `Borrow` compatibility with `LruCache`.
_Avoid_: cache key, type key, intern key

**String Interner**: `TYPE_STR_INTERNER` — a global `LazyLock<Mutex<HashMap<String, Arc<str>>>>` that deduplicates the ~20 Solidity/EVM type strings (e.g., `"uint256"`, `"address"`) into `Arc<str>` values. Eliminates per-call `String` allocation.
_Avoid_: string cache, type string pool

**drain_raw()**: A pure-Rust method on `SubscriptionHandle` that swaps the double buffer and returns `RawDrainResult` without touching the GIL. The Python-facing `drain_buffer()` wraps it with `Python::attach()`.
_Avoid_: raw drain, buffer swap

**IntHopState**: The solver's integer hop state carrying pre-converted U512 fields (`fee_over_fee_scale`, `scaled_reserve_in`, `scaled_reserve_out`). Constructed once from U256 values; `swap()` and `compute_int_mobius_coefficients()` use U512 arithmetic directly, eliminating per-call conversions. **Live construction site:** the V2 path builds `IntHopState` per-solve and passes it to `exact_mobius_solve`; the V3/V4 path uses `IntV3TickRangeSequence` (built by `build_int_v3_sequence` / `build_int_v4_sequence` on `V3PoolState` / `V4PoolState`) instead — direct `IntHopState` construction appears only in tests for V3/V4. Not stored in a per-pool LRU (`PyPoolCache` was deleted — see {PyPoolCache}).
_Avoid_: hop state, integer state, mobius state

**PyPoolCache** *(removed)*: Formerly `parking_lot::Mutex<LruCache<u64, IntHopState>>` — a bounded (10K entry) LRU cache keyed by pool ID, replacing the former unbounded `HashMap`. Deleted under ADR-003's "Legacy solver path retirement: delete, not migrate", along with `RustPoolCache` / `RustIntHopState` / `RustArbResult`. The live memoization seam is now the per-pool `cached_tick_ranges: parking_lot::Mutex<TickRangeCache>` field on `V3PoolState` / `V4PoolState`, consumed by `build_int_v3_sequence` / `build_int_v4_sequence` and invalidated on every `apply_*`. Entry retained only to name what was removed; see ADR-004.
_Avoid_: pool cache, solver cache, rust cache

**V2BlockEngine**: A pure-Rust struct that owns the full per-block V2 arbitrage lifecycle: Sync event decoding, pool state updates (`apply_sync`), path resolution, and Mobius solver dispatch. Python participates only in initial construction (`register_pool`, `register_path`) and reading results (`latest_results`). Registration is frozen after `start()`.
_Avoid_: V2 engine, block processor, arbitrage engine

**V2EnginePump**: A standalone async task that drives `V2BlockEngine`. Owns its own `AlloyProvider` (created from an RPC URL string), subscribes to block headers via WS, fetches Sync logs via `eth_getLogs` on each new block, and calls `engine.process_block()`. No dependency on the Python subscription infrastructure. Superseded by `UniswapEnginePump` for mixed-protocol deployments.
_Avoid_: pump, block subscriber, event driver

**V2_SYNC_TOPIC**: `0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1` — the Keccak256 hash of the Uniswap V2 `Sync(uint112,uint112)` event signature. The Rust-side constant matches the Python `V2_SYNC_TOPIC` in `log_decoders.py`.
_Avoid_: sync topic, sync hash, V2 sync signature

**Dual-Orientation Registration** *(legacy sub-engine pattern)*: Each `register_pool()` call created two `IntHopState` entries — forward (reserve0→reserve1) and reverse (reserve1→reserve0) — mirroring `ArbPoolCacheAdapter`'s pattern. `apply_sync()` updated both from the same Sync event and returned the forward pool key. Under ADR-003 this storage pattern leaves `Bot` (state is stored once); orientation is resolved at solve time (see **Swap Orientation**). The term survives only to name what was removed.
_Avoid_: double registration, mirror entry, forward-reverse pair

**Swap Orientation**: The direction in which a path consumes a pool — forward (`zero_for_one`: reserve0→reserve1) or reverse (`!zero_for_one`: reserve1→reserve0). A solver-dispatch concern, not a state-storage concern. Under ADR-003 `Bot` stores one `PoolEntry` per pool; `resolve_path` derives the forward or reverse `IntHopState` per hop from the pool's raw reserves and the path's `zero_for_one` flag. Eliminates the legacy dual-orientation *storage* of one entry per direction.
_Avoid_: direction, side, mirror orientation

**Pool's Authority Over Its Own Math**: The principle that a pool instance owns both its state and its single-pool swap math (e.g. `calculate_tokens_out_from_tokens_in` / `Bot::calculate_tokens_out`). A solve engine reads pool state **by reference** for path-level optimization (Mobius) or threads pool-to-pool by calling each pool's swap calc, but never mutates pool state arbitrarily during a solve. Pool state changes only through recognized event-application methods (`apply_swap`, `apply_liquidity_update`) that push a delta to the Reorg Journal. Mirrors the Python `UniswapLpCycle` pattern (iterates `self.swap_pools`, calls `pool.calculate_tokens_out_from_tokens_in` per hop, threads output→next input, never mutates). The Rust consequence: `UniswapEngine` reads `Bot` state via reference-returning accessors; only `LiquidityMap.apply_*` methods mutate, and every mutation journals a delta.
_Avoid_: separation, read/write split, pool ownership pattern

**PyV2ArbEngine**: The PyO3 wrapper class (exposed as `V2ArbEngine` in Python) that holds an `Arc<parking_lot::Mutex<V2BlockEngine>>` plus a shutdown flag and pump handle. The `start(rpc_url)` method freezes registration and spawns the `V2EnginePump`; `stop()` sets the shutdown flag and aborts the pump task. For testing, `freeze()` freezes registration without starting the pump, and `process_logs()` drives the engine synchronously.
_Avoid_: Python engine, engine wrapper, arb engine pyclass

**Bot** (formerly `BotCore`): The single owner of runtime pool and token state — V2 reserves/fees, V3/V4 sqrt_price/liquidity/tick/tick_data, token metadata (address, decimals, symbol, name), and reorg-rollback journals. A **peer module** to `UniswapEngine` (ADR-003): the engine holds `Arc<Mutex<Bot>>` and reads/writes state through it, never through private per-engine stores. Lock order when nested is engine-then-core; Python-facing `PyBot` methods take core alone and never call into the engine (the rule that keeps the deadlock surface empty). All data lives in Rust `HashMap`s; Python holds thin `PyO3` handles (`PyLiquidityPool` carrying a `pool_id` key, `PyErc20Token` carrying an `Address` key — completed in Slice 5). Owns the per-pool swap math (`calculate_tokens_out`/`calculate_tokens_in` for V2 constant-product + V3/V4 concentrated-liquidity via `v3_simulate_swap`/`v4_simulate_swap`, mirroring Python's `calculate_tokens_out_from_tokens_in`) and per-pool swap encoding (`encode_swap`); multi-pool math (path optimization) does not belong here. **Implementation status (Plan 100, complete):** V2/V3/V4 state all live here on the live path (`register_v2_pool`/`register_v3_pool`/`register_v4_pool`, `apply_v2_sync`/`apply_v3_swap`/`apply_v4_swap` + per-variant liquidity-update + buffer/drain, `get_v2_pool_state`/`get_v3_pool`/`get_v4_pool`, `restore_all_pools_before_block`). The block engines (`V2BlockEngine`, `V3BlockEngine`, `V4BlockEngine`) are **dissolved** — `UniswapEngine` holds `core: Arc<Mutex<Bot>>` only and reads/writes all pool state through it. The standalone `LiquidityMap` generic was not extracted: the inline-`PoolEntry` + `Bot::apply_*` pattern suited V2/V3/V4 without forcing a generic (per the ADR's no-abstraction-against-sample-of-one discipline; deferred until a third family like Curve ports). See {Polars-Inspired Three-Layer Architecture} (ADR-005) for the FFI topology; `UniswapEngine` still holds its own `Arc<Mutex<Bot>>` pending unification.
_Avoid_: core, bot state, rust state owner

**PoolEntry**: An enum with one variant per pool family (`V2(V2PoolState)`, future `V3`, `Curve`, `BalancerWeighted`, `BalancerStable`). Calculation methods match on the variant — no trait objects, no vtable dispatch.
_Avoid_: pool enum, pool type enum

**V2PoolState**: The Rust-owned state for a Uniswap V2 pool: address, token0/token1, reserves, per-direction fee parameters, factory. Replaces Python's `LiquidityPool._state_cache` (formerly `UniswapV2Pool`, renamed in ADR-005 slice 7 step 1) — data lives in `Bot.pools`, not in a Python object.
_Avoid_: v2 state, v2 pool data

**RegisterV2PoolParams**: A parameter struct for `Bot::register_v2_pool()` — bundles address, token pair, reserves, per-direction fees, and factory into one argument (replaces 9 positional parameters, satisfies `clippy::too_many_arguments`).
_Avoid_: v2 params, pool params

**Polars-Inspired Three-Layer Architecture**: The FFI topology for stateful Rust-owned resources (ADR-005): a `#[pyclass]` wrapper (`PyBot`) holds `Arc<parking_lot::RwLock<Bot>>` and *is* the sharing mechanism; thin handles (`PyLiquidityPool`/`PyErc20Token`) clone that `Arc` so N Python objects reference one Rust-owned core; the Python `Bot` session constructs and owns the wrapper (`self._py_bot = PyBot()`). Read methods take a read guard, mutations a write guard. Grounded in Polars' `RwLock<DataFrame>` + `Arc`-shared `SharedStorage`. The stateful specialization of `rust/AGENTS.md`'s generic three-layer convention; complements ADR-003 (which answers *who owns state*, not how Python reaches it). `UniswapEngine` still holds its own `Arc<Mutex<Bot>>` pending unification.
_Avoid_: polars model, polars-style middle layer, FFI wrapper topology

**PyBot** (formerly `PyBotCore`): The `PyO3` wrapper class (exposed as `PyBot` in Python) holding `Arc<parking_lot::RwLock<Bot>>`. The Python `Bot.__init__` constructs one (`self._py_bot = PyBot()`); the FFI topology this wrapper participates in is defined in {Polars-Inspired Three-Layer Architecture} (ADR-005). Provides `register_v2_pool`, `register_v3_pool`, `update_v2_pool`, `update_v3_pool`, `calculate_tokens_out`, `calculate_tokens_in`, `encode_swap`, `get_pool`, `register_token`, `get_token` — queries/calcs take a read guard, mutations a write guard. Thin `PyLiquidityPool`/`PyErc20Token` handles returned by `get_pool`/`get_token` are the Rust-core read entry points. `register_v3_pool` / `register_v4_pool` register the pool with `Bot` and apply staged backfill/pump buffers (`apply_backfill_buffer` → `apply_pump_buffer`); on-chain verification (`liquidity_verifier::verify_v3_pool` / `verify_v4_pool`) is invoked ad-hoc from `py_binding.rs` (the free-function surface, not a method on a `LiquidityMap` — the generic was never extracted; see {LiquidityMap}).
_Avoid_: pybot wrapper, python bot handle

**PyLiquidityPool**: A thin `PyO3` handle (exposed in Python as `PyLiquidityPool` — keeps the `Py` prefix per ADR-005's full-Polars naming rule; the earlier `#[pyclass(name = "Pool")]` override was dropped) that holds `Arc<parking_lot::RwLock<Bot>>` + a `pool_id` key, sharing the same `Arc` as the owning `PyBot` (see {Polars-Inspired Three-Layer Architecture}). Property reads and calculation calls cross `PyO3` on every access (read guard) — the handle owns no state itself. Generalized by design (no per-variant `PyV2PoolState`/`PyV3PoolState`/`PyV4PoolState` wrappers; the variant lives only as internal `PoolEntry` dispatch). ADR-005 slice 7 stance B (implemented): the generalized `LiquidityPool` Python companion wraps `PyLiquidityPool` (the hollow V2 DEX subclasses `SushiswapV2Pool`/etc. are deleted; DEX identity is `DexIdentity` data passed as `dex=`, not a class hierarchy — see `docs/migration-guides/dex-subclass-collapse.md`). `calculate_tokens_out`/`calculate_tokens_in` are the per-pool swap-math surface (V2 constant-product + V3/V4 concentrated-liquidity via `v3_simulate_swap`/`v4_simulate_swap`, mirroring Python's `calculate_tokens_out_from_tokens_in`). `PyErc20Token` is the token-side counterpart (see `PyErc20Token`).
_Avoid_: pool handle, pool wrapper

**PyErc20Token**: A thin `PyO3` handle (exposed in Python as `PyErc20Token` — keeps the `Py` prefix per ADR-005; the earlier `#[pyclass(name = "Token")]` override was dropped) that holds `Arc<parking_lot::RwLock<Bot>>` + an `Address` key, sharing the same `Arc` as the owning `PyBot` (see {Polars-Inspired Three-Layer Architecture}). Reads token metadata (address, decimals, symbol, name) from `Bot.tokens`. ADR-005 (implemented): `Erc20Token` is the Python companion over `PyErc20Token` (slice 3 — no bridge type remains).
_Avoid_: token handle, token wrapper

**EncodedCall**: A pre-encoded EVM call ready for on-chain submission: `to` (target `Address`), `data` (selector + ABI-encoded parameters as `Vec<u8>`), `value` (`U256`, always zero for V2 swaps). Produced by `encode_v2_swap()` — Python never calls `eth_abi.encode()`.
_Avoid_: encoded swap, call data, swap call

**V2_SWAP_SELECTOR**: `[0x02, 0x2c, 0x0d, 0x9f]` — the first 4 bytes of `keccak256("swap(uint256,uint256,address,bytes)")`. Prepended to ABI-encoded parameters in `encode_v2_swap()`.
_Avoid_: swap selector, v2 swap function hash

**V2Snapshot** *(removed)*: Formerly a frozen snapshot of V2 pool reserves at a given block, stored in `StateHistory`. Both `V2Snapshot` and `StateHistory` were deleted from the code when `ReorgJournal` (the delta mechanism) replaced the full-snapshot approach. Entry retained only to name what was removed.
_Avoid_: v2 state snapshot, reserve snapshot

**StateHistory** *(removed)*: Formerly a `VecDeque<V2Snapshot>` with bounded depth, supporting `push_state()` / `discard_before_block()` / `restore_before_block()`. Deleted from the code; superseded by **Reorg Journal** (delta-based, not snapshot-based). Entry retained only to name what was removed.
_Avoid_: state cache, history deque, v2 cache

**Removed-Flag**: The canonical reorg signal carried on every `eth_subscribe` log — a `bool` the node sets to `true` for events orphaned by a reorg, `false` for events on the canonical fork (fresh or re-emitted). The pump reads this on each WS log (ADR-003) rather than inferring reorgs from block-number comparison: no false positives from out-of-order delivery, and catches removed logs whose block number is still ≥ `last_solved_block`. Matches Alloy's `Log::removed` field.
_Avoid_: orphan flag, reorg bit, unsubscribe flag

**Reorg Journal**: The rollback mechanism on `Bot`'s `PoolEntry` variants — a bounded `VecDeque` of per-block **deltas** carrying "before" values, enabling `restore_before_block(block)` to pop deltas at/after the target block and restore prior state. `V2BlockDelta` carries scalar reserve priors (cheap — two `U256`s); `V3BlockDelta` carries scalar priors (`sqrt_price_x96`, `liquidity`, `tick`) plus per-tick priors for ticks modified by V3 Mint/Burn or V4 `ModifyLiquidity` (the same `V3BlockDelta` shape serves both V3 and V4 CL families). `restore_before_block` also handles the "first event at the reorg target block" case (single delta at target → restore to registration state, no panic); `ReorgJournal::newest_block` gates per-pool idempotent restore in `Bot::restore_all_pools_before_block` — which dispatches V2 (scalar reserves) vs V3/V4 (`v3_restore_before_block`/`v4_restore_before_block`: scalars + reverse-applied per-tick priors + tick-range cache invalidation). **Implementation status (Plan 100, complete):** V2/V3/V4 reorg are all LIVE — `apply_v2_sync`/`apply_v3_swap`/`apply_v4_swap` journal deltas, the pump reads the canonical `eth_subscribe` `log.removed` flag and calls `engine.handle_reorg(block)` (ADR-003 Option α), which restores all registered pools + invalidates `path_resolved` so the next `solve_dirty` re-derives and `send_result_batch` emits `expired` diffs against `delivered`. V4 reorg uses the same V3BlockDelta shape (no separate `V4BlockDelta`). Depth is user-configurable via `Bot::with_journal_depth`, default 1 mainnet epoch (32 blocks); reorgs deeper than the depth fail-stop the pump with a diagnostic rather than silently corrupting state. The previously-latent V3 partial-priors bug (`restore_before_block` returning only the last-popped delta's tick priors across multi-block rollbacks) is fixed — the restore now accumulates tick priors across every popped delta (oldest wins on duplicate tick idx).
_Avoid_: delta-journal, state history, rollback stack, undo log

**Slot0 Head / Tick Bookkeeping Map**: The conceptual two-part split *within* a single CL pool's state (`V3PoolState` / `V4PoolState`). The **slot0 head** = the high-churn scalar state (`sqrt_price_x96`, `tick`, active `liquidity`) mutated on *every Swap* (and `liquidity` again at tick crossings inside the swap loop). The **tick bookkeeping map** = `tick_data: HashMap<i32, TickInfo>` (initialized ticks' `liquidity_gross`/`liquidity_net`), mutated *only* by Mint/Burn (V3) or `ModifyLiquidity` (V4) — low churn, stable between liquidity events. The seam shows up operationally: `liquidity_verifier`'s module doc explicitly states it verifies the tick bookkeeping map and **never** the slot0 head ("would always be stale"), and `V3BlockDelta` carries the two as separate journal sub-structures (scalar priors + per-tick priors). **Structural — see ADR-004.** A TODO-6177b602 survey (148 V3 + 184 V4 field accesses → 22 function-level surfaces) found the deferral trigger condition met: six `takes-whole-but-wants-one` sites (`verify_v3_pool` / `verify_v4_pool` + `apply_v3_liquidity_update` / `apply_v4_liquidity_update` + their batch entry points) want only the tick map and recover the "don't read slot0" rule from comments today, while zero `slot0-only` consumers exist anywhere. ADR-004 adopts a typed `TickMap` trait that narrows those six sites — the type carries the rule, not a doc comment. The {Pool's authority over its own math} principle is preserved: `v3_simulate_swap` / `v4_simulate_swap` / `build_int_*_sequence` keep taking `&V3PoolState` / `&V4PoolState` flat (the `slot0` + `tick_data` lockstep reads are a genuine `both` consumer — the two halves are one deep module *as a simulator input*, but not as a verifier/apply input). The full-struct-split reading (`slot0: Slot0Head`, `ticks: TickBookkeepingMap`) was **rejected by survey evidence**: it earns nothing (no `slot0-only` reader exists) and costs HIGH migration at the lockstep sites. The hold reason recorded previously ("they are one *deep* module, not two shallow ones — a split would be pure layering unless and until a typed boundary pays for itself") was correct as a rejection of the full-split reading, and the typed-boundary reading pays for itself today per ADR-004.
_Avoid_: hot/cold state split, head vs. map, slot vs. ticks

**V3SwapTopic**: `0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67` — the Keccak256 hash of the Uniswap V3 `Swap(address,address,int256,int256,uint160,uint128,int24)` event signature. Used by the V3 swap decoder to filter and decode logs.
_Avoid_: v3 swap topic, swap hash, v3 swap signature

**V3BlockEngine**: A pure-Rust struct that owns the full per-block V3 arbitrage lifecycle: Swap event decoding, pool state updates (including tick-level changes), tick-range computation via `compute_tick_ranges`, and Mobius piecewise solver dispatch. V3 paths carry `V3TickRangeSequence` objects built from the engine's tick data. Registration is frozen after `start()`. Follows the `V2BlockEngine` pattern.
_Avoid_: V3 engine, v3 block processor, v3 arbitrage engine

**V3PoolState**: The engine-internal state for a Uniswap V3 pool: address, token0/token1, fee, tick_spacing, factory, mutable fields (sqrt_price_x96, liquidity, tick), and `tick_data: HashMap<i32, TickInfo>`. Computes tick-range sequences for both swap directions via `build_tick_range_sequences()`.
_Avoid_: v3 state, v3 pool data

**RegisterV3PoolParams**: A parameter struct for `V3BlockEngine::register_pool()` — bundles address, token pair, fee, tick_spacing, factory, sqrt_price_x96, liquidity, tick, and tick_data into one argument. `register_pool()` only creates the pool entry; buffer application is handled separately by `apply_backfill_buffer()` and `apply_pump_buffer()`.
_Avoid_: v3 params, pool registration params

**V3PoolRef**: A path hop reference: (`pool_idx`, `zero_for_one`). Used in path registration to identify which pool and direction each hop represents.
_Avoid_: pool ref, v3 hop ref

**V3SwapUpdate**: A pre-decoded V3 Swap update for testing without log decoding. Carries pool_address, sqrt_price_x96, liquidity, tick, and tick_priors.
_Avoid_: swap update, v3 swap event data

**PyV3ArbEngine**: The PyO3 wrapper class (exposed as `V3ArbEngine` in Python) that holds `Arc<Mutex<V3BlockEngine>>` plus a shutdown flag and pump handle. Mirrors `PyV2ArbEngine` — Python registers V3 pools with tick_data from the DB snapshot, then starts a Rust-side pump. Buffered Mint/Burn events are applied in two stages via `apply_backfill_buffer()` and `apply_pump_buffer()` (not inline during `register_pool()`). `process_logs()` drives the engine synchronously for testing.
_Avoid_: Python V3 engine, V3 engine wrapper

**V3EnginePump**: A standalone async task that drives `V3BlockEngine`. Owns its own `AlloyProvider` (created from an RPC URL string), subscribes to block headers via WS, fetches V3 Swap/Mint/Burn logs via `eth_getLogs` on each new block, and calls `engine.process_block()`. No dependency on the Python subscription infrastructure. Mirrors `V2EnginePump`. Superseded by `UniswapEnginePump` for mixed-protocol deployments.

**V4EnginePump**: A standalone async task that drives `V4BlockEngine`. Owns its own `AlloyProvider`, subscribes to block headers via WS, fetches V4 Swap/ModifyLiquidity logs via `eth_getLogs` on each new block, and calls `engine.process_block()`. Key difference from V2/V3: filters on PoolManager address (typically one) instead of individual pool contract addresses. Mirrors `V2EnginePump` and `V3EnginePump`. Superseded by `UniswapEnginePump` for mixed-protocol deployments.

**UniswapEnginePump**: The unified pump that drives `UniswapEngine`. Maintains two concurrent WS subscriptions — `newHeads` (block boundary notifications) and `logs` (unfiltered, all filtering in Rust). Each WS log is applied to engine state immediately via `apply_log` (zero-latency state update), but solves are deferred to the top of the next loop iteration — this naturally coalesces multiple logs that arrive between `await` points into a single `solve_dirty` call, avoiding redundant O(total_paths) carry-forward merges for hot pools with multiple events per block. Solves and sends are decoupled: `solve_dirty` updates `self.results` without sending; `send_result_batch` (→ `compute_diff_and_send`) is driven by a 50ms debounce timer (`DEBOUNCE_MS`) that starts/resets on each dirty log and force-flushes on block boundaries (`finalize_if_dirty`) and the 60s-timeout recovery path — one dispatch per log burst rather than one per log. Block boundaries are detected by `log.block_number > current_block` (primary) or `header.number > current_block` (empty-block fallback) — headers provide metadata (timestamp, fees) and trigger empty-block batches. A 60s timeout with no activity triggers `eth_getLogs` backfill for the missing range. Rust-side `filter_relevant_logs()` checks topic0 against 6 monitored topics AND address against registered pool/PoolManager addresses — no provider-side filter needed. Spawned by `PyUniswapArbEngine.start(rpc_url)`.
_Avoid_: unified pump, combined pump, single pump, per-block getLogs pump

**V3_MINT_TOPIC**: `0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde` — the Keccak256 hash of the Uniswap V3 `Mint(address,address,int24,int24,uint128,uint256,uint256)` event signature. Used by the V3 Mint/Burn decoder to filter and decode Mint logs. Matches the Python `V3_MINT_TOPIC` in `log_decoders.py`.

**V3_BURN_TOPIC**: `0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c` — the Keccak256 hash of the Uniswap V3 `Burn(address,int24,int24,uint128,uint256,uint256)` event signature. Used by the V3 Mint/Burn decoder to filter and decode Burn logs. Matches the Python `V3_BURN_TOPIC` in `log_decoders.py`.

**update_tick_liquidity**: A shared helper in `tick_bitmap.rs` that mutates a single tick's `liquidity_gross` and `liquidity_net` in-place, matching V3's Solidity `Tick.update()`. Both lower and upper tick receive `liquidity_gross += delta`; `liquidity_net += delta` for the lower tick and `liquidity_net -= delta` for the upper tick (controlled by the `is_lower_tick` parameter). Used by both `V3BlockEngine` and `V4BlockEngine`.
_Avoid_: tick update, tick liquidity mutation

**apply_liquidity_to_tick_range**: A shared helper in `tick_bitmap.rs` that composes the standard Mint/Burn/ModifyLiquidity pattern: calls `update_tick_liquidity` for both `tick_lower` and `tick_upper`, then removes ticks whose `liquidity_gross` has dropped to zero. Used by both `V3BlockEngine` and `V4BlockEngine` (including dual-orientation forward/reverse updates in V4).
_Avoid_: range update, tick range mutation

**V4_MODIFY_LIQUIDITY_TOPIC**: `0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec` — the Keccak256 hash of the Uniswap V4 `ModifyLiquidity(bytes32,address,int24,int24,int256,bytes32)` event signature. Used by the V4 ModifyLiquidity decoder to filter and decode ModifyLiquidity logs. Matches the Python `V4_MODIFY_LIQUIDITY_TOPIC` in `log_decoders.py`. Unlike V3 Mint/Burn, tickLower and tickUpper are in the data (not indexed), and liquidityDelta is signed (int256, positive for additions, negative for removals).
_Avoid_: v4 modify liquidity hash, modify liquidity event hash

**V4ModifyLiquidityEvent**: Decoded V4 ModifyLiquidity event carrying `pool_id`, `sender`, `tick_lower`, `tick_upper`, `liquidity_delta` (I256), `salt`. V4's single ModifyLiquidity event replaces V3's separate Mint and Burn events — a signed `liquidity_delta` handles both directions.
_Avoid_: v4 liquidity event, modify liquidity result

**apply_liquidity_update**: A method on both `V3BlockEngine` and `V4BlockEngine` that applies a Mint/Burn or ModifyLiquidity event to a pool's `tick_data`. Delegates to the shared `apply_liquidity_to_tick_range` helper in `tick_bitmap.rs`. For V3, `liquidity_delta` is positive for Mint, negative for Burn. For V4, `liquidity_delta` is already signed (I256). Returns the affected pool key: `Option<u64>` for V3, `Option<(u64, u64)>` (fwd+rev) for V4. If the pool is not registered, the event is buffered in `pump_event_buffer` (not `backfill_event_buffer` — backfill events use `buffer_backfill_liquidity_update`).
_Avoid_: liquidity mutation, mint/burn handler

**backfill_event_buffer**: A `HashMap<K, Vec<BufferedLiquidityUpdate>>` on `V3BlockEngine` (keyed by `Address`) and `V4BlockEngine` (keyed by `(Address, PoolId)`) that stores liquidity events from the backfill phase (`snapshot_block+1` to `first_ws_block-1`). Never expired — covers a fixed block range and drains pool-by-pool during `build_paths` via `apply_backfill_buffer()`. Populated by `buffer_backfill_liquidity_update()`, which is called by `process_backfill_logs()`.
_Avoid_: backfill buffer, snapshot buffer, cold buffer

**pump_event_buffer**: A `HashMap<K, Vec<BufferedLiquidityUpdate>>` on `V3BlockEngine` and `V4BlockEngine` that stores liquidity events from the WS pump phase (`first_ws_block` onward) for unregistered pools. Expired normally via `expire_buffered_events()`. Populated by `apply_liquidity_update()` when the pool is not yet registered. Drained pool-by-pool during `build_paths` via `apply_pump_buffer()`.
_Avoid_: pump buffer, live buffer, hot buffer

**buffer_backfill_liquidity_update**: A method on `V3BlockEngine` and `V4BlockEngine` that routes a backfill-phase liquidity event to `backfill_event_buffer` (not `pump_event_buffer`). During backfill, no pools are registered yet, so all events are buffered. If a pool is already registered (unusual), the event is applied directly. Called by `process_backfill_logs()` on the outer engine.
_Avoid_: backfill liquidity handler, cold liquidity route

**apply_backfill_buffer**: A method on `V3BlockEngine` and `V4BlockEngine` that drains and applies all backfill-buffered events for one pool after `register_pool()`. Called under the engine lock, before `apply_pump_buffer()`. The pool state after this call is at the backfill boundary — a deterministic point suitable for verification cloning.
_Avoid_: apply backfill, drain backfill, backfill apply

**apply_pump_buffer**: A method on `V3BlockEngine` and `V4BlockEngine` that drains and applies all pump-buffered events for one pool after `apply_backfill_buffer()`. Called under the engine lock. The pool state after this call reflects all pump-processed events and is ready for solving.
_Avoid_: apply pump, drain pump, pump apply

**verify_on_register**: A flag on `PyUniswapArbEngine` that enables two-phase verification during pool registration. When enabled and the pool has `Tracked` coverage, the engine captures deterministic state snapshots under the lock and verifies them against on-chain: (1) raw tick_data vs on-chain at the snapshot block, (2) post-backfill state vs on-chain at the backfill block. Verification RPC calls happen outside the lock. On mismatch, `RuntimeError` is raised causing immediate bot shutdown.
_Avoid_: registration verification, startup check, tick verification

**UniswapEngine**: A unified engine that composes a `V2BlockEngine`, `V3BlockEngine`, and `V4BlockEngine` to handle mixed V2/V3/V4 arbitrage paths in the same per-block lifecycle. A **peer module** to `Bot` (ADR-003): owns path registry, solver dispatch, result batching, the pump, and diagnostics, but owns **no pool state** — it reads/mutates state through `Arc<Mutex<Bot>>`. Routes Sync events to the V2 engine, Swap events to the V3 engine, and V4 Swap events (from PoolManager) to the V4 engine. Solver dispatch: V2-V2 → `exact_mobius_solve`, V3-V3 and V4-V4 → `int_solve_v3_v3` (same CL math), V2-V3/V3-V2/V4-V3/V3-V4/V4-V2/V2-V4 → `exact_solve_mixed_v2_v3_sequence` (integer effective reserves + piecewise tick-range enumeration). Registration is frozen after `start()` — calls `start()` on all sub-engines.
_Avoid_: mixed engine, combined engine, uniswap engine

**apply_log**: A method on `UniswapEngine` that decodes a single WS log and applies it to the appropriate sub-engine immediately. Returns the set of affected path IDs via the `pool_to_paths` reverse index. Does NOT trigger a solve — the caller (the pump) defers solving to the top of the next loop iteration for coalescing. Sub-engine `apply_*` methods return affected pool keys: `V2BlockEngine::apply_sync` → `Option<u64>`, `V3BlockEngine::apply_swap` → `Option<u64>`, `V3BlockEngine::apply_liquidity_update` → `Option<u64>`, `V4BlockEngine::apply_swap` → `Option<(u64, u64)>`, `V4BlockEngine::apply_liquidity_update` → `Option<(u64, u64)>`.
_Avoid_: per-log update, single-log apply

**solve_dirty**: A method on `UniswapEngine` that takes accumulated dirty pool keys (from `apply_log` calls since the last solve), expires buffered events, and rebuilds+solves affected paths (via `rebuild_and_solve_affected`), updating `self.results` in place. It does **NOT** send a `ResultBatch` — sending is decoupled into `send_result_batch` (→ `compute_diff_and_send`), driven by the pump's 50ms debounce timer or block-boundary flush. Called by the pump at the top of each loop iteration (coalescing multiple logs that arrived between `await` points). Recorded at `solver_dispatch.rs:90`: *"no compute_diff_and_send here — the pump controls when batches are dispatched."*
_Avoid_: batch solve, deferred solve, flush dirty

**ResultBatch**: An incremental diff between two solver runs. Carries `solve_block`, block metadata (timestamp, fees, gas), and four lists: `fresh` (paths newly within the profit window), `updated` (paths within the window with changed values), `expired` (paths that left the window), `removed` (de-registered paths). Sent to Python via an unbounded `mpsc` channel — every diff is delivered, no silent drops. The `self.delivered` HashMap in `UniswapEngine` tracks what Python has seen, enabling correct incremental diff computation on each call. _Profit window_: `compute_diff_and_send` filters with `profit > min_profit && profit < max_profit` (strict on both bounds, open interval). `min_profit` is the lower actionability threshold (default `U256::ZERO`); `max_profit` is an upper sanity cap that excludes likely-defect / scam-token profits (default `U256::MAX`, i.e. disabled). Set via `set_profit_thresholds`; results outside the window are excluded from `delivered` and from batches.

_`delivered` invariant_: `delivered` is the set of paths Python has **actually received via the result channel**. It is advanced **only** by `compute_diff_and_send`, and only after building a `ResultBatch` for the current above-threshold subset of `results` (the channel send itself is `if let Some(ref tx)`-guarded, but the advance runs unconditionally — correct when a channel exists and the send fires; a bug when it does not, since `delivered` would then claim "Python has seen these" for a batch that was never sent, silently omitting those paths from the next real batch's `fresh` list). It must stay **empty before the first pump-driven send** (cold-start / `solve_all_paths`, which is solve-only and does not dispatch). `deregister_path` removes entries as paths are de-registered. Pinned by `solve_all_paths_does_not_advance_delivered_without_channel` (cold-start) and `send_result_batch_advances_delivered_to_above_threshold` (live).

_ResultBatch metadata on recovery paths_: `solve_block` is always the real block number (passed through even on backfill); only the block *metadata* (timestamp/fees/gas) can be zero (`BlockMetadata::default()`). The hot-loop finalize path (`UniswapEnginePump::finalize_if_dirty` → `UniswapEngine::finalize_block`) threads the caller's real `current_metadata`, so its batches carry genuine fees/gas/timestamp. The only remaining default-metadata site is `backfill_range`'s `process_block`, which **solves but does NOT send** — accumulated results piggyback onto the next debounce `send_result_batch` with real metadata (and the newer block's `results_block`). (`solve_all_paths` was the second default-metadata site but is now solve-only — no dispatch — so it no longer sends at all; it's a test/cold-start synchronization entry point, not a hot-loop path.) The Python consumer computes `base_fee_next = next_base_fee(0,0,0) = 0` (safe — no div-by-zero, the `0==0` gas-target branch); a hypothetical profitable batch dispatched with zero metadata would yield `maxFeePerGas = priority_fee` ≪ the real base fee → underpriced tx dropped from mempool (no crash).
_Avoid_: solver result, solve output, batch result

**Eager Processing**: The pump architecture where WS logs are applied to engine state immediately upon arrival (via `apply_log`), with solver runs deferred and coalesced at the top of the pump loop. This eliminates the 1-block (~12s on mainnet) latency of the previous design where logs were buffered until a block header arrived. **Solve and send are decoupled**: solves happen at the top of each loop iteration when `has_dirty_paths()` is true; sends (`send_result_batch` → `compute_diff_and_send`) are debounced — one dispatch per burst of logs, via a 50ms debounce timer (`DEBOUNCE_MS = 50`) that starts/resets on each dirty log and force-flushes on block boundaries (`finalize_if_dirty`) and on the 60s-timeout recovery path. The pump loop: (1) top of iteration — check `has_dirty_paths()`, call `solve_dirty` if needed; (1a) if the 50ms debounce timer has expired, call `send_result_batch`; (2) await next WS event; (3) on `Log` → `apply_log` only (no solve), start/reset the debounce timer; (4) on `BlockHeader` → force-flush via `finalize_if_dirty`, record metadata, advance block tracking, handle empty blocks and gap backfill. Multiple logs arriving between `await` points are naturally batched into a single `solve_dirty` call, and a burst of such logs yields a single `send_result_batch`.
_Avoid_: real-time processing, immediate processing, streaming

**HopType**: An enum (`V2`, `V3`, `V4`) identifying which sub-engine owns a given hop in a mixed path. `V4` is treated as concentrated liquidity (same math as `V3`) by the solver.
_Avoid_: hop enum, engine type

**MixedPoolRef**: A pool reference in a mixed path: (`hop_type`, `pool_key`, `zero_for_one`). `pool_key` is the pool ID in the V2 engine or the pool key in the V3 engine.
_Avoid_: mixed ref, mixed hop ref

**PyUniswapArbEngine**: The PyO3 wrapper class (exposed as `UniswapArbEngine` in Python) that holds `Arc<Mutex<UniswapEngine>>` plus a shutdown flag and pump handle. Provides `register_v2_pool`, `register_v3_pool`, `register_v4_pool`, `register_path`, `process_logs`, `latest_results`, and `backfill_from_snapshot`. The three-phase startup is `subscribe()` → `backfill_from_snapshot()` → `resume()`.
_Avoid_: Python mixed engine, uniswap engine wrapper

**backfill_from_snapshot()**: A PyO3 method on `PyUniswapArbEngine` that bridges the gap between the DB snapshot block and the first WS block. Called after `subscribe()`, before `resume()`. Creates an HTTP provider, fetches Swap/Mint/Burn/ModifyLiquidity events from `snapshot_block + 1` to `first_ws_block - 1` in paginated `eth_getLogs` calls, and applies them via `process_backfill_logs()`. Liquidity events for unregistered pools are routed to `backfill_event_buffer` (never expired); Swap events are applied directly to registered pools. Captures `verify_snapshot_block` and `verify_backfill_block` internally — no Python-side configuration needed.
_Avoid_: snapshot backfill, cold-start backfill

**process_backfill_logs()**: A method on `UniswapEngine` that splits raw Alloy logs by topic (V3 Swap/Mint/Burn vs V4 Swap/ModifyLiquidity). Liquidity events (V3 Mint/Burn, V4 ModifyLiquidity) are routed to `buffer_backfill_liquidity_update()` which stores them in the `backfill_event_buffer` (never expired). Swap events are applied directly to already-registered pools. Called by `backfill_from_snapshot()` for each paginated chunk.
_Avoid_: backfill log processing

**Code Injection**: Injecting contract runtime bytecode at a fresh address via `eth_simulateV1`'s `stateOverrides.code` field, enabling simulation of undeployed contracts. The executor contract's runtime bytecode (with 9 immutable slots baked in after the CBOR metadata) is loaded from `contracts/cmd_executor_runtime_bytecode.txt`. Vyper's deployed bytecode layout is `[code_section][CBOR_metadata][immutable_data]`; the CBOR bytes serve dual purpose as compiler metadata AND runtime data (function dispatch jump table at offset `0x404a`, JUMPDEST target at `0x4046`). The CBOR must NOT be stripped or CODECOPY offsets will misalign. `eth_simulateV1` chains calls sequentially, so the 3-call WETH balanceOf pattern correctly captures profit without WETH storage prefunding.
_Avoid_: contract injection, bytecode override, code override

**V4BlockEngine**: A pure-Rust struct that owns the full per-block V4 arbitrage lifecycle: V4 Swap event decoding from PoolManager, pool state updates (including tick-level changes), tick-range computation, and Mobius piecewise solver dispatch. V4 pools are identified by `(pool_manager: Address, pool_id: [u8; 32])` instead of a single contract address. V4 paths carry `IntV3TickRangeSequence` objects (same type as V3) — the solver can't distinguish V3 from V4 hops. Follows the `V3BlockEngine` pattern exactly.
_Avoid_: V4 engine, v4 block processor, v4 arbitrage engine

**V4PoolState**: The engine-internal state for a Uniswap V4 pool: pool_manager address, pool_id (32-byte hash of PoolKey), pool_key (currency0, currency1, fee, tick_spacing, hooks), mutable fields (sqrt_price_x96, liquidity, tick), and `tick_data: HashMap<i32, TickInfo>`. Mirrors `V3PoolState` with added `(pool_manager, pool_id)` identification.
_Avoid_: v4 state, v4 pool data

**V4PoolKey**: A struct with 5 fields matching Solidity's `PoolKey`: `currency0: Address`, `currency1: Address`, `fee: u32`, `tick_spacing: i32`, `hooks: Address`. Used for engine-internal tracking; the hooks field is stored but not used for solving (hook filtering happened at registration).
_Avoid_: pool key, v4 key, poolkey

**V4SwapUpdate**: A pre-decoded V4 Swap update. Carries pool_manager, pool_id, sqrt_price_x96, liquidity, tick, and tick_priors. Same structure as `V3SwapUpdate` with replaced identification fields.
_Avoid_: v4 swap event data, v4 update

**LiquidityMap** *(not extracted)*: Conceptual first-class owner of accurate CL pool state for one pool family, bundling three operations — applying events (Swap, Mint/Burn, ModifyLiquidity); buffering deferred liquidity events for unregistered pools; and verifying the resulting state against on-chain truth. A cross-cutting concern, not a solver-engine concern (ADR-003). **Never implemented as a generic** (ADR-003's no-abstraction-against-sample-of-one discipline; the {Bot} term documents this): the live shape is the inline `PoolEntry::V3(V3PoolState)` / `PoolEntry::V4(V4PoolState)` collection in `Bot.pools`, with apply methods on `Bot` (`apply_v3_swap` / `apply_v3_liquidity_update` + V4 mirrors) and verify functions in `liquidity_verifier.rs` (`verify_v3_pool` / `verify_v4_pool` / `verify_v3_liquidity_map` / `verify_v4_liquidity_map`). ADR-004 adds a `TickMap` typed boundary narrowing the verifier/apply views of these entries. Trait extraction deferred until a third CL-style family (Curve) ports and proves the shape.
_Avoid_: liquidity store, tick map, state map (the last collides with `PyPoolCache`'s solver-cache sense)

**Buffered Liquidity Update**: A Mint/Burn (V3) or ModifyLiquidity (V4) event received while its pool is not yet registered. Held in the `LiquidityMap`'s `LiquidityEventBuffer` until registration triggers staged drain (`apply_backfill` → `apply_pump`), guaranteeing the registered pool's initial state reflects every event between the DB snapshot block and the current block.
_Avoid_: pending event, queued event, delayed update

**Verify-Against-Onchain**: The on-chain self-check comparing in-memory tick data (raw snapshot at the snapshot block, post-backfill state at the backfill block) against on-chain truth via RPC. Lives as the free-function surface in `liquidity_verifier.rs`: `verify_v3_liquidity_map` / `verify_v4_liquidity_map` (the snapshot-block variants, which already take a typed `&HashMap<i32, TickInfo>` + `Address` + block — the typed-boundary precedent ADR-004 generalizes) and `verify_v3_pool` / `verify_v4_pool` (the live variants, which take `&V3PoolState` / `&V4PoolState` and recover the "don't read slot0" rule from the module doc today — ADR-004's `TickMap` trait narrows these to take `&impl TickMap`). Invoked ad-hoc from `py_binding.rs`; not a method on a `LiquidityMap` (that generic was never extracted — see {LiquidityMap}).
_Avoid_: state check, registration check, verify hook

**AMOUNT_MODIFYING_HOOK_MASK**: `0xCC` — a bitmask covering the 4 V4 hook flags that can modify swap amounts: `BEFORE_SWAP` (0x80), `AFTER_SWAP` (0x40), `BEFORE_SWAP_RETURNS_DELTA` (0x08), `AFTER_SWAP_RETURNS_DELTA` (0x04). Pools with `(hook_flags & 0xCC) != 0` are rejected in Python before `register_v4_pool` — the Rust engine is permissive and performs no filtering.
_Avoid_: hook mask, hook filter, amount mask

**V4_DYNAMIC_FEE_FLAG**: `0x100000` — the fee value indicating a V4 pool has dynamic (swap-dependent) fees. Pools with this fee value are rejected in Python before `register_v4_pool`. The Rust engine is permissive.
_Avoid_: dynamic fee, variable fee flag

**V3 amountSpecified Sign Convention**: V3 and V4 use opposite conventions for `amountSpecified`. In V3: positive (> 0) = exact INPUT (swap exactly this much into the pool), negative (< 0) = exact OUTPUT (receive exactly this much from the pool). In V4, the convention is reversed. This is verified in `v3_simulator.py:93` — `exact_input = amount_specified > 0`. For arbitrage (always exact-input mode), V3 swap calldata must use **positive** values. The original bot implementation incorrectly used the V4 convention (negative values), causing all V3-involving simulations to fail with V3's "IIA" (Insufficient Input Amount) error.
_Avoid_: amount sign, swap direction sign, amount convention

## Relationships

- **String Interner** → **Type Cache Key**: The interner produces `Arc<str>` values that are collected into `Arc<[Arc<str>]>` keys for `LruCache` lookups
- **Type Cache Key** → **`Arc<CachedAbiTypes>`**: On cache hit, `Arc::clone` returns an O(1) reference instead of deep-cloning the `DynSolType` tree
- **`Arc<CachedAbiTypes>`** → **`FunctionSignature`**: `FunctionSignature` stores `Option<Arc<CachedAbiTypes>>` so cloning a signature is O(1) rather than O(tree depth)
- **`parking_lot::Mutex`** → **`PyPoolCache`**: Avoids poisoning (`lock()` returns `MutexGuard` directly, not `LockResult`), sidestepping `clippy::expect_used`/`clippy::unwrap_used` lints
- **`IntHopState`** → **`PyPoolCache`**: Pre-converted U512 values stored in the cache, eliminating per-swap U256→U512 conversions
- **`drain_raw()`** → **`drain_buffer()`**: Pure-Rust buffer mechanics extracted from the Python-touching method for testability without `with_embedded_python_interpreter` (which can only be called once per process)
- **`py.detach()`** → **async provider**: GIL released before `block_on()` calls; this is the no-circular-wait guarantee that makes `Python::attach()` safe in the async path
- **`auto-initialize` feature** → **concurrency tests**: Default Cargo feature that auto-initializes the Python interpreter; required for `concurrency_stress.rs` and integration tests
- **`V2BlockEngine`** → **`IntHopState`**: V2BlockEngine stores forward and reverse `IntHopState` entries per pool; dual-orientation enables path construction in either direction
- **`V2BlockEngine`** → **`mobius_solve_with_refinement`**: The engine calls the solver directly on pre-resolved paths — no PyO3 crossing
- **`V2EnginePump`** → **`V2BlockEngine`**: The pump holds `Arc<Mutex<V2BlockEngine>>` and calls `process_block()` on each new block
- **`V2EnginePump`** → **`AlloyProvider`**: The pump creates its own `AlloyProvider` from an RPC URL — no PyO3 dependency
- **`V2_SYNC_TOPIC`** → **`V2BlockEngine`**: The engine's Sync decoder uses `V2_SYNC_TOPIC` to filter and decode logs
- **`V2EnginePump`** → **`PyV2ArbEngine`**: PyV2ArbEngine holds `Arc<Mutex<V2BlockEngine>>` (shared with the pump) and a shutdown `Arc<AtomicBool>`; pump is spawned on `start(rpc_url)` and stopped via the shutdown flag
- **`PyV2ArbEngine`** → **`V2BlockEngine`**: The PyO3 wrapper delegates all operations to the inner engine through the shared `Mutex`; `latest_results()` is the only per-block call when the pump is running
- **`Bot`** → **`PoolEntry`**: `Bot.pools` is a `HashMap<u64, PoolEntry>` — the single source of truth for all pool state, replacing the 4-copy model (Python `StateCache`, Python adapter, Rust `PyPoolCache`, Rust `V2BlockEngine` HashMap)
- **`Bot`** → **`IntHopState::swap()`**: `calculate_tokens_out` for V2 delegates to `IntHopState::swap()` (U512-based EVM-exact arithmetic), guaranteeing integer-level agreement with the on-chain contract
- **`Bot`** → **`V2PoolState`**: V2 pools store per-direction fee parameters (`fee_token0`, `fee_token1`), reserves, and update block — eliminating the need for Python's `V2PoolState` cache object
- **`Bot`** → **`RegisterV2PoolParams`**: The params struct bundles 9 registration fields into one argument; `py_bot.rs` constructs it from Python keyword arguments
- **`PyBot`** → **`Bot`**: The PyO3 wrapper holds `Arc<parking_lot::RwLock<Bot>>`; every Python call acquires the lock (read for queries/calcs, write for mutations), performs the operation, and releases
- **`PyLiquidityPool`** → **`Bot`**: The thin handle holds `Arc<parking_lot::RwLock<Bot>>` + `pool_id` (shared with the owning `PyBot`); `calculate_tokens_out/in` delegate directly to `Bot` via the key (read guard)
- **`PyErc20Token`** → **`Bot`**: The thin handle holds `Arc<parking_lot::RwLock<Bot>>` + `Address` (shared with the owning `PyBot`); future property reads (symbol, decimals) will delegate through the same pattern (read guard)
- **`encode_v2_swap()`** → **`EncodedCall`**: Produces the pre-encoded calldata by ABI-encoding `(amount0_out, amount1_out, recipient, b"")` with the V2 swap selector; `Bot::encode_swap()` dispatches to this per pool type
- **`V2_SWAP_SELECTOR`** → **`encode_v2_swap()`**: The 4-byte selector prepended to ABI-encoded parameters; matches Python's `Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]`
- **`V2PoolState`** → **Reorg Journal**: Each V2 `PoolEntry` carries a `ReorgJournal<V2BlockDelta>`; `update_v2_pool` pushes a delta (scalar reserve priors), enabling `restore_before_block` rollback
- **`V3PoolState`** → **Reorg Journal**: Each V3 `PoolEntry` carries a `ReorgJournal<V3BlockDelta>`; `update_v3_pool` pushes a delta (scalar sqrt/liq/tick priors + per-tick priors for Mint/Burn). Swap deltas carry empty tick priors (Swaps mutate only scalars)
- **Reorg Journal** → **`Bot`**: `Bot` exposes `v2_journal_len`/`v2_discard_before_block`/`v2_restore_before_block` (and V3 mirrors) — `PyBot` delegates to these for Python access. Under ADR-003 the live `apply_*` methods (on `Bot` directly — the `LiquidityMap` generic was never extracted; see {LiquidityMap}) push deltas too, making restore callable from the hot path for the first time
- **`v2_restore_before_block()`** → **`V2PoolState`**: Restoring a snapshot also syncs the pool's current `reserve0`/`reserve1`/`update_block` fields, so subsequent calculations use the restored state
- **`V3BlockEngine`** → **`V3TickRangeSequence`**: The engine builds tick-range sequences from `compute_tick_ranges()` output; sequences are stored per-path per-direction and passed to `solve_v3_v3()`
- **`V3BlockEngine`** → **`solve_v3_v3()`**: The engine dispatches V3-V3 2-hop paths to the piecewise Mobius solver; results are (optimal_input, profit) as U256
- **`V3PoolState`** → **`compute_tick_ranges()`**: The pool state's `build_sequence()` calls the tick bitmap walk and constructs `V3TickRangeHop` entries with liquidity, sqrt prices, and fee
- **`V3BlockEngine`** → **`decode_v3_swap_log()`**: Process_block decodes V3 Swap events from raw logs, then applies updates via `apply_swap()`
- **`V3BlockEngine`** → **`RegisterV3PoolParams`**: Registration uses the params struct to bundle 10 fields; `V3PoolState::from()` converts params to state
- **`V3BlockEngine`** → **`TickInfo`**: The engine stores per-tick `TickInfo` (liquidity_gross, liquidity_net) in the pool's `tick_data` HashMap; Swap updates carry tick_priors for journaling
- **`PyV3ArbEngine`** → **`V3BlockEngine`**: The PyO3 wrapper delegates all operations to the inner engine through a shared `Mutex`; `process_logs()` is the main test entry point, `latest_results()` is the per-block result reader
- **`V3EnginePump`** → **`PyV3ArbEngine`**: PyV3ArbEngine holds `Arc<Mutex<V3BlockEngine>>` (shared with the pump) and a shutdown `Arc<AtomicBool>`; pump is spawned on `start(rpc_url)` and stopped via the shutdown flag
- **`V3EnginePump`** → **`AlloyProvider`**: The pump creates its own `AlloyProvider` from an RPC URL — no PyO3 dependency
- **`V3EnginePump`** → **`V3_SWAP_TOPIC`**: The pump builds an Alloy `Filter` with `V3_SWAP_TOPIC` and registered pool addresses for `eth_getLogs` queries (used by the standalone V3 pump; `UniswapEnginePump` does its own filtering)
- **`V4EnginePump`** → **`V4_SWAP_TOPIC`** + **`V4_MODIFY_LIQUIDITY_TOPIC`**: The pump builds an Alloy `Filter` with both topics and PoolManager addresses (used by the standalone V4 pump; `UniswapEnginePump` does its own filtering)
- **`UniswapEnginePump`** → **`UniswapEngine`**: The pump holds `Arc<Mutex<UniswapEngine>>` and calls `apply_log()` on each WS log (immediate state update) and `solve_dirty()` at the top of each loop iteration (coalesced solve)
- **`UniswapEnginePump`** → **`AlloyProvider`**: The pump creates its own provider from an RPC URL; subscribes to both `newHeads` and `logs` (unfiltered) via WS
- **`UniswapEnginePump`** — backfill on timeout/empty-block: 60s with no WS activity, or block header with zero buffered logs → `eth_getLogs` to verify
- **`PyUniswapArbEngine`** → **`UniswapEnginePump`**: `start(rpc_url)` spawns the pump; `pump_handle` stores the JoinHandle; `shutdown` `AtomicBool` stops it
- **`V3BlockEngine`** → **`decode_v3_mint_log()`**: Process_block decodes V3 Mint events from raw logs, then applies updates via `apply_liquidity_update()`
- **`V3BlockEngine`** → **`decode_v3_burn_log()`**: Process_block decodes V3 Burn events from raw logs, then applies updates via `apply_liquidity_update()` (negated delta)
- **`update_tick_liquidity`** → **`TickInfo`**: Mutates `liquidity_gross` and `liquidity_net` in-place on the `TickInfo` struct (shared helper in `tick_bitmap.rs`)
- **`apply_liquidity_to_tick_range`** → **`update_tick_liquidity`**: Calls `update_tick_liquidity` for both lower and upper ticks, then `retain` zero-gross ticks (shared helper in `tick_bitmap.rs`)
- **`apply_liquidity_update`** → **`apply_liquidity_to_tick_range`**: Delegates tick mutation to the shared helper in `tick_bitmap.rs`
- **`UniswapEngine`** → **`V2BlockEngine`**: UniswapEngine composes a V2BlockEngine for V2 pool state and constant-product solving; `start()` calls `v2_engine.start()`
- **`apply_log`** → **sub-engine `apply_*` methods**: `apply_log` decodes a log by topic, routes to `V2BlockEngine::apply_sync`, `V3BlockEngine::apply_swap`, `V3BlockEngine::apply_liquidity_update`, `V4BlockEngine::apply_swap`, or `V4BlockEngine::apply_liquidity_update`, collecting their returned pool keys into dirty sets
- **`apply_log`** → **`pool_to_paths`**: After applying a log, `apply_log` looks up each dirty pool key in the `pool_to_paths` reverse index to find affected path IDs, which it returns to the caller
- **`solve_dirty`** → **`rebuild_and_solve_affected`**: `solve_dirty` takes the accumulated dirty pool keys, calls `rebuild_and_solve_affected` to re-resolve and re-solve affected paths, and updates `self.results` in place. It does NOT send — sending is a separate step (`send_result_batch`)
- **`send_result_batch`** → **`ResultBatch`**: `send_result_batch` (→ `compute_diff_and_send`) constructs an incremental `ResultBatch` diff from `self.results` vs `self.delivered` and sends it to Python via the unbounded `mpsc` channel. Driven by the pump's 50ms debounce timer or block-boundary flush; `solve_dirty` does not send
- **`ResultBatch`** → **`self.delivered` HashMap**: `compute_diff_and_send` compares current above-threshold results against `self.delivered` (what Python has already seen) to produce the incremental diff — `fresh`, `updated`, `expired` lists — then updates `self.delivered` to the new set
- **`UniswapEnginePump`** → **`apply_log`**: The pump calls `apply_log` on each WS log for immediate state updates, deferring the solve
- **`UniswapEnginePump`** → **`send_result_batch`**: The pump calls `send_result_batch` on 50ms debounce expiry, on block boundaries (via `finalize_if_dirty`), and (with empty metadata) on the 60s-timeout recovery path
- **`UniswapEnginePump`** → **`solve_dirty`**: The pump calls `solve_dirty` at the top of each loop iteration (coalescing multiple logs). `solve_dirty` solves only; sends are driven separately via `send_result_batch`
- **`UniswapEngine`** → **`V3BlockEngine`**: UniswapEngine composes a V3BlockEngine for V3 pool state, tick ranges, and piecewise solving; `start()` calls `v3_engine.start()`
- **`UniswapEngine`** → **`V4BlockEngine`**: UniswapEngine composes a V4BlockEngine for V4 pool state, tick ranges, and piecewise solving; `start()` calls `v4_engine.start()`
- **`UniswapEngine`** → **`solve_v3_v3()`**: Pure V3/V4 paths are dispatched to the integer-exact piecewise Mobius solver (`int_solve_v3_v3`); mixed paths use integer sequence solver
- **`UniswapEngine`** → **`exact_solve_mixed_v2_v3_sequence()`**: Mixed V2-V3 paths use integer effective reserves + piecewise tick-range enumeration with closed-form Möbius solve per range, replacing the former golden-section search
- **`PyUniswapArbEngine`** → **`UniswapEngine`**: The PyO3 wrapper delegates all operations to the inner engine through a shared `Mutex`; `process_logs()` drives both V2 and V3 updates synchronously; `backfill_from_snapshot()` applies pre-resume events via `process_backfill_logs()`
- **`backfill_from_snapshot()`** → **`process_backfill_logs()`** → **`buffer_backfill_liquidity_update()`**: The cold-start backfill pipeline; splits V3/V4 logs by topic, routes Swap events directly, and liquidity events to `backfill_event_buffer` via `buffer_backfill_liquidity_update()`
- **`backfill_from_snapshot()`** → **`verify_snapshot_block` / `verify_backfill_block`**: After backfill completes, the engine captures both verification blocks internally — `verify_snapshot_block` is the `snapshot_block` parameter, `verify_backfill_block` is `first_ws_block - 1` (the last backfill block)
- **`verify_on_register`** → **`verify_v3_liquidity_map()` / `verify_v4_liquidity_map()`**: Snapshot-block check — raw tick_data cloned before `apply_backfill_buffer()` is verified against on-chain at the snapshot block
- **`verify_on_register`** → **`verify_v3_pools()` / `verify_v4_pools()`**: Backfill-block check — pool state cloned after `apply_backfill_buffer()` (before `apply_pump_buffer()`) is verified against on-chain at the backfill block
- **`register_pool()`** → **split buffer (backfill + pump)**: V3 and V4 `register_pool()` no longer applies buffers inline. Instead, the caller (`uniswap_engine.rs`) sequences three stages under the engine lock: (1) `register_pool()` — pool created with DB tick_data at snapshot block state, (2) `apply_backfill_buffer()` — pool at backfill boundary (clone for verification), (3) `apply_pump_buffer()` — pool at pump's current block. Backfill events are routed to `backfill_event_buffer` (never expired); pump events to `pump_event_buffer` (expired normally). This split enables race-free two-phase verification: raw tick_data vs on-chain at snapshot block, and post-backfill state vs on-chain at backfill block.
- **`Bot`** owns one `HashMap<u64, PoolEntry>` with `PoolEntry::V2(V2PoolState)` / `V3(V3PoolState)` / `V4(V4PoolState)` variants — a single inline store (the `LiquidityMap` generic was never extracted; see {LiquidityMap}). ADR-004 adds a `TickMap` trait that narrows the verifier/apply views of the V3/V4 entries.
- **`LiquidityEventBuffer`** is generic storage for buffered liquidity events awaiting registration; `Bot` owns two (`v3_buffer` keyed by `Address`, `v4_buffer` keyed by `(Address, PoolId)`) and the application policy (when to drain via `apply_pump_buffer` / `apply_backfill_buffer`, how to apply via `update_tick_liquidity` + `cached_tick_ranges` cache invalidation).
- **`PyBot`** exposes `register_v3_pool` / `register_v4_pool` as `Bot` registration with staged buffer drain (`apply_backfill_buffer` → `apply_pump_buffer`). On-chain verification (`verify_v3_pool` / `verify_v4_pool`) is invoked ad-hoc from `py_binding.rs` as the free-function `liquidity_verifier::*` surface — not a method on a `LiquidityMap`.
- **`UniswapEngine`** → **`Bot`** (LiquidityMap): the engine is a *consumer* of `LiquidityMap`, not its owner — `apply_log` calls `bot_core.v3_map.apply_swap(...)` / `apply_liquidity_update(...)` under the engine-then-core lock order (ADR-003); `solve_dirty` re-derives via `v3_map.get_sequence(key, zfo)`. Dispatch stays on the engine (path registry, solver selection, batch diffing); accurate pool state stays on Bot.
- **`MixedPoolRef`** → **`HopType`**: Each pool ref in a mixed path carries a `HopType` (`V2` or `V3`) for solver dispatch
- **`Code Injection`** → **`PyUniswapArbEngine`**: The bot's code injection feature uses `stateOverrides.code` to test the undeployed executor contract; the engine's `latest_results()` provides profitable paths that are encoded and simulated against the injected code

## Resolved Ambiguities

### GIL release vs GIL hold

**Ruling: Hold the GIL for sub-μs compute; release for I/O.** The threshold is empirical: GIL release/reacquire costs ~200ns. Any function completing in under 200ns (tick math ~20ns, address utils ~50ns) must hold the GIL. I/O-bound operations (async provider `block_on()`) must release it. The decision is documented per call site with `// SAFETY:` comments.

### parking_lot::Mutex vs std::sync::Mutex vs RefCell

**Ruling: `parking_lot::Mutex` for all Rust extension interior mutability.** `RefCell` is unsafe under free-threaded Python 3.14+ (no GIL guarantee). `std::sync::Mutex` poisons on panic, requiring `.expect()`/`.unwrap()` that violate strict clippy. `parking_lot::Mutex` avoids both issues: no poisoning, direct `MutexGuard` return.

### LruCache vs HashMap for PyPoolCache

**Ruling: `LruCache` with 10K capacity.** Unbounded `HashMap` causes memory leaks in long-running processes. LruCache evicts the least-recently-used entry when full. 10K entries is sufficient for typical arbitrage workloads (~100 pools) with headroom.

### f64_to_u256 decomposition

**Ruling: Iterative 4-limb decomposition.** The previous 2-limb decomposition (`hi * 2^64 + lo`) silently produced wrong results for values exceeding 128 bits. The 4-limb iterative decomposition correctly handles the full U256 range. f64's 52-bit mantissa limits round-trip precision to ~15-16 significant digits; the lower ~61 digits of a 77-digit U256 are lost in the float conversion (inherent to f64, not fixable).

### f64 vs U512 Möbius solver stack

**Ruling: eliminate f64 entirely; the U512-native solver is the single Möbius module.** The f64 Möbius recurrence (`mobius.rs`, `mobius_v3.rs`, `mobius_v3_v3.rs`) and the f64-seed-then-integer-refine path (`mobius_solve_with_refinement`, `int_mobius_solve`, `mobius_refine_int`, the f64 conversion helpers `u256_to_f64`/`u512_to_f64`) are transitional artifacts. They existed to amortise PyO3 conversion overhead in the old Python-owns-state model; Rust now owns all pool state (`Bot`, the block engines), so there is no FFI conversion cost to amortise and no f64 caller on the hot loop — the production bot (`examples/eth_backrun_v2_v3_v4_rust.py`) drives the Rust `UniswapArbEngine` pump exclusively and does not import the Python f64 solver stack (`ArbSolver`/`MobiusSolver`/`PiecewiseMobiusSolver`/`RustArbSolver`).

Gen-3 (`exact_mobius_solve`, `int_solve_cl_path`, `int_solve_v3_v3`, `exact_solve_mixed_path_n`, `isqrt_u512`) is f64-free for correctness — every `f64` reference in those modules is a `#[cfg(test)]` comparison oracle against the f64 solver, not a solve-path dependency. Gen-3 already covers every topology (`UniswapEngine::solve_path` dispatches V2-V2 / CL×CL / mixed all through gen-3). `mobius_batch` is deleted (zero callers).

The stand-alone block engines (`V2BlockEngine`, `V3BlockEngine`) survive as sub-state composed inside `UniswapEngine` but repoint their own solve calls to gen-3 (their f64 calls were only reached via the stand-alone `V2ArbEngine`/`V3ArbEngine` PyO3 classes, which are not on the production path). Full retirement of the stand-alone PyO3 engine wrappers is a separate concern. `MobiusError` migrates from `mobius.rs` into `mobius_int.rs` — the only structural dependency gen-3 had on gen-1.

f64 survives only as a **test oracle** (gen-3 tests compare against the deleted f64 solver's expected values, pinned before deletion).

**Status: complete.** The f64 Möbius recurrence and the f64-seed-then-refine path are deleted; `mobius.rs` / `mobius_v3.rs` / `mobius_v3_v3.rs` no longer exist, and `mobius_int.rs` carries only the pure-U512 survivors (`IntHopState`, `compute_int_mobius_coefficients`, `int_simulate_path`, `IntMobiusCoefficients`, `SimulationResult`, `MobiusError`). The stand-alone `V2ArbEngine` / `V3ArbEngine` PyO3 wrappers and their pumps are deleted; `V2BlockEngine` / `V3BlockEngine` / `V4BlockEngine` survive as sub-state inside `UniswapEngine` with **state-only** surfaces (no stand-alone `solve_all` / `process_block` / `resolve_path`). The `mobius_py.rs` PyO3 seam exposes only the gen-3-backed classes — `RustPoolCache` (registered-path solving now flows through `exact_mobius_solve`, integer-exact), `RustIntHopState`, `RustArbResult` (no f64 fields). The removed f64 PyO3 surface (`RustArbSolver`, `RustHopState`, `RustV3TickRangeHop`, `RustV3TickRangeSequence`, `RustTickRangeCrossing`, `RustIntMobiusResult`, `py_mobius_refine_int`, `py_int_mobius_solve`, `py_int_simulate_path`, `RustMobius`) is gone.

The Python orchestrator-era solver package (`degenbot/arbitrage/optimizers/`: `ArbSolver` / `MobiusSolver` / `PiecewiseMobiusSolver` / Brent / Newton / SolidlyStable) is **kept** for the not-yet-ported paths. It runs entirely on the pure-Python f64 recurrence in `_solver_utils.py` / `_mobius_math.py` — the dependency on the removed Rust f64 PyO3 surface is severed (the Rust fast paths were deleted, the pure-Python fallback that already existed is now the only path). Per the project's Polars-inspired three-layer direction (ADR-005; Rust core + thin Python orchestrator), these Python solvers will be deleted when their Rust-native equivalents under `UniswapArbEngine` arrive.

## Observability

**DiagnosticPathState**: A serializable snapshot of a Rust-engine arbitrage path at a single block, returned by `PyUniswapArbEngine.diagnostic_inspect_path()`. Captures each hop's pool ID, version, zero-for-one flag, token/currency pair, engine reserves/state, and — when an RPC URL is configured — on-chain state plus a per-field `diff` showing mismatches against the engine view.
_Avoid_: dump, state dump, debug snapshot
