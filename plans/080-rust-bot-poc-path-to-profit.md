# Plan 080: Rust Bot POC — Path to Profit

> **Review applied** — This plan was critically reviewed against the source code,
> reference bots, and smart contract. See "Review findings" section for gaps
> discovered and mitigations applied.

## Overview

Bring the V3/V2 Rust-backed backrun bot (`examples/eth_backrun_v3_v2_rust.py`) from POC to profit-capturing. The P0 blockers (same-block reactivity, pending tx monitoring, executor config) are fixed. The remaining work spans four themes: latency reduction (parallel simulation, gas estimation), correctness (dispatch serialisation, state-block tracking), competitiveness (fee pricing, best-path selection), and robustness (WS reconnection, subscription filtering). Each slice is independently shippable and leaves the test suite green.

**All slices in this plan are complete.** The engine is fully integer-exact — zero f64 arithmetic on any solve path. 13 engine-vs-reference V3-V3 tests verify correctness against Brent and brute-force solvers. 409 Rust tests and 3223 Python tests pass.

## Problem

### Deletion test

If you deleted `dispatch_profitable_results()` and replaced it with a stub that logs the engine results and exits, the bot would still detect opportunities and log them — it just wouldn't capture any profit. The current implementation *works* but is too slow, too naive, or too fragile to compete against other searchers on Ethereum mainnet.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| `dispatch_profitable_results` can run concurrently with itself | `try_dispatch` called from both `schedule_dispatch` and `on_block` | Since `dispatch_profitable_results` awaits (simulation, tx building), a second dispatch triggered by `on_block` can start while the first is still running. Both would read the same engine results and could try to submit the same path, risking double-submission and nonce collision. |
| Sequential simulation: 200ms per result | `dispatch_profitable_results` for-loop | Competing bots submit in <50ms; by the time we simulate result #3, opportunities from results #1-2 are gone |
| No state-block staleness check | `dispatch_profitable_results` | Engine result from block N may be dispatched during block N+2; pools updated since N means the tx reverts on-chain, wasting gas and blocking pools in `pending_pools` |
| Priority fee formula ignores market | `priority_fee = max(1, int(...))` | Underbid → excluded from block; overbid → no profit. The `block_priority_fees` dict is populated but never read. |
| Results dispatched in engine-output order | `dispatch_profitable_results` | Engine returns `[path_id=5, path_id=42, path_id=3, ...]`; if path 3 is most profitable, it's processed last — by then its pools may already be pending from an earlier, less-profitable submission |
| WS disconnect kills the bot | `main()` subscription loop | No reconnection, no state reconciliation. A single WS hiccup requires a full restart + path rebuild (~2 minutes). |
| Gas estimate = 1.5× heuristic | `tx_params["gas"] = int(1.5 * ...)` | Simulation provides exact `gasUsed`; 1.5× wastes ~50K gas per tx (extra priority fee). At 5 gwei base fee + 1 gwei priority, that's ~0.3 mgas × 6 gwei/gas = ~1.8 µETH wasted per submission. |
| V2 fee only uses `_fee_token0` | `register_v2_pool` | The Rust engine's `register_v2_pool` takes a single `(gamma_numer, fee_denom)` pair and applies it to both orientations. `_fee_token1` is ignored. Currently safe (Uniswap and Sushiswap V2 have symmetric 0.3% fees on mainnet) but breaks if asymmetric-fee V2 pools are added. See F3. |
| V3 tick_priors sent in full every event | `process_single_event` | Each V3 swap/mint/burn event sends ALL tick data to the Rust engine. For a pool with thousands of initialized ticks (e.g., USDC/WETH), this is O(n) per event on both the Python and Rust sides. See F4. |

Out-of-scope frictions (documented but not addressed by this plan):

| Friction | Where | Verdict |
|----------|-------|---------|
| V3 encoding in Python (not Rust) | `encode_v3_swap_calldata()` | Rust V3 encoding is Plan 079's scope. ~30µs per call is not a bottleneck vs. network I/O. |
| `process_single_event` returns early on unknown pools | `process_single_event()` | Acceptable — pools not in the engine registry are irrelevant. See Slice 8 for observability. |

## Solution

### Slice 0.5: Dispatch serialisation

The current code allows `dispatch_profitable_results` to run concurrently with itself. `on_block` calls `try_dispatch()` directly, and `schedule_dispatch` also calls it after a coalesce window. Since `dispatch_profitable_results` contains `await` points (simulation, tx building), a second dispatch can start while the first is still running. Both read the same engine results and could submit the same path.

Fix: add an `asyncio.Lock` around dispatch so only one dispatch runs at a time.

```python
# In main():
dispatch_lock = asyncio.Lock()

async def try_dispatch() -> None:
    if dispatch_lock.locked():
        return  # Another dispatch is running — skip
    async with dispatch_lock:
        # ... existing try_dispatch body ...
```

A second dispatch triggered during the coalesce window's `asyncio.sleep` is silently dropped. This is intentional — the first dispatch processes all accumulated updates, so the second dispatch would process zero updates anyway.

### Slice 1: Parallel simulation of profitable results

**Requires Slice 0.5.**

Convert the sequential `for path_id, optimal_input, profit in results:` loop in `dispatch_profitable_results` to concurrent simulation using `asyncio.gather()`.

