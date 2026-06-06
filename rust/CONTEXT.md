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

**IntHopState**: The solver's integer hop state carrying pre-converted U512 fields (`fee_over_fee_scale`, `scaled_reserve_in`, `scaled_reserve_out`). Constructed once from U256 values; `swap()` and `compute_int_mobius_coefficients()` use U512 arithmetic directly, eliminating per-call conversions.
_Avoid_: hop state, integer state, mobius state

**PyPoolCache**: `parking_lot::Mutex<LruCache<u64, IntHopState>>` — a bounded (10K entry) LRU cache keyed by pool ID, replacing the former unbounded `HashMap`. Uses `parking_lot::Mutex` (no poisoning, returns `MutexGuard` directly from `lock()`) instead of `std::sync::Mutex` or `RefCell`.
_Avoid_: pool cache, solver cache, rust cache

**V2BlockEngine**: A pure-Rust struct that owns the full per-block V2 arbitrage lifecycle: Sync event decoding, pool state updates (`apply_sync`), path resolution, and Mobius solver dispatch. Python participates only in initial construction (`register_pool`, `register_path`) and reading results (`latest_results`). Registration is frozen after `start()`.
_Avoid_: V2 engine, block processor, arbitrage engine

**V2EnginePump**: A standalone async task that drives `V2BlockEngine`. Owns its own `AlloyProvider` (created from an RPC URL string), subscribes to block headers via WS, fetches Sync logs via `eth_getLogs` on each new block, and calls `engine.process_block()`. No dependency on the Python subscription infrastructure. Superseded by `UniswapEnginePump` for mixed-protocol deployments.
_Avoid_: pump, block subscriber, event driver

**V2_SYNC_TOPIC**: `0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1` — the Keccak256 hash of the Uniswap V2 `Sync(uint112,uint112)` event signature. The Rust-side constant matches the Python `V2_SYNC_TOPIC` in `log_decoders.py`.
_Avoid_: sync topic, sync hash, V2 sync signature

**Dual-Orientation Registration**: Each `register_pool()` call creates two `IntHopState` entries — forward (reserve0→reserve1) and reverse (reserve1→reserve0) — mirroring `ArbPoolCacheAdapter`'s pattern. `apply_sync()` updates both from the same Sync event and returns the forward pool key (`Option<u64>`). Paths reference either orientation's pool ID.
_Avoid_: double registration, mirror entry, forward-reverse pair

**PyV2ArbEngine**: The PyO3 wrapper class (exposed as `V2ArbEngine` in Python) that holds an `Arc<parking_lot::Mutex<V2BlockEngine>>` plus a shutdown flag and pump handle. The `start(rpc_url)` method freezes registration and spawns the `V2EnginePump`; `stop()` sets the shutdown flag and aborts the pump task. For testing, `freeze()` freezes registration without starting the pump, and `process_logs()` drives the engine synchronously.
_Avoid_: Python engine, engine wrapper, arb engine pyclass

**BotCore**: The single owner of all runtime state — pool data, token metadata, and calculation methods. All data lives in Rust `HashMap`s; Python objects are thin `PyO3` handles carrying keys (`pool_id`, `Address`). `calculate_tokens_out` and `calculate_tokens_in` dispatch on `PoolEntry` variant and delegate to pure-Rust math (V2 uses `IntHopState::swap()` for exact-in, explicit U256 arithmetic for exact-out).
_Avoid_: core, bot state, rust state owner

**PoolEntry**: An enum with one variant per pool family (`V2(V2PoolState)`, future `V3`, `Curve`, `BalancerWeighted`, `BalancerStable`). Calculation methods match on the variant — no trait objects, no vtable dispatch.
_Avoid_: pool enum, pool type enum

**V2PoolState**: The Rust-owned state for a Uniswap V2 pool: address, token0/token1, reserves, per-direction fee parameters, factory. Replaces Python's `UniswapV2Pool._state_cache` — data lives in `BotCore.pools`, not in a Python object.
_Avoid_: v2 state, v2 pool data

**RegisterV2PoolParams**: A parameter struct for `BotCore::register_v2_pool()` — bundles address, token pair, reserves, per-direction fees, and factory into one argument (replaces 9 positional parameters, satisfies `clippy::too_many_arguments`).
_Avoid_: v2 params, pool params

