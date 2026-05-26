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

**V2EnginePump**: A standalone async task that drives `V2BlockEngine`. Owns its own `AlloyProvider` (created from an RPC URL string), subscribes to block headers via WS, fetches Sync logs via `eth_getLogs` on each new block, and calls `engine.process_block()`. No dependency on the Python subscription infrastructure.
_Avoid_: pump, block subscriber, event driver

**V2_SYNC_TOPIC**: `0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1` — the Keccak256 hash of the Uniswap V2 `Sync(uint112,uint112)` event signature. The Rust-side constant matches the Python `V2_SYNC_TOPIC` in `log_decoders.py`.
_Avoid_: sync topic, sync hash, V2 sync signature

**Dual-Orientation Registration**: Each `register_pool()` call creates two `IntHopState` entries — forward (reserve0→reserve1) and reverse (reserve1→reserve0) — mirroring `ArbPoolCacheAdapter`'s pattern. `apply_sync()` updates both from the same Sync event. Paths reference either orientation's pool ID.
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

**RegisterV3PoolParams**: A parameter struct for `V3BlockEngine::register_pool()` and future `BotCore::register_v3_pool()` — bundles address, token pair, fee, tick_spacing, factory, sqrt_price_x96, liquidity, tick, and tick_data into one argument.
_Avoid_: v3 params, pool registration params

**V3PoolRef**: A path hop reference: (`pool_idx`, `zero_for_one`). Used in path registration to identify which pool and direction each hop represents.
_Avoid_: pool ref, v3 hop ref

**V3SwapUpdate**: A pre-decoded V3 Swap update for testing without log decoding. Carries pool_address, sqrt_price_x96, liquidity, tick, and tick_priors.
_Avoid_: swap update, v3 swap event data

**PyV3ArbEngine**: The PyO3 wrapper class (exposed as `V3ArbEngine` in Python) that holds `Arc<Mutex<V3BlockEngine>>` plus a shutdown flag and pump handle. Mirrors `PyV2ArbEngine` — Python constructs the engine (registers V3 pools with tick data, registers paths), then starts a Rust-side pump. `process_logs()` drives the engine synchronously for testing.
_Avoid_: Python V3 engine, V3 engine wrapper

**V3EnginePump**: A standalone async task that drives `V3BlockEngine`. Owns its own `AlloyProvider` (created from an RPC URL string), subscribes to block headers via WS, fetches V3 Swap logs via `eth_getLogs` on each new block, and calls `engine.process_block()`. No dependency on the Python subscription infrastructure. Mirrors `V2EnginePump`.

**UniswapEngine**: A unified engine that composes a `V2BlockEngine` and a `V3BlockEngine` to handle mixed V2/V3 arbitrage paths in the same per-block lifecycle. Routes Sync events to the V2 engine and Swap events to the V3 engine. Solver dispatch: V2-V2 → `mobius_solve_with_refinement`, V3-V3 → `solve_v3_v3`, V2-V3/V3-V2 → golden-section search over the piecewise profit function. Registration is frozen after `start()` — calls `start()` on both sub-engines.
_Avoid_: mixed engine, combined engine, uniswap engine

**HopType**: An enum (`V2`, `V3`) identifying which sub-engine owns a given hop in a mixed path.
_Avoid_: hop enum, engine type

**MixedPoolRef**: A pool reference in a mixed path: (`hop_type`, `pool_key`, `zero_for_one`). `pool_key` is the pool ID in the V2 engine or the pool key in the V3 engine.
_Avoid_: mixed ref, mixed hop ref

**PyUniswapArbEngine**: The PyO3 wrapper class (exposed as `UniswapArbEngine` in Python) that holds `Arc<Mutex<UniswapEngine>>` plus a shutdown flag and pump handle. Provides `register_v2_pool`, `register_v3_pool`, `register_path` (with `hop_type` string "V2"/"V3"), `process_logs`, and `latest_results`. Mirrors `PyV2ArbEngine`/`PyV3ArbEngine` pattern.
_Avoid_: Python mixed engine, uniswap engine wrapper
_Avoid_: V3 pump, V3 block subscriber, V3 event driver

**Code Injection**: Injecting contract runtime bytecode at a fresh address via `eth_simulateV1`'s `stateOverrides.code` field, enabling simulation of undeployed contracts. The executor contract's runtime bytecode (with immutables OWNER_ADDR/WETH_ADDR baked in) is loaded from `contracts/tstore_executor_runtime_bytecode.txt`. Vyper immutables are embedded in the code section, not storage — no storage slot overrides are needed. `eth_simulateV1` chains calls sequentially, so the 3-call WETH balanceOf pattern correctly captures profit without WETH storage prefunding.
_Avoid_: contract injection, bytecode override, code override

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
- **`V3EnginePump`** → **`V3_SWAP_TOPIC`**: The pump builds an Alloy `Filter` with `V3_SWAP_TOPIC` and registered pool addresses for `eth_getLogs` queries
- **`UniswapEngine`** → **`V2BlockEngine`**: UniswapEngine composes a V2BlockEngine for V2 pool state and constant-product solving; `start()` calls `v2_engine.start()`
- **`UniswapEngine`** → **`V3BlockEngine`**: UniswapEngine composes a V3BlockEngine for V3 pool state, tick ranges, and piecewise solving; `start()` calls `v3_engine.start()`
- **`UniswapEngine`** → **`solve_v3_v3()`**: Pure V3 paths are dispatched to the piecewise Mobius solver; mixed paths use golden-section search
- **`UniswapEngine`** → **`golden_section_search_max()`**: Mixed V2-V3 paths use golden-section search over the piecewise profit function, simulating V2 and V3 hops sequentially
- **`PyUniswapArbEngine`** → **`UniswapEngine`**: The PyO3 wrapper delegates all operations to the inner engine through a shared `Mutex`; `process_logs()` drives both V2 and V3 updates synchronously
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