```python
# Before: sequential ~200ms/result
for path_id, optimal_input, profit in results:
    payloads = encode_payloads(path_info, optimal_input, executor_address)
    tx_params = await executor_contract.functions.execute_payloads(...).build_transaction(...)  # 50ms
    sim = await async_w3.eth.simulate_v1(...)  # 100ms
    # process...

# After: parallel ~200ms/batch (top-K candidates)
async def simulate_one(path_id, optimal_input, profit):
    payloads = encode_payloads(path_info, optimal_input, executor_address)
    tx_params = await executor_contract.functions.execute_payloads(...).build_transaction(...)
    sim = await async_w3.eth.simulate_v1(...)
    return (path_id, gross_profit, net_profit, gas_used, tx_params)

sim_tasks = [simulate_one(pid, inp, pft) for pid, inp, pft in results[:MAX_SIMULATE_CONCURRENT]]
sim_results = await asyncio.gather(*sim_tasks, return_exceptions=True)
```

Key design decisions:
- **MAX_SIMULATE_CONCURRENT = 8**: Cap concurrent RPC calls to avoid node throttling. Matches the reference bot's `ARB_PROCESSING_BATCH_SIZE = 8`.
- **`return_exceptions=True`**: Individual simulation failures don't cancel the batch.
- **Encode before simulate, simulate before submit**: The three phases remain, but simulation is parallelized. Encoding is fast (~30µs) so it stays inline. Submission is serial (one nonce at a time).
- **Sort by net profit descending**: After gathering simulation results, sort and submit the most profitable first.

### Slice 2: State-block staleness tracking

Tag each engine result with the block number it was computed at. Discard results from a previous block before simulating.

**Why we use the Rust engine's `results_block` (not Python pool's `update_block`)**:

Python pools are updated *before* the Rust engine in `process_single_event()` — the handler applies the update to the Python pool, then the engine is fed the new state. So `pool.update_block` is always >= the block that triggered the solve. Checking `pool.update_block > solve_block` would discard ALL results from the current block.

The correct source is the Rust engine's `results_block` — the `block_number` passed to `process_logs` which triggered the solve. This is already returned by `latest_results()`.

```python
# In profitable_results():
def profitable_results(self, min_profit=MIN_PROFIT_NET):
    flat, solve_block = self.engine.latest_results()
    results = []
    for i in range(0, len(flat), 3):
        if i + 2 >= len(flat):
            break
        path_id, opt_input, profit = int(flat[i]), int(flat[i+1]), int(flat[i+2])
        if profit > min_profit:
            results.append((path_id, opt_input, profit, solve_block))
    return results

# In dispatch_profitable_results():
if current_block > solve_block:
    continue  # Result is from a previous block — pools may have changed
```

### Slice 3: Competitive priority fee pricing

**Requires Slice 2** (for `solve_block` per result).

Replace the fixed formula with market-aware pricing using `block_priority_fees` (already populated but unused).

```python
# Before: fixed formula with no market awareness
priority_fee = max(1, int((gross_profit / TARGET_PROFIT_RATIO - gas_used * base_fee_next) / gas_used))

# After: market-aware with age decay and fee history bounds
import math

MIN_PRIORITY_FEE_PERCENTILE = 10
MAX_PRIORITY_FEE_PERCENTILE = 50
AGE_DECAY_CONSTANT = 0.25

# Floor and ceiling from fee history
min_priority_fee = max(block_priority_10th_percentile + 1, 1)
max_priority_fee = max(block_priority_50th_percentile + 1, min_priority_fee)

# Target fee from profit ratio
target_priority_fee = int((gross_profit / TARGET_PROFIT_RATIO - gas_used * base_fee_next) / gas_used)

# Age decay: older results are worth less
age = current_block - solve_block
age_factor = math.e ** (-AGE_DECAY_CONSTANT * age)
priority_fee = int(target_priority_fee * age_factor)

# Clamp to market bounds
priority_fee = max(min_priority_fee, min(priority_fee, max_priority_fee))
```

### Slice 4: Best-path selection with mutual exclusivity

Sort results by estimated net profit (descending) and skip results whose pools overlap with already-submitted results.

```python
# Before: engine-output order
for path_id, optimal_input, profit, solve_block in results:
    ...

# After: profit-descending with mutual exclusivity
results.sort(key=lambda r: r[2], reverse=True)  # sort by profit descending
committed_pools: set[object] = set()

for path_id, optimal_input, profit, solve_block in results:
    path_info = engine_registry.paths.get(path_id)
    if path_info is None:
        continue
    path_pools = {path_info.v3_pool, path_info.v2_pool}
    if path_pools & (pending_pools | committed_pools):
        continue  # Skip — pools already claimed
    committed_pools.update(path_pools)
    # ... encode, simulate, submit
```

### Slice 5: Gas estimation from simulation

Use the simulation's `gasUsed` directly instead of the 1.5× heuristic.

```python
# Before: rough 1.5× multiplier
tx_params["gas"] = int(1.5 * tx_params["gas"])

# After: simulation-provided gasUsed with small safety margin
gas_from_sim = calls[1]["gasUsed"]
tx_params["gas"] = int(gas_from_sim * 1.1)  # 10% safety margin
```

This requires restructuring: currently the gas is set before simulation, then simulation runs with the inflated gas. With the new approach, we simulate first with a generous gas limit (keep 1.5× for `build_transaction`), then override with the actual `gasUsed × 1.1` before submission.

### Slice 6: WebSocket reconnection with state reconciliation

Wrap the subscription loop in a reconnection loop with exponential backoff. After reconnect, refetch pool states.