**PyBotCore**: The `PyO3` wrapper class (exposed as `BotCore` in Python) that holds `Arc<parking_lot::Mutex<BotCore>>`. Provides `register_v2_pool`, `update_v2_pool`, `calculate_tokens_out`, `calculate_tokens_in`, `get_pool`, `register_token`, `get_token` — all delegating through the `Mutex`.
_Avoid_: botcore wrapper, python botcore

**PyPool**: A thin `PyO3` handle (exposed as `Pool` in Python) that holds `Arc<Mutex<BotCore>>` + a `pool_id` key. Property reads and calculation calls cross `PyO3` on every access — the handle owns no state itself.
_Avoid_: pool handle, pool wrapper

**PyToken**: A thin `PyO3` handle (exposed as `Token` in Python) that holds `Arc<Mutex<BotCore>>` + an `Address` key. Currently provides only `address`; symbol/decimals/name will be added when `BotCore` supports token property reads.
_Avoid_: token handle, token wrapper

**EncodedCall**: A pre-encoded EVM call ready for on-chain submission: `to` (target `Address`), `data` (selector + ABI-encoded parameters as `Vec<u8>`), `value` (`U256`, always zero for V2 swaps). Produced by `encode_v2_swap()` — Python never calls `eth_abi.encode()`.
_Avoid_: encoded swap, call data, swap call

**V2_SWAP_SELECTOR**: `[0x02, 0x2c, 0x0d, 0x9f]` — the first 4 bytes of `keccak256("swap(uint256,uint256,address,bytes)")`. Prepended to ABI-encoded parameters in `encode_v2_swap()`.
_Avoid_: swap selector, v2 swap function hash

**V2Snapshot**: A frozen snapshot of V2 pool reserves at a given block: `(reserve0: U256, reserve1: U256, block: u64)`. Stored in `StateHistory`; mirrors Python's `UniswapV2PoolState` frozen dataclass.
_Avoid_: v2 state snapshot, reserve snapshot

**StateHistory**: A `VecDeque<V2Snapshot>` with bounded depth (default 8), supporting `push_state()` (append, same-block replace), `discard_before_block()` (evict old snapshots), and `restore_before_block()` (rollback to prior state). Mirrors Python's `StateCache[T]` — the caller holds the lock, methods are unlocked.
_Avoid_: state cache, history deque, v2 cache

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

**UniswapEnginePump**: The unified pump that drives `UniswapEngine`. Maintains two concurrent WS subscriptions — `newHeads` (block boundary notifications) and `logs` (unfiltered, all filtering in Rust). Each WS log is applied to engine state immediately via `apply_log` (zero-latency state update), but solves are deferred to the top of the next loop iteration — this naturally coalesces multiple logs that arrive between `await` points into a single `solve_dirty` call, avoiding redundant O(total_paths) carry-forward merges for hot pools with multiple events per block. Block boundaries are detected by `log.block_number > current_block` (primary) or `header.number > current_block` (empty-block fallback) — headers provide metadata (timestamp, fees) and trigger empty-block batches. A 60s timeout with no activity triggers `eth_getLogs` backfill for the missing range. Rust-side `filter_relevant_logs()` checks topic0 against 6 monitored topics AND address against registered pool/PoolManager addresses — no provider-side filter needed. Spawned by `PyUniswapArbEngine.start(rpc_url)`.
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

**UniswapEngine**: A unified engine that composes a `V2BlockEngine`, `V3BlockEngine`, and `V4BlockEngine` to handle mixed V2/V3/V4 arbitrage paths in the same per-block lifecycle. Routes Sync events to the V2 engine, Swap events to the V3 engine, and V4 Swap events (from PoolManager) to the V4 engine. Solver dispatch: V2-V2 → `exact_mobius_solve`, V3-V3 and V4-V4 → `int_solve_v3_v3` (same CL math), V2-V3/V3-V2/V4-V3/V3-V4/V4-V2/V2-V4 → `exact_solve_mixed_v2_v3_sequence` (integer effective reserves + piecewise tick-range enumeration). Registration is frozen after `start()` — calls `start()` on all sub-engines.
_Avoid_: mixed engine, combined engine, uniswap engine

