# Plan 081: V4 Extension — Rust Engine + Universal Executor + Bot

## Overview

Extend the Rust-backed V3/V2 arbitrage system (Plan 080) to support Uniswap V4 pools. V4 uses identical concentrated-liquidity math to V3, so the integer-exact solver (`int_solve_v3_v3`, `exact_solve_mixed_v2_v3_sequence`) is reused directly. The new work is: (1) a `V4BlockEngine` in Rust that tracks V4 pool state behind PoolManager, (2) hook filtering to exclude pools with amount-modifying hooks, (3) a universal executor contract that handles V4's unlock/settle/take settlement alongside existing V2/V3 callbacks, and (4) bot-level V4 pool discovery and path wiring.

## Problem

### Deletion test

If you deleted the V4 pool class, all V4 executor contracts, and `build_managed_pool()`, the bot would still detect V3/V2 arbitrage — it would just miss every V4-V3, V4-V2, and V4-V4 opportunity. V4 pools on Ethereum mainnet represent growing liquidity share; excluding them means systematic missed profit.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| No V4 state in Rust engine | `UniswapEngine` only has V2+V3 | V4 pools can't be solved by the integer-exact engine; path discovery sees only V2/V3 |
| V4 uses different settlement | PoolManager.unlock/settle/take vs V2 callback/V3 callback | Existing `tstore_executor.vy` has no `unlockCallback` — V4 paths can't be encoded through it |
| V4 pools identified by (PoolManager, PoolKey) | `build_pool(address)` doesn't work | Bot's pool discovery, address indexing, and event routing all assume one address = one pool |
| V4 hook flags can modify swap amounts | `BEFORE_SWAP`, `AFTER_SWAP`, `*_RETURNS_DELTA` | Solving assumes V3 math; hooked pools violate this assumption, producing phantom profits |
| V4 amountSpecified convention is OPPOSITE to V3 | Negative = exact input, positive = exact output | Encoding that gets V3 right will get V4 wrong — this caused the IIA bug on V3 (Plan 080), would be reversed on V4 |
| V4 uses NATIVE_ADDRESS for ETH | `currency0 = address(0)` means ETH, not a token | The bot's WETH-centric profit accounting and token pair logic assume ETH is always wrapped |
| 5 separate V4 executor contracts exist | `examples/uniswap_v4_*/` | Each handles one V4 pairing (V4-V2, V4-V3, V4-V4) with fixed-structure code — can't compose V4+V3+V2 in a single transaction |

## Solution

### Architecture: V4BlockEngine alongside V2+V3

V4 pools share V3's concentrated-liquidity math (same tick structure, same sqrtPriceX96, same liquidity tracking). The `V4BlockEngine` mirrors `V3BlockEngine`'s structure but identifies pools by `(Address, PoolId)` instead of just `Address`. The engine constructs `IntV3TickRangeSequence` objects (same type as V3) — the solver can't distinguish V3 from V4 hops, and shouldn't need to.

```
UniswapEngine
├── V2BlockEngine (constant-product)
├── V3BlockEngine (concentrated liquidity, contract-address pools)
└── V4BlockEngine (concentrated liquidity, PoolManager pools)
```

### Hook filtering: gate at registration

Pools with any amount-modifying hook flag set are rejected at registration time. The four hook flags that modify swap amounts:

| Hook Flag | Bit | Effect |
|-----------|-----|--------|
| `BEFORE_SWAP` | 1<<7 | Hook can modify swap parameters before execution |
| `AFTER_SWAP` | 1<<6 | Hook can modify swap results after execution |
| `BEFORE_SWAP_RETURNS_DELTA` | 1<<3 | Hook returns custom swap delta |
| `AFTER_SWAP_RETURNS_DELTA` | 1<<2 | Hook returns custom swap delta |

If `(hook_flags & 0xCC) != 0`, the pool is excluded. This is checked once at registration — no runtime overhead.

### Universal executor: extend tstore_executor.vy

The existing `tstore_executor.vy` uses a generic payload queue with `will_callback` flags. V4 integration adds:

1. **`unlockCallback` handler** — called by PoolManager.unlock(). Resumes payload delivery (same pattern as V2/V3 callbacks).
2. **V4 swap payload type** — a new payload variant carrying `PoolKey` + `SwapParams` instead of raw calldata. The executor encodes the `PoolManager.swap()` call internally since V4 swaps target the PoolManager, not a pool contract.
3. **V4 settlement operations** — `sync`, `settle`, and `take` as explicit payload types. The executor issues these V4 operations at the right points in the callback chain.
4. **Delta resolution** — after a V4 swap, the executor reads the returned delta to determine how much to `settle` (pay) or `take` (receive). This replaces V3's callback-based payment.

This keeps the single-contract model: one executor handles V2+V3+V4 paths in a single transaction.

### V4 swap encoding in the bot

V4 swaps go through PoolManager, not the pool contract. The encoding path differs from V2/V3:

| Path type | Entry point | Settlement |
|-----------|-------------|------------|
| V3-V2 | executor → V3.swap → callback → V2.swap | Token transfers |
| V2-V3 | executor → V2.swap → callback → V3.swap → callback | Token transfers |
| V4-V3 | executor.execute_payloads → PoolManager.unlock → V4.swap → unlockCallback → V3.swap → callback | V4: settle/take; V3: token transfer |
| V3-V4 | executor.execute_payloads → V3.swap → callback → PoolManager.unlock → V4.swap → unlockCallback | V3: token transfer; V4: settle/take |
| V4-V2 | executor.execute_payloads → PoolManager.unlock → V4.swap → unlockCallback → V2.swap | V4: settle/take; V2: invariant check |
| V2-V4 | executor.execute_payloads → V2.swap → callback → PoolManager.unlock → V4.swap → unlockCallback | V2: invariant check; V4: settle/take |
| V4-V4 | executor.execute_payloads → PoolManager.unlock → V4.swap_A → unlockCallback → V4.swap_B | V4: net delta settle/take |

The key insight: V4 swaps happen inside `unlockCallback`, which is called by `PoolManager.unlock()`. The executor's `execute_payloads()` triggers `unlock()` as the first V4 payload, and `unlockCallback` resumes the payload queue inside the unlock context.

### V4 amountSpecified sign convention

V4's convention is OPPOSITE to V3:

| Mode | V3 | V4 |
|------|----|----|
| Exact INPUT | `amountSpecified > 0` | `amountSpecified < 0` |
| Exact OUTPUT | `amountSpecified < 0` | `amountSpecified > 0` |

For arbitrage (always exact-input), V4 encoding uses **negative** `amountSpecified`. The bot's `encode_v3_swap_calldata` already documents this difference.

### NATIVE_ADDRESS handling

V4 PoolKey uses `currency0 = address(0)` for ETH pools. The bot tracks WETH as the profit token. When V4 gives ETH (NATIVE_ADDRESS), the executor wraps it to WETH via `IWETH.deposit()` before the profit measurement. The settlement logic in `unlockCallback` handles this automatically.

## Files Involved

**Primary (new):**
- `rust/src/optimizers/v4_block_engine.rs` — V4 pool state, swap event decoding, tick-range construction
- `rust/src/optimizers/v4_swap_decoder.rs` — Decode V4 Swap events from PoolManager
- `contracts/tstore_executor_v4.vy` — Universal executor with V4 unlockCallback support

**Primary (modified):**
- `rust/src/optimizers/uniswap_engine.rs` — Add `V4BlockEngine` composition, `HopType::V4`, V4 solver dispatch
- `rust/src/optimizers/mod.rs` — Register `v4_block_engine` and `v4_swap_decoder` modules
- `examples/eth_backrun_v3_v2_rust.py` — V4 pool discovery, encoding, path building

**Secondary (modified):**
- `rust/CONTEXT.md` — V4 engine terminology
- `src/degenbot/uniswap/CONTEXT.md` — V4 terms, sign convention ruling
- `AGENTS.md` — V4 hook filtering policy
- `CONTEXT-MAP.md` — V4 module context reference
- `plans/080-rust-bot-poc-path-to-profit.md` — Cross-reference

**No change needed:**
- `rust/src/optimizers/mobius_v3_int.rs` — `IntV3TickRangeSequence` and `int_solve_v3_v3` are reused for V4 directly (same CL math)
- `rust/src/optimizers/mobius_int_exact.rs` — `exact_mobius_solve` reused for V4-V4 two-hop paths (same as V3-V3)
- Existing V4 executor contracts in `examples/` — kept for reference/testing, but superseded by universal executor