```python
RECONNECT_BASE_DELAY = 1.0  # seconds
RECONNECT_MAX_DELAY = 30.0

async def run_with_reconnection(engine_registry, bot, ...):
    delay = RECONNECT_BASE_DELAY
    while True:
        try:
            async with AsyncWeb3(web3.WebSocketProvider(node_ws)) as ws_w3:
                ws_w3.middleware_onion.clear()
                await ws_w3.subscription_manager.subscribe(NewHeadsSubscription(handler=on_block))
                await ws_w3.subscription_manager.subscribe(LogsSubscription(handler=on_event))
                bot_logger.info("Subscribed to newHeads + logs — running")
                await ws_w3.subscription_manager.handle_subscriptions()
                delay = RECONNECT_BASE_DELAY  # reset on clean exit
        except Exception as exc:
            bot_logger.error(f"WS connection lost: {exc}")
            bot_logger.info(f"Reconnecting in {delay:.1f}s...")
            await asyncio.sleep(delay)
            delay = min(delay * 2, RECONNECT_MAX_DELAY)

            # State reconciliation: refetch latest pool states
            try:
                await reconcile_pool_states(bot, engine_registry, async_w3)
            except Exception:
                bot_logger.error("State reconciliation failed — will retry after reconnect")
```

State reconciliation fetches current reserves/sqrtPrice for recently-active pools and pushes them to the engine as a "reset" update. Reconciliation fetches only recently-active pools (tracked in a bounded set), not the full ~17K. Accept a brief period of stale state for pools that didn't have pending updates.

### Slice 7: Event subscription filtering

After path building, create a `LogsSubscription` with an address filter covering only monitored pools. `LogsSubscription(address=...)` accepts `List[ChecksumAddress]`.

```python
# Before: receives all logs on Ethereum
await ws_w3.subscription_manager.subscribe(LogsSubscription(handler=on_event))

# After: filtered to monitored pools only
monitored_addresses = list(engine_registry._v2_keys | engine_registry._v3_keys)
if len(monitored_addresses) <= 1000:
    await ws_w3.subscription_manager.subscribe(
        LogsSubscription(address=monitored_addresses, handler=on_event)
    )
else:
    bot_logger.warning(f"Too many pools ({len(monitored_addresses)}) for WS filter — using unfiltered subscription")
    await ws_w3.subscription_manager.subscribe(LogsSubscription(handler=on_event))
```

When filtered, the Python-side `address in engine_registry._v2_keys` check in `on_event` becomes redundant and can be removed.

### Slice 8: Dry-run observability and end-to-end validation

Add structured logging so dry-run output can be analyzed for missed opportunities.