**apply_log**: A method on `UniswapEngine` that decodes a single WS log and applies it to the appropriate sub-engine immediately. Returns the set of affected path IDs via the `pool_to_paths` reverse index. Does NOT trigger a solve — the caller (the pump) defers solving to the top of the next loop iteration for coalescing. Sub-engine `apply_*` methods return affected pool keys: `V2BlockEngine::apply_sync` → `Option<u64>`, `V3BlockEngine::apply_swap` → `Option<u64>`, `V3BlockEngine::apply_liquidity_update` → `Option<u64>`, `V4BlockEngine::apply_swap` → `Option<(u64, u64)>`, `V4BlockEngine::apply_liquidity_update` → `Option<(u64, u64)>`.
_Avoid_: per-log update, single-log apply

**solve_dirty**: A method on `UniswapEngine` that takes accumulated dirty pool keys (from `apply_log` calls since the last solve), expires buffered events, rebuilds+solves affected paths, computes the incremental diff, and sends a `ResultBatch` via the unbounded channel. Called by the pump at the top of each loop iteration (coalescing) and on block boundaries (force-flush).
_Avoid_: batch solve, deferred solve, flush dirty

**ResultBatch**: An incremental diff between two solver runs. Carries `solve_block`, block metadata (timestamp, fees, gas), and four lists: `fresh` (paths newly above profit threshold), `updated` (paths above threshold with changed values), `expired` (paths that dropped below threshold), `removed` (de-registered paths). Sent to Python via an unbounded `mpsc` channel — every diff is delivered, no silent drops. The `self.delivered` HashMap in `UniswapEngine` tracks what Python has seen, enabling correct incremental diff computation on each call.
_Avoid_: solver result, solve output, batch result

**Eager Processing**: The pump architecture where WS logs are applied to engine state immediately upon arrival (via `apply_log`), with solver runs deferred and coalesced at the top of the pump loop. This eliminates the 1-block (~12s on mainnet) latency of the previous design where logs were buffered until a block header arrived. The pump loop: (1) check `has_dirty_paths()`, call `solve_dirty` if needed; (2) await next WS event; (3) on `Log` → `apply_log` only (no solve); (4) on `BlockHeader` → record metadata, advance block tracking, handle empty blocks and gap backfill. Multiple logs arriving between `await` points are naturally batched into a single `solve_dirty` call.
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

**Code Injection**: Injecting contract runtime bytecode at a fresh address via `eth_simulateV1`'s `stateOverrides.code` field, enabling simulation of undeployed contracts. The executor contract's runtime bytecode (with immutables OWNER_ADDR/WETH_ADDR baked in) is loaded from `contracts/tstore_executor_runtime_bytecode.txt`. The committed bytecode uses a randomly generated throwaway OWNER_ADDR to avoid leaking operational addresses — override `EXECUTOR_OWNER_ADDRESS` at runtime with the real key. Vyper immutables are embedded in the code section, not storage — no storage slot overrides are needed. `eth_simulateV1` chains calls sequentially, so the 3-call WETH balanceOf pattern correctly captures profit without WETH storage prefunding.
_Avoid_: contract injection, bytecode override, code override

**V4BlockEngine**: A pure-Rust struct that owns the full per-block V4 arbitrage lifecycle: V4 Swap event decoding from PoolManager, pool state updates (including tick-level changes), tick-range computation, and Mobius piecewise solver dispatch. V4 pools are identified by `(pool_manager: Address, pool_id: [u8; 32])` instead of a single contract address. V4 paths carry `IntV3TickRangeSequence` objects (same type as V3) — the solver can't distinguish V3 from V4 hops. Follows the `V3BlockEngine` pattern exactly.
_Avoid_: V4 engine, v4 block processor, v4 arbitrage engine

**V4PoolState**: The engine-internal state for a Uniswap V4 pool: pool_manager address, pool_id (32-byte hash of PoolKey), pool_key (currency0, currency1, fee, tick_spacing, hooks), mutable fields (sqrt_price_x96, liquidity, tick), and `tick_data: HashMap<i32, TickInfo>`. Mirrors `V3PoolState` with added `(pool_manager, pool_id)` identification.
_Avoid_: v4 state, v4 pool data