## Design Decisions

- **V4BlockEngine mirrors V3BlockEngine**: Same CL math, same tick-range sequence type, same solver dispatch. The only differences are pool identification and swap settlement. This maximizes code reuse and keeps the learning curve minimal.
- **Single HopType::V4**: The engine doesn't subdivide V4 into "V4-with-hooks" vs "V4-without-hooks" — hook filtering happens at registration. If a pool passes the filter, it's treated identically to a V3 pool by the solver.
- **IntV3TickRangeSequence reused**: V4's tick data is identical to V3's (same `{tick: (liquidity_gross, liquidity_net)}` format, same `compute_tick_ranges` logic). The type name is `IntV3TickRangeSequence` — not renamed to `IntCLTickRangeSequence` — to avoid a mass rename across the codebase. The documentation and CONTEXT.md will clarify it applies to both V3 and V4.
- **Universal executor over separate contracts**: A single `tstore_executor_v4.vy` handles V2+V3+V4 in one transaction. The existing fixed-structure V4 executor contracts are kept for reference but don't support cross-protocol (V4+V3+V2) paths.
- **V4 swap payloads carry PoolKey**: V4 swaps can't use raw calldata (the target is always PoolManager, the key varies). Payloads encode `PoolKey + SwapParams` and the executor constructs the `PoolManager.swap()` call internally.
- **V4 delta resolution in executor**: The executor reads the `int256` return from `PoolManager.swap()`, extracts the two int128 deltas, and determines which token to settle (pay) vs take (receive). This is the same delta-resolution logic in the existing `v4_v3_executor.vy` and `v4_v2_executor.vy`.
- **NATIVE_ADDRESS wrapping**: The executor wraps any received ETH to WETH via `IWETH.deposit()` during `unlockCallback` settlement. This keeps the profit measurement in WETH terms (consistent with V2/V3 paths).

## Implementation Order

### Slice 1: V4BlockEngine — pool state and registration

Create `v4_block_engine.rs` and `v4_swap_decoder.rs`:

1. `V4PoolState`: `pool_manager: Address`, `pool_id: [u8; 32]`, `pool_key: V4PoolKey` (5 fields: currency0, currency1, fee, tick_spacing, hooks), `sqrt_price_x96`, `liquidity`, `tick`, `tick_data`, `update_block`
2. `V4PoolKey`: struct with the 5 PoolKey fields
3. `RegisterV4PoolParams`: parameter struct for `register_pool()`
4. Hook filtering: `assert!(hook_flags & AMOUNT_MODIFYING_MASK == 0)` at registration time
5. `register_pool()`: dual-orientation registration (like V2), returns forward pool key
6. `register_path()`: path registration with `V4PoolRef` (pool_key, zero_for_one)
7. `build_int_v4_sequence()`: construct `IntV3TickRangeSequence` from V4 pool state (same as V3's `build_int_v3_sequence`)
8. `apply_swap_update()`: update V4 pool state from decoded Swap event
9. `decode_v4_swap_log()`: decode V4 Swap event (PoolId, amount0, amount1, sqrtPriceX96, liquidity, tick, fee)
10. Dual-orientation address mapping: `(pool_manager, pool_id)` → `(forward_key, reverse_key)`

Run: `just test-rust` — unit tests for registration, hook filtering, tick-range construction.

### Slice 2: V4BlockEngine — path solving and block processing

1. `solve_all()`: dispatch V4-V4 paths to `int_solve_v3_v3`, V4-V3/V4-V2 to `exact_solve_mixed_v2_v3_sequence`
2. `process_block()`: decode V4 Swap events, apply updates, rebuild affected sequences, solve affected paths
3. `latest_results()`: return `Vec<(u64, U256, U256)>` (same format as V2/V3 engines)
4. `initial_solve()`: solve all paths from current state
5. `rebuild_and_solve_affected()`: rebuild sequences + solve for paths referencing updated pools
6. `pool_to_paths` reverse index for dependency tracking

Run: `just test-rust` — unit tests for V4-V4, V4-V3, V4-V2 solving with mock pool data.

### Slice 3: Wire V4BlockEngine into UniswapEngine

Extend `uniswap_engine.rs`:

1. Add `v4_engine: V4BlockEngine` field
2. Add `HopType::V4` variant
3. Add `MixedPoolRef.hop_type == V4` dispatch in `resolve_path()`
4. V4-V4: detect both hops are V4 → `int_solve_v3_v3`
5. V4-V3: detect mixed V4+V3 → `exact_solve_mixed_v2_v3_sequence` with V4 hop providing `IntV3TickRangeSequence`
6. V4-V2: detect mixed V4+V2 → `exact_solve_mixed_v2_v3_sequence` with V4 hop providing `IntV3TickRangeSequence`
7. PyO3 binding: `register_v4_pool()`, `register_path()` with `hop_type="V4"`, `initial_solve()`, `process_logs()`
8. Event routing: V4 Swap events (topic match + from PoolManager) → V4 engine

Run: `just test-rust` — integration tests for mixed V4+V3+V2 engine.

### Slice 4: Universal executor contract — V4 settlement

Create `contracts/tstore_executor_v4.vy`:

1. Add `POOL_MANAGER_ADDRESS: immutable(address)` immutable
2. Add `IUnlockCallback` implementation
3. Add `V4PoolKey` and `SwapParams` structs
4. Add new payload type: `V4SwapPayload` carrying `PoolKey` + `SwapParams`
5. Add `unlockCallback`: resume payload delivery inside PoolManager unlock context
6. Add V4 settlement operations as payload types: `V4Settle`, `V4Take`, `V4Sync`
7. V4 delta resolution: after `PoolManager.swap()` returns, extract amount deltas to determine settle/take amounts
8. NATIVE_ADDRESS wrapping: deposit ETH to WETH during settlement
9. `execute_payloads()` entry point: unchanged — V4 paths start with a `PoolManager.unlock()` payload
10. Compile and verify all selectors

Run: Deploy on anvil fork, test V4-V3 and V4-V2 paths with `debug_traceCall`.

### Slice 5: Bot — V4 pool discovery

Extend `eth_backrun_v3_v2_rust.py`:

1. Add `UNISWAP_V4_POOL_MANAGER_ADDRESS` constant
2. Add V4 pool tracker: discover V4 pools from `PoolCreated` events via `bot.build_managed_pool()`
3. Hook filtering in `EngineRegistry.register_v4_pool()`: check `pool.active_hooks` against the amount-modifying set, skip if any are active
4. `register_v4_pool()`: extract pool state (sqrtPriceX96, liquidity, tick, tick_data) and call `engine.register_v4_pool()`
5. WS subscription: subscribe to PoolManager address for V4 Swap events alongside V2/V3

Run: `--observe` mode — V4 pools discovered, V4-V3 and V4-V2 paths registered, engine results logged.

### Slice 6: Bot — V4 swap encoding

Add encoding functions:

1. `encode_v4_swap_calldata()`: encode `PoolManager.swap(PoolKey, SwapParams, hook_data)` with V4 sign convention (negative for exact-input)
2. `encode_v4v3_payloads()`: V4→V3 path — `PoolManager.unlock()` entry, V4 swap in `unlockCallback`, V3 swap in callback, settlement
3. `encode_v3v4_payloads()`: V3→V4 path — V3 swap with callback, `PoolManager.unlock()` in callback, V4 swap in `unlockCallback`, settlement
4. `encode_v4v2_payloads()`: V4→V2 path — `PoolManager.unlock()`, V4 swap in `unlockCallback`, V2 swap, settlement
5. `encode_v2v4_payloads()`: V2→V4 path — V2 flash swap, callback → `PoolManager.unlock()`, V4 swap in `unlockCallback`, settlement
6. `encode_v4v4_payloads()`: V4→V4 path — `PoolManager.unlock()`, V4 swap A in `unlockCallback`, V4 swap B, net delta settlement
7. Update `encode_payloads()` dispatch to route all 10 path types (V2-V2, V2-V3, V2-V4, V3-V2, V3-V3, V3-V4, V4-V2, V4-V3, V4-V4)

Key encoding differences from V2/V3:
- V4 amountSpecified is **negative** for exact-input (opposite convention)
- V4 entry point is `PoolManager.unlock(data)`, not `pool.swap()`
- V4 settlement uses `sync` + `settle()` (for ERC-20) or `settle(value=)` (for ETH), not ERC20 transfers
- V4 token receipt uses `take(currency, to, amount)`, not direct transfers

Run: `--dry-run` with code injection.

### Slice 7: Bot — path building with V4

1. Extend `build_paths()` to include V4 pools alongside V2/V3
2. V4 pool type detection: `UniswapV4PoolTableBase` alongside V2/V3 base classes
3. Path combination: add V4↔V3, V4↔V2, V4↔V4 pair discovery (same WETH-pair matching as existing V2/V3)
4. Hybrid paths: V4-V3-V2 multi-hop paths (three hops, two intermediate tokens) — deferred to future plan unless simple to add
5. Update `PathInfo`/`HopInfo` dataclasses to carry V4-specific fields (`pool_manager`, `pool_id`, `pool_key`)
6. V4 event processing in `on_event()`: route PoolManager Swap events to V4 engine updates

Run: `--observe` mode with V4 pools, verify all path types appear.

### Slice 8: Validate and clean up

1. Run `just lint` + `just test-all`
2. Update `rust/CONTEXT.md` with V4 engine terminology
3. Update `src/degenbot/uniswap/CONTEXT.md` with V4 sign convention ruling
4. Update `AGENTS.md` with V4 hook filtering policy
5. Update `CONTEXT-MAP.md` with V4 module context reference
6. Save runtime bytecode for code injection
7. Remove any dead code from migration

## Testing

### Per-slice test runs

Each slice runs `just test-rust` and/or `just test-python`.

### New unit tests (Rust)

```rust
// rust tests in v4_block_engine.rs
fn register_v4_pool_creates_both_orientations()
fn register_v4_pool_rejects_hooked_pools()
fn v4_build_int_sequence_matches_v3()
fn v4_v4_two_hop_path_solves()
fn v4_v3_mixed_path_solves()
fn v4_v2_mixed_path_solves()
```

### New unit tests (Python)

```python
# tests/arbitrage/test_optimizers/test_engine_v4.py
def test_v4_v4_profitable_arb()
def test_v4_v3_profitable_arb()
def test_v4_v2_profitable_arb()
def test_v4_hooked_pool_excluded()
```

### Integration tests

- Anvil fork test: deploy universal executor, submit V4-V3 path, verify profit
- Anvil fork test: submit V4-V2 path, verify profit
- Anvil fork test: submit V4-V4 path, verify profit
- Mainnet `--observe`: V4 engine results match Python V4 pool calculations

### Existing test coverage

- `tests/arbitrage/test_optimizers/test_engine_v3v3_vs_brent.py` — 13 tests covering `int_solve_v3_v3`, reused for V4-V4
- `tests/arbitrage/test_optimizers/test_uniswap_arb_engine.py` — 9 engine integration tests, to be extended with V4 paths
- `examples/uniswap_v4_*/tests/` — V4 executor contract tests, reference for settlement logic

## Benefits

- **Leverage**: V4 reuses V3's entire solver infrastructure (`IntV3TickRangeSequence`, `int_solve_v3_v3`, `exact_solve_mixed_v2_v3_sequence`). No new solver code — just engine plumbing and encoding.
- **Locality**: A single `V4BlockEngine` owns all V4 pool state, tick data, and path resolution. Python participates only in construction and result reading (same pattern as V2/V3 engines).
- **Depth**: The `tstore_executor_v4.vy` replaces 5 separate fixed-structure V4 contracts with one generic executor, supporting all V2+V3+V4 path combinations in a single transaction.
- **Safety**: Hook filtering at registration prevents phantom profits from amount-modifying hooks. The `AMOUNT_MODIFYING_MASK` constant makes the policy explicit and auditable.
- **Completeness**: The bot discovers and solves V4-V4, V4-V3, V4-V2, V3-V4, V2-V4 paths alongside existing V3-V3, V3-V2, V2-V3, V2-V2 — full coverage of all concentrated-liquidity × constant-product combinations.

## Risks

- **V4 hook edge cases**: Pools with hooks that don't modify amounts (e.g., `BEFORE_DONATE`, `AFTER_ADD_LIQUIDITY`) are allowed but may still cause unexpected behavior in edge cases. **Mitigation**: The four specifically amount-modifying flags are well-documented in the V4 core library. Pools with only non-amount hooks are mathematically equivalent to no-hook pools.
- **V4 delta encoding**: `PoolManager.swap()` returns a packed `int256` where the two `int128` deltas are in the upper and lower 16 bytes. Incorrect extraction produces wrong settle/take amounts. **Mitigation**: The extraction logic is already implemented and tested in the existing V4 executor contracts (`v4_v3_executor.vy`, `v4_v2_executor.vy`). Port the exact same byte-slicing logic.
- **Nested callbacks (V3 callback inside V4 unlockCallback)**: V3's `uniswapV3SwapCallback` fires inside `unlockCallback` for V4-V3 paths. The executor must handle this nesting correctly. **Mitigation**: The existing `tstore_executor.vy` already supports nested callbacks via the payload queue. The V4 executor adds `unlockCallback` as another callback entry point — same queue, same `will_callback` registration.
- **V4 dynamic fees**: Some V4 pools have dynamic fees (not fixed at pool creation). The engine currently assumes a fixed fee per pool. **Mitigation**: V4 pools with `fee == 0x100000` (dynamic fee flag) must be handled. For the initial implementation, exclude dynamic-fee V4 pools from registration (same approach as hook filtering). Dynamic fee support is deferred to a future plan.
- **PoolManager reentrancy**: V4's unlock context is reentrant — `unlockCallback` can call `unlock()` again. **Mitigation**: The executor uses the `t_v4_unlock` transient flag (same pattern as existing V4 executor contracts). Only one `unlockCallback` is processed per `execute_payloads()` call.
- **Gas costs**: V4's unlock/settle/take pattern adds overhead vs V2/V3 direct transfers. V4 swaps cost ~150K gas vs V3's ~150K, but the settlement operations (sync + settle) add ~50K per token. **Mitigation**: This is inherent to V4's design. The bot's gas estimation accounts for it via `eth_simulateV1` measurement.

## Relationship to Other Plans

- **Plan 079** (Rust-Owned Bot Core): V4 integration follows the same Rust-centric architecture. The `V4BlockEngine` is another engine owned by BotCore. Complementary.
- **Plan 080** (Rust Bot POC — Path to Profit): The bot from Plan 080 is extended with V4 support. All Plan 080 fixes (sign convention, auto-pay, code injection) apply equally to V4 paths. Superset.
- **Independent**: No dependencies on Curve, Balancer, Aerodrome, or database plans.

## Status

- [x] Slice 1: V4BlockEngine — pool state and registration
- [x] Slice 2: V4BlockEngine — path solving and block processing
- [x] Slice 3: Wire V4BlockEngine into UniswapEngine
- [x] Slice 4: Universal executor contract — V4 settlement
- [x] Slice 5: Bot — V4 pool discovery
- [x] Slice 6: Bot — V4 swap encoding
- [x] Slice 7: Bot — path building with V4 (done in Slice 5 — build_paths, event routing, HopInfo/PathInfo all extended for V4)
- [x] Slice 8: Validate and clean up

## Runtime Notes

### V4 pool discovery performance

The database contains ~6.8M V4 pools (~102K without hooks). Including `UniswapV4PoolTable` in `find_paths_async` with `ZERO_ADDRESS` as a start/end token significantly increases path enumeration time. `ENABLE_V4_POOL_DISCOVERY=1` env var controls this — default off so V2/V3 baseline starts in ~240s.

### WS subscription address filter

`engine_registry._v2_keys` and `_v3_keys` are dicts (address→key), not sets. Must use `.keys()` when constructing the WS subscription filter: `_v2_keys.keys() | _v3_keys.keys() | {PM_ADDR}`. Using `dict | set` raises `TypeError`.

### V4 pool construction

`bot.build_managed_pool(address=PM_ADDR, pool_id=pool_hash)` constructs V4 pools from DB records. For V4 ETH pools (currency0=address(0)), the DB stores address(0) as a token — `build_managed_pool` may fail if the ERC20 builder cannot handle the zero address. WETH/ERC20 V4 pools work fine.

### V4 encoding validation status

V4 swap selectors (`unlock=0x48c89491`, `swap=0xfd478a6c`, `take=0x0b0d9c09`, `sync=0xa5841194`, `settle=0x11da60b4`) and ABI encoding (negative amountSpecified for exact-input) are verified in unit tests. Not yet validated against a real anvil fork or mainnet.