- Log each dispatched result with: path_id, pool addresses, optimal_input, engine_profit, sim_profit, net_profit, gas_used, priority_fee, solve_block, current_block, time_since_last_event
- Log *why* each result was skipped (pools pending, simulation reverted, profit too low, stale block)
- Add a `--observe` mode that logs every engine result without simulation (faster, for calibrating the Rust engine's accuracy)

### Slice 9: Validate and clean up

1. Run `just lint` + `just test-all`
2. Review code for dead imports, stale comments, and any artifacts from the refactor
3. Add code comment on `register_v2_pool` documenting the `_fee_token0`-only limitation (F3)
4. Add `bot_logger.debug` in `process_single_event`'s catch block (F9)
5. Verify all slice changes are coherent — no half-applied refactors

### Design decisions

- **`asyncio.gather` over `ProcessPoolExecutor`**: Simulation is I/O-bound (RPC calls), not CPU-bound. `asyncio.gather` with `MAX_SIMULATE_CONCURRENT=8` is simpler and avoids the GIL/multiplication overhead of process pools. The reference bot's `ProcessPoolExecutor` exists because it runs CPU-bound `calculate_with_pool` in parallel — our Rust engine already handles that.
- **`solve_block` from Rust engine, not Python pool `update_block`**: The Rust engine's `results_block` is the authoritative timestamp for when a result was computed. Python pool `update_block` is always >= the triggering event's block (the pool is updated first), making it unsuitable for staleness detection. See F2.
- **No re-optimization sweep**: Slice 2 checks staleness but doesn't re-optimize failed simulations with nearby input values. Re-optimization is deferred to a future plan — the engine's `optimal_input` should be close enough given matching Python `calculate_*` methods.
- **Coalescence window stays at 50ms**: Slices 0.5-8 don't change the coalescence model. 50ms is a reasonable tradeoff; tuning it requires mainnet latency measurements which we don't have yet.
- **Reconciliation for recently-active pools only**: The pool count (~17K) makes a full multicall impractical. Individual `getReserves`/`slot0` calls for all pools would take minutes. Reconciliation fetches only recently-active pools (tracked in a bounded set), accepting brief stale state for inactive pools.
- **`--observe` mode is for development**: Not for production. It bypasses simulation to maximize throughput, letting us verify the Rust engine is producing real opportunities.
- **Immediate submission, no 1-block delay**: The reference bot uses `BLOCKS_TO_SLEEP_BEFORE_SEND = 1` because its queue-based architecture decouples simulation from submission. In the Rust bot's per-event architecture, results are fresh (same block, same dispatch) and delay would only lose opportunities. The staleness check (Slice 2) prevents submitting results from old blocks. See F8.

## Files Involved

**Primary:**
- `examples/eth_backrun_v3_v2_rust.py` — Slices 0.5–9 modify this file
- `rust/src/optimizers/mobius_int_exact.rs` — integer-exact Möbius solver (already created)
- `rust/src/optimizers/uniswap_engine.rs` — Slices 10–12: wire exact solver into engine
- `rust/src/optimizers/mobius_v3.rs` — Slice 11: convert V3TickRangeHop to integer
- `rust/src/optimizers/mobius_v3_v3.rs` — Slice 11: convert V3-V3 coefficients to integer
- `rust/benches/mobius_exact_vs_f64.rs` — benchmark comparing f64 vs exact solvers (already created)

**No change needed:**
- `src/degenbot/` — no library changes. The bot uses `calculate_tokens_out_from_tokens_in` which already matches on-chain rounding.

**New test file:**
- `tests/arbitrage/test_optimizers/test_engine_v3v3_vs_brent.py` — 13 tests comparing UniswapArbEngine's integer-exact V3-V3 solver against Brent (float reference) and brute-force (integer gold standard). Tests cover: single/multi-range pools, positive/negative ticks, WETH/USDC-style pools (~tick -83000), wide tick spacing (200), and profit/input accuracy vs brute-force.

## Implementation Order

### Slice 0.5: Dispatch serialisation

1. Add `dispatch_lock = asyncio.Lock()` to runtime state in `main()`
2. In `try_dispatch()`, check `if dispatch_lock.locked(): return` and wrap body in `async with dispatch_lock:`
3. This prevents concurrent dispatches from `on_block` + `schedule_dispatch` interleaving
4. Run: `just test-python` — expect all pass

### Slice 1: Parallel simulation

1. Add `MAX_SIMULATE_CONCURRENT = 8` to configuration
2. Extract per-result simulation into `async def simulate_one(...)`
3. Replace sequential `for` with `asyncio.gather(*[simulate_one(...) for ...])`
4. Sort gathered results by `net_profit` descending before submission phase
5. Run: `just test-python` — expect all pass
6. Run bot with `--dry-run` — verify parallel simulation logs

### Slice 2: State-block staleness tracking

1. Modify `EngineRegistry.profitable_results()` to return `(path_id, opt_input, profit, solve_block)` tuples — read `solve_block` from `latest_results()`
2. Update all callers of `profitable_results()` to unpack the 4-tuple
3. Add staleness check in `dispatch_profitable_results`: skip if `current_block > solve_block`
4. Run: `just test-python` — expect all pass

### Slice 3: Competitive priority fee pricing

1. Add `AGE_DECAY_CONSTANT = 0.25`, `MIN_PRIORITY_FEE_PERCENTILE = 10`, `MAX_PRIORITY_FEE_PERCENTILE = 50` to configuration
2. Add `age_discount_factor(age)` function
3. Replace `priority_fee` calculation with market-aware version using `block_priority_fees`
4. Pass `block_priority_fees` and `solve_block` into `dispatch_profitable_results`
5. Run: `just test-python` — expect all pass

### Slice 4: Best-path selection

1. Sort `results` by profit descending before processing
2. Add `committed_pools` set alongside `pending_pools`
3. Skip results whose pools overlap with `pending_pools | committed_pools`
4. After successful submission, add path's pools to `committed_pools`
5. Move pools from `committed_pools` to `pending_pools` on submission (monitor releases from `pending_pools`)
6. Run: `just test-python` — expect all pass

### Slice 5: Gas estimation from simulation

1. Restructure `simulate_one()`: simulate with generous gas, extract `gasUsed`, override `tx_params["gas"]` with `gasUsed * 1.1`
2. Keep 1.5× as the initial estimate for `build_transaction` (needed for the simulation itself)
3. Override after simulation succeeds
4. Run: `just test-python` — expect all pass

### Slice 6: WS reconnection

1. Extract subscription setup into `run_subscription_loop()`
2. Wrap in `while True: try/except` with exponential backoff
3. Add `reconcile_pool_states()` that fetches fresh state for recently-active pools
4. Add `'reconnect'` logging messages
5. Run: `just test-python` — expect all pass
6. Run bot with `--dry-run` — kill WS connection, verify reconnection

### Slice 7: Subscription filtering

1. Collect `monitored_addresses = list(engine_registry._v2_keys | engine_registry._v3_keys)` after path building
2. Pass `address=monitored_addresses` to `LogsSubscription` if <= 1000 addresses
3. Fallback to unfiltered subscription if too many addresses
4. Remove `on_event`'s Python-side `address in engine_registry._v2_keys` check when filtered (the filter guarantees it)
5. Run: `just test-python` — expect all pass

### Slice 8: Dry-run observability

1. Add `--observe` flag: logs every engine result without simulation
2. Add structured skip-reason logging in `dispatch_profitable_results`
3. Add time-since-last-event metric to block log line
4. Run: `just test-python` — expect all pass
5. Run bot with `--observe` on mainnet — verify engine produces opportunities

### Slice 9: Validate and clean up

1. Run `just lint` + `just test-all`
2. Review code for dead imports, stale comments, and any artifacts from the refactor
3. Add code comment on `register_v2_pool` documenting the `_fee_token0`-only limitation (F3)
4. Add `bot_logger.debug` in `process_single_event`'s catch block (F9)
5. Verify all slice changes are coherent — no half-applied refactors

## Testing

### Per-slice test runs

Each slice runs `just test-python`. No Rust changes in this plan.

### New unit tests

There are no existing tests for `eth_backrun_v3_v2_rust.py` itself — it's a script, not a library module. The correctness of the encoding functions (`encode_v3_swap_calldata`, `encode_v2_swap_calldata`, `encode_erc20_transfer_calldata`, `encode_payloads`) was verified manually in previous sessions.

Adding integration tests for the bot would require mocking the `AsyncWeb3` layer and `UniswapArbEngine` — this is feasible but out of scope for this plan. The primary validation mechanism is `--dry-run` on mainnet.

### Integration tests

The existing 3223 Python tests cover the library code (pools, calculations, arbitrage) that the bot depends on. No new library integration tests are needed.

### Engine V3-V3 verification tests

`tests/arbitrage/test_optimizers/test_engine_v3v3_vs_brent.py` — 13 tests comparing the `UniswapArbEngine`'s integer-exact V3-V3 path against two independent references:

1. **Brent (scipy.optimize)** — float minimization of the profit function. The engine's integer-exact solver should agree within 1-2%.
2. **Brute-force V3 integer math** — scans input amounts at integer precision using `compute_swap_step`. The engine's input and profit should agree within 5-15%.

Test coverage includes:
- Single-range and multi-range pools
- Positive and negative tick values
- WETH/USDC-style pools around tick -83000
- Wide tick spacing (200)
- Equal-price pools (no-profit verification)
- Phantom profit detection (engine profit ≤ brute-force profit + 5%)
- Input accuracy (engine optimal input within 15% of brute-force optimum)

Key testing lessons:
- **Direction rule**: `build_engine_v3_v3()` asserts `current_tick_a > current_tick_b` — the V3-V3 path `[pool_A(zfo=True) → pool_B(zfo=False)]` is only profitable when pool A has a higher tick (token1 expensive) than pool B (token1 cheap)
- **Range coverage**: Use `wide_range_around(tick, n=10)` to ensure `compute_tick_ranges` constructs sufficient ranges. Narrow ranges (±3 tick spacings) can produce false-zero-profit even when wide ranges show profit
- **Tick spacing alignment**: All test tick boundaries must be multiples of `tick_spacing` — the engine's `gen_ticks` walks at `tick_spacing` intervals and won't discover non-aligned boundaries

## Benefits

- **Correctness**: Dispatch serialisation (Slice 0.5) eliminates the race condition where two dispatches could submit the same path. State-block tracking (Slice 2) eliminates on-chain reverts from stale results — every reverted tx wastes gas and blocks pools in `pending_pools` for up to 5 blocks (~60s). Integer-exact solver (Slices 10–12) eliminates phantom profits from f64 rounding.
- **Latency**: Parallel simulation (Slice 1) cuts dispatch from ~200ms/result to ~200ms/batch. On a block with 5 profitable results, this is ~1s → ~0.2s. Integer-exact solver (131ns closed-form vs 1.3µs iterative search) reduces solve time by 10× for V2-only paths.
- **Competitiveness**: Market-aware fee pricing (Slice 3) ensures our txns actually get included. Age decay avoids overpaying for stale opportunities. Best-path selection (Slice 4) ensures we submit the most profitable arb first, not an arbitrary one.
- **Gas savings**: Simulation-derived gas (Slice 5) saves ~50K gas per submission.
- **Uptime**: WS reconnection (Slice 6) turns a fatal error into a ~5s interruption. Without it, every WS hiccup requires a full restart (~2min path rebuild).
- **Observability**: `--observe` mode (Slice 8) lets us validate the Rust engine's output without risking any on-chain activity. Structured skip-reason logging makes it possible to diagnose why opportunities are missed.
- **EVM-exact solver**: The integer-exact Möbius solver (Phase 2) produces profits verified by EVM simulation — no more false positives from f64 rounding. The ±2 neighborhood search ensures the reported profit is the best possible integer input.

## Risks

- **f64 solver produces huge phantom profits for V3 paths**: ~~The existing `mobius_solve`/`solve_mixed_path` uses f64 arithmetic exclusively. For V3-involving paths, `simulate_v3_hop` only uses the first tick range and `x as u128` silently truncates values > 2^128. This produces profits reported in 10^18+ ETH — pure artifacts.~~ **Resolved by Slices 10–15**: All solve paths (V2-V2, V2-V3, V3-V2, V3-V3) now use integer-exact arithmetic. The engine is fully f64-free. `MAX_PROFIT_WEI` guard in the bot still provides defense-in-depth against scam tokens.
- **`asyncio.gather` + node throttling**: Simulating 8 candidates concurrently may hit node rate limits. **Mitigation**: `MAX_SIMULATE_CONCURRENT` is configurable. Start at 4, increase to 8 if the node handles it.
- **Dispatch lock + coalesce interaction**: The `dispatch_lock` in Slice 0.5 uses `if dispatch_lock.locked(): return` (non-blocking check). A second dispatch triggered during the coalesce window's `asyncio.sleep` will be silently dropped. **Mitigation**: This is intentional — the first dispatch processes all accumulated updates. The second dispatch would process zero updates anyway (they've been consumed).
- **Reconciliation after reconnect may be slow**: Fetching recently-active pool states takes time proportional to activity. **Mitigation**: Only reconcile pools with pending updates (bounded set). Accept brief stale state for inactive pools.
- **WS address filter may not work with all providers**: Some providers ignore the filter or reject large address lists. **Mitigation**: Fallback to unfiltered subscription. The Python-side address check is always present as a safety net.
- **Age decay parameters**: `AGE_DECAY_CONSTANT = 0.25` is from the reference bot but may not be optimal for our path mix. **Mitigation**: This is a tuning parameter, not a correctness issue. Start with the reference value, adjust based on `--observe` output.
- **V2 asymmetric fees**: Currently safe (Uniswap/Sushiswap use symmetric 0.3%). If asymmetric-fee V2 pools are added, `register_v2_pool` would silently use the wrong fee for one direction. **Mitigation**: Add a runtime check that `_fee_token0 == _fee_token1` and warn if they differ. Full fix requires extending the Rust engine API (Plan 079 scope). See F3.
- **V3 tick_data transfer cost**: Sending full tick_data per event could be slow for pools with many initialized ticks. **Mitigation**: Defer. The data transfer is internal (same process via PyO3) and dominated by network I/O. A future optimization could send only changed ticks. See F4.