**V4PoolKey**: A struct with 5 fields matching Solidity's `PoolKey`: `currency0: Address`, `currency1: Address`, `fee: u32`, `tick_spacing: i32`, `hooks: Address`. Used for engine-internal tracking; the hooks field is stored but not used for solving (hook filtering happened at registration).
_Avoid_: pool key, v4 key, poolkey

**V4SwapUpdate**: A pre-decoded V4 Swap update. Carries pool_manager, pool_id, sqrt_price_x96, liquidity, tick, and tick_priors. Same structure as `V3SwapUpdate` with replaced identification fields.
_Avoid_: v4 swap event data, v4 update

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
- **`BotCore`** → **`PoolEntry`**: `BotCore.pools` is a `HashMap<u64, PoolEntry>` — the single source of truth for all pool state, replacing the 4-copy model (Python `StateCache`, Python adapter, Rust `PyPoolCache`, Rust `V2BlockEngine` HashMap)
- **`BotCore`** → **`IntHopState::swap()`**: `calculate_tokens_out` for V2 delegates to `IntHopState::swap()` (U512-based EVM-exact arithmetic), guaranteeing integer-level agreement with the on-chain contract
- **`BotCore`** → **`V2PoolState`**: V2 pools store per-direction fee parameters (`fee_token0`, `fee_token1`), reserves, and update block — eliminating the need for Python's `V2PoolState` cache object
- **`BotCore`** → **`RegisterV2PoolParams`**: The params struct bundles 9 registration fields into one argument; `py_bot.rs` constructs it from Python keyword arguments
- **`PyBotCore`** → **`BotCore`**: The PyO3 wrapper holds `Arc<Mutex<BotCore>>`; every Python call acquires the lock, performs the operation, and releases
- **`PyPool`** → **`BotCore`**: The thin handle holds `Arc<Mutex<BotCore>>` + `pool_id`; `calculate_tokens_out/in` delegate directly to `BotCore` via the key
- **`PyToken`** → **`BotCore`**: The thin handle holds `Arc<Mutex<BotCore>>` + `Address`; future property reads (symbol, decimals) will delegate through the same pattern
- **`encode_v2_swap()`** → **`EncodedCall`**: Produces the pre-encoded calldata by ABI-encoding `(amount0_out, amount1_out, recipient, b"")` with the V2 swap selector; `BotCore::encode_swap()` dispatches to this per pool type
- **`V2_SWAP_SELECTOR`** → **`encode_v2_swap()`**: The 4-byte selector prepended to ABI-encoded parameters; matches Python's `Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]`
- **`V2PoolState`** → **`StateHistory`**: Each V2 pool carries a `StateHistory` of `V2Snapshot` entries; `update_v2_pool` pushes a snapshot, enabling temporal navigation (discard/restore)
- **`StateHistory`** → **`BotCore`**: `BotCore` exposes `v2_history_len`, `v2_discard_before_block`, and `v2_restore_before_block` — PyBotCore delegates to these for Python access
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
- **`solve_dirty`** → **`rebuild_and_solve_affected`**: `solve_dirty` takes the accumulated dirty pool keys, calls `rebuild_and_solve_affected` to re-resolve and re-solve affected paths, then sends the result diff via the channel
- **`solve_dirty`** → **`ResultBatch`**: After solving, `solve_dirty` (via `compute_diff_and_send`) constructs an incremental `ResultBatch` diff and sends it to Python via the unbounded `mpsc` channel
- **`ResultBatch`** → **`self.delivered` HashMap**: `compute_diff_and_send` compares current above-threshold results against `self.delivered` (what Python has already seen) to produce the incremental diff — `fresh`, `updated`, `expired` lists — then updates `self.delivered` to the new set
- **`UniswapEnginePump`** → **`apply_log`**: The pump calls `apply_log` on each WS log for immediate state updates, deferring the solve
- **`UniswapEnginePump`** → **`solve_dirty`**: The pump calls `solve_dirty` at the top of each loop iteration (coalescing multiple logs) and on block boundaries (force-flush)
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