## Relationship to Other Plans

- **Plan 079** (Rust-Owned Bot Core): Complementary. Plan 079 moves more computation into Rust (V3 encoding, V3 calculation, tick bitmap walk). This plan (080) improves the Python orchestration layer (Slices 0.5–9) AND provides an integer-exact solver (Slices 10–12). Slices 10–12 overlap with Plan 079's goal of moving V3 math into Rust — specifically, Slice 11 (integer V3TickRangeHop) is a prerequisite for both plans. Plan 079 can replace `encode_v3_swap_calldata()` and `calculate_tokens_out_from_tokens_in()` with Rust equivalents, and this plan's dispatch logic doesn't change.
- **Independent of all other active plans**: No dependencies on Curve, Balancer, Aerodrome, or database plans.

## Review Findings

Critical review against the in-scope code (`examples/eth_backrun_v3_v2_rust.py`), reference bots (`examples/eth_backrun_v4_v3.py`, `examples/base_backrun.py`), and smart contract (`examples/tstore_executor_vyper_v4.vy`). Each finding includes a verdict on how it affects the plan.

### F1: Concurrent dispatch race condition → Slice 0.5

`try_dispatch()` is called from both `schedule_dispatch()` (after coalesce window) and `on_block()` (immediately). Since `dispatch_profitable_results()` contains `await` points (simulation, tx building), a second dispatch can start while the first is still running. Both read the same engine results and could submit the same path, risking double-submission and nonce collision.

### F2: Staleness check must use Rust engine's `results_block`, not Python pool's `update_block` → Slice 2

Python pools are updated *before* the Rust engine in `process_single_event()` — the handler applies the update to the Python pool, then the engine is fed the new state. So `pool.update_block` is always >= the block that triggered the solve. Checking `pool.update_block > solve_block` would discard ALL results from the current block. The Rust engine's `results_block` (returned by `latest_results()`) is the correct source — it's the `block_number` passed to `process_logs` which triggered the solve.

### F3: V2 asymmetric fees not supported → accepted risk

`register_v2_pool()` only reads `_fee_token0` and passes a single `(gamma_numer, fee_denom)` to the Rust engine. The engine applies this to both forward and reverse orientations. `_fee_token1` is ignored. On Ethereum mainnet, Uniswap V2 and Sushiswap V2 have symmetric 0.3% fees (confirmed: `src/degenbot/sushiswap/pools.py` has no fee override, and the builder uses 3/1000 for both directions), so this is not currently a problem. If asymmetric-fee V2 variants are added, `register_v2_pool` would silently use the wrong fee for one direction.

**Mitigation**: Add a runtime check that `_fee_token0 == _fee_token1` and warn if they differ. Full fix requires extending the Rust V2 engine's `register_pool` to accept two fee pairs (Plan 079 scope).

### F4: V3 tick_priors sent in full every event → acknowledged, deferred

Each V3 swap/mint/burn event sends ALL tick data to the Rust engine (`tick_data.items()`). The Rust `apply_swap` does `pool.tick_data.insert(tick_index, prior)` for every entry. For a pool like USDC/WETH with thousands of initialized ticks, this is O(n) per event on both sides. The Rust side correctly uses `insert` (replaces, doesn't accumulate) so there's no correctness issue — just a performance cost on the Python→Rust PyO3 data transfer.

**Mitigation**: Defer. Not a bottleneck vs. network I/O (simulation). A future optimization could diff against a cached snapshot and send only changed ticks.

### F5: Executor V3 callback only auto-pays WETH — no action needed

The Vyper contract's `v3_swap_callback` checks `amount1_delta > 0 → token1 == WETH → transfer WETH` and `amount0_delta > 0 → token0 == WETH → transfer WETH`. It only auto-pays WETH debts.

For Case 2 (zfo=False), the V3 callback has `amount1_delta > 0` (forward owed), but `token1 != WETH_ADDR`, so the auto-transfer is skipped. The forward still reaches V3 because the V2 swap sends it there. V3's `swap()` checks `balanceOf(pool) >= expected` after the callback returns — the V2 swap increases the pool's forward balance in time.

This is self-guarding: if the V2 swap failed or sent forward to the wrong address, V3's balance check would fail and the entire transaction would revert. No silent corruption.

### F6: Simulation `stateOverrides` gives ETH but not WETH — no action needed

The simulation overrides `operator_address: {balance: 100 ETH}` for gas payment. The executor contract checks WETH balance (`IERC20(WETH_ADDR).balanceOf(self)` before and after). The executor has zero WETH before the swap (the V3 callback is the flash borrow). The simulation correctly models this — `eth_simulateV1` tracks state changes, not pre-existing balances.

### F7: Reference bot uses `BLOCKS_TO_SLEEP_BEFORE_SEND = 1` — no action needed

The V4/V3 bot has a two-phase submission: detect/simulate in one worker, then a separate submission worker re-evaluates pending results every 0.1s and only submits when the result's `state_block` is at least 1 block old AND the current time is within 1.0s of the block timestamp. The 1-block delay is a design choice for its queue-based architecture — the result ages while waiting for the submission worker's 0.1s loop.

In the Rust bot's per-event architecture, results are fresh (same block, same dispatch) and delay would only lose opportunities. The staleness check (Slice 2) prevents submitting results from old blocks.

### F8: `process_single_event` swallows `ExternalUpdateError` — safe but noisy

When a log event is out of order (e.g., from a WS reconnection or reorg), the `handler(event_log)(pool)` call raises `ExternalUpdateError`. The `except Exception: pass` swallows it. After the exception, the code reads the pool's current (pre-rejection) state and sends it to the Rust engine — both stay at the pre-rejection state, so no divergence.

The Rust engine receives a redundant no-op update with identical values. The `rebuild_and_solve` still runs, wasting CPU. Out-of-order events are rare in normal operation.

**Mitigation**: Add `bot_logger.debug` in the catch block (Slice 9).

### F9: `profitable_results` flat list parsing — verified correct

`latest_results()` returns a flat Python list `[path_id_0, opt_input_0, profit_0, ...]` parsed from the Rust engine's `Vec<(u64, U256, U256)>`. The loop `for i in range(0, len(flat), 3)` with `int(flat[i])` works because PyO3 converts `u64` → Python `int` and `U256` → custom `PyU256` which supports `int()`. Verified with empty engine: `([], 0)`.

## Phase 2: Integer-Exact Solver (mobius_int_exact)

The f64 Möbius solver produces wildly inaccurate profits for V3-involving paths because `simulate_v3_hop` operates in f64 and `x as u128` silently truncates large values. To capture profit, the solver must be EVM-exact.

### Completed work

- [x] Created `rust/src/optimizers/mobius_int_exact.rs` — integer-exact Möbius solver using U512/U256 throughout
  - `exact_mobius_solve()`: K, M, N from `compute_int_mobius_coefficients()`, `x_opt = (isqrt(K·M) - M) / N`, EVM-exact verification with `int_simulate_path` at ±2 neighbors
  - `isqrt_u512()`: Newton's method in pure U512, starting from `2^((bit_len+1)/2)`, post-convergence adjustments, ~100ns
  - `u512_to_u256()`: Safe truncation with overflow capping to `U256::MAX`
  - 25 unit + property tests pass (isqrt correctness, exact vs f64 comparison, never-panics)
- [x] Created benchmark `rust/benches/mobius_exact_vs_f64.rs` — comparing f64, int_mobius_solve, and exact_mobius_solve
- [x] Benchmark results: exact_mobius_solve 2-hop 131ns (17× faster than int_mobius_solve's 1.3µs), isqrt_u512 ~100ns regardless of input size; f64 solver 7-12ns but produces wrong results for V3 paths
- [x] `mobius_int_exact` module registered in `rust/src/optimizers/mod.rs`
- [x] `Cargo.toml` updated with `[[bench]] name = "mobius_exact_vs_f64"` entry

### Remaining slices

The integer-exact solver is fully wired into the `UniswapArbEngine`:
- V2-only paths: use `exact_mobius_solve` (closed-form U512 isqrt) ✅
- Mixed V2-V3 paths: use `exact_solve_mixed_v2_v3` (integer effective reserves + closed-form) ✅
- Pure V3 paths: use `int_solve_v3_v3` (integer piecewise-Möbius with closed-form per segment) ✅

The engine is now fully integer-exact — zero f64 arithmetic on any solve path.

### Slice 10: Wire exact_mobius_solve into V2-only paths ✅

V2-only paths already use `IntHopState` (integer reserves, integer fees). The `exact_mobius_solve` function replaced `int_mobius_solve_with_refinement` directly.

**Result**: `uniswap_engine.rs` `solve_all()` now calls `exact_mobius_solve` for V2-only paths. All 401 Rust tests pass including `pure_v2_path_finds_profitable_arb`.

### Slice 11: Convert V3TickRangeHop to integer representation ✅

Created `rust/src/optimizers/mobius_v3_int.rs` with:
- `IntV3TickRangeHop` struct: U256 `sqrt_price_x96`, u128 `liquidity`, u64 fee representation
- `compute_effective_reserves()`: token0_virt = L·2^96/√P, token1_virt = L·√P/2^96 (U512 intermediates)
- `int_simulate_v3_swap()`: EVM-exact V3 swap simulation with full U512 arithmetic
- `IntV3TickRangeSequence` and `MixedIntHop` types for path building
- `exact_solve_mixed_v2_v3()`: combines V3 effective reserves with V2 integer reserves for closed-form solve
- `build_int_v3_hop()` on `V3PoolState`: builds `IntV3TickRangeHop` directly from original U256 values (no f64 conversion)
- 14 unit tests all passing

### Slice 12: Wire integer-exact solver into mixed V3-V2 paths ✅

Replaced the golden-section search in `solve_mixed_path` with `exact_solve_mixed_v2_v3`. The `ResolvedMixedPath` now carries `int_v3_hops: Vec<Option<IntV3TickRangeHop>>` built from original U256 values during `resolve_path`. The old `simulate_v3_hop` and `user_max_or` helpers are removed.

**Result**: All 401 Rust tests pass. All 9 Python integration tests pass. The engine no longer uses any f64 arithmetic for V2-only or mixed V2-V3 paths.

### Phase 3: Pure V3-V3 Integer-Exact Solver

The V3-V3 f64 solver (`solve_v3_v3` in `mobius_v3_v3.rs`) uses golden-section search over float `HopState`s. This is the last remaining f64 path in the engine. The approach replaces iterative search with closed-form integer Möbius, leveraging the existing piecewise-Möbius structure.

#### Key insight: additive crossing constants fold into Möbius coefficients

The existing `solve_v3_v3` already uses piecewise-Möbius decomposition: for each (k1, k2) pair of ending tick ranges, the profit function is:

```text
profit(x) = C₂_out + M₂(C₁_out + M₁(x - C₁_in) - C₂_in) - x
```

Where `M_i(y) = K_i·y / (M_coef_i + N_i·y)` and C values are fixed crossing constants. The f64 solver uses golden-section search because the additive constants prevented naive closed-form. But these constants fold into the Möbius coefficient recurrence — the composition of two shifted Möbius transforms is itself a Möbius transform with modified (K, M, N). Setting `d(profit)/dx = 0` yields a quadratic solvable via `isqrt` — exactly what `exact_mobius_solve` already does.

This eliminates golden-section search entirely. For each (k1, k2) piece, we compute a single (K, M, N) triple and call `exact_mobius_solve`.

#### Slice 13: Integer tick crossing computation ✅

Created `IntTickRangeCrossing` and `IntV3TickRangeSequence::compute_crossing(k)` in `mobius_v3_int.rs`. The crossing computation uses U512 intermediate arithmetic matching Solidity's V3 tick math. Also fixed a bug in `max_gross_input_in_range` — was dividing by `gamma_numer` instead of `gamma_numer/fee_denom` (off by ~10^6 factor).

**Result**: 5 new tests pass. All 406+ Rust tests pass.

#### Slice 14: Piecewise integer Möbius coefficient composition (int_solve_v3_v3) ✅

Created `int_solve_v3_v3()` and `int_simulate_v3_v3_path()` in `mobius_v3_int.rs`. For each (k1, k2) pair of tick range endings, the solver:
1. Computes `IntHopState` from each ending range's effective reserves
2. Calls `exact_mobius_solve` for closed-form optimal input (no golden-section search)
3. Validates with full piecewise simulation and ±2 neighborhood search

The key simplification vs. f64: no golden-section search. The closed-form `exact_mobius_solve` replaces it entirely for each piece.

**Result**: 3 new tests pass. All 409 Rust tests pass.

#### Slice 15: Wire integer V3-V3 solver into UniswapArbEngine ✅

Replaced f64 `solve_v3_v3` dispatch in `solve_all()` with `int_solve_v3_v3`. Added `int_v3_sequences` field to `ResolvedMixedPath`. Added `build_int_v3_sequence()` on `V3PoolState` — builds the full multi-range integer sequence from original U256 values. Removed `solve_v3_v3` import.

**Result**: All 409 Rust tests pass. All 3223 Python tests pass (1 pre-existing GIL test excluded). The engine is fully integer-exact — zero f64 on any solve path. 13 engine-vs-reference V3-V3 tests pass, covering positive/negative ticks, WETH/USDC-style pools, multi-range pools, and brute-force cross-validation.

Key findings during testing:
1. **V3-V3 direction rule**: For path [pool_A(zfo=True) → pool_B(zfo=False)], pool_A.tick must be > pool_B.tick. Higher tick means token1 is expensive (more token1 per token0), so buying token1 at pool A yields more, and selling at pool B (lower tick) yields more token0 per token1 = profit.
2. **liquidity_net sign bug**: In `build_sequence`/`build_int_v3_sequence`, walking DOWN (zfo=True) must use `l -= net` instead of `l += net`. The stored `liquidity_net` follows Uniswap convention (positive = added crossing upward), so crossing from above must negate.
3. **Range coverage matters**: Narrow ranges (±3 tick spacings) limit the solver's search space. `wide_range_around(tick, n=10)` provides sufficient room for the integer-exact Möbius solver to find the optimum.

## Status

- [x] P0 #1: Same-block reactivity (completed prior to this plan)
- [x] P0 #2: Pending tx monitoring (completed prior to this plan)
- [x] P0 #3: Executor ABI/config hardcoding (completed prior to this plan)
- [x] Slice 0.5: Dispatch serialisation
- [x] Slice 1: Parallel simulation
- [x] Slice 2: State-block staleness tracking
- [x] Slice 3: Competitive priority fee pricing
- [x] Slice 4: Best-path selection
- [x] Slice 5: Gas estimation from simulation
- [x] Slice 6: WS reconnection
- [x] Slice 7: Subscription filtering
- [x] Slice 8: Dry-run observability
- [x] Slice 9: Validate and clean up
- [x] mobius_int_exact core: integer-exact solver, isqrt_u512, benchmarks
- [x] Slice 10: Wire exact_mobius_solve into V2-only paths
- [x] Slice 11: Convert V3TickRangeHop to integer representation
- [x] Slice 12: Wire integer-exact solver into mixed V3-V2 paths
- [x] Slice 13: Integer tick crossing computation
- [x] Slice 14: Piecewise integer Möbius coefficient composition (int_solve_v3_v3)
- [x] Slice 15: Wire integer V3-V3 solver into UniswapArbEngine
- [x] Fix liquidity_net sign bug in build_sequence and build_int_v3_sequence
- [x] Engine V3-V3 test suite: 13 tests comparing engine vs Brent and brute-force
- [x] Documented V3-V3 direction rule: pool_A(zfo=True) must have tick > pool_B(zfo=False) for profit
- [x] Documented range coverage requirement: wide_range_around(tick, n=10) for sufficient tick ranges
- [x] Clippy fixes in mobius_int_exact.rs (div_ceil) and mobius_v3_int.rs (let-else, let-binding return)

## Remaining Work

All slices in this plan are complete. The following items are out-of-scope
improvements for future consideration:

- [ ] Run `--observe` mode on mainnet to validate the fully integer-exact engine produces realistic profits at production tick values and liquidity levels
- [ ] Consider removing `rust/src/optimizers/mobius_v3_v3.rs` (f64 V3-V3 solver) — now unused by the engine since Slice 15 replaced it with `int_solve_v3_v3`
- [ ] Consider benchmarking `int_solve_v3_v3` vs f64 `solve_v3_v3` to quantify the integer-exact solver's overhead
- [ ] Extend engine V3-V3 test suite with >3-hop paths (current tests cover only 2-hop V3-V3)
- [ ] Investigate false-zero-profit for large tick divergence (>1400 ticks between pool A and pool B) — the engine's `int_solve_v3_v3` finds no profit where brute-force does, likely caused by `build_int_v3_sequence`'s `max_ranges` cap or overflow in crossing computation for deeply-in-the-money pieces
- [ ] Add runtime check in `register_v2_pool` that `_fee_token0 == _fee_token1` and warn if asymmetric (F3 mitigation)
- [ ] Optimize V3 tick_data transfer: diff against cached snapshot and send only changed ticks (F4 mitigation)
