# Contracts

On-chain executor contracts for MEV arbitrage.

## Directory Structure

```
contracts/
├── README.md                            ← you are here
├── tstore_executor.vy                   ← Vyper 0.4.x source (V2+V3+V4)
├── tstore_executor_bytecode.txt         ← Init bytecode (includes constructor; for deployment)
├── tstore_executor_runtime_bytecode.txt ← Deployed bytecode (runtime + immutables; for code injection)
├── tstore_executor_abi.json             ← ABI (for web3.py contract objects)
└── tests/                               ← Ape + Foundry test suite (see tests/README.md)
    ├── README.md                        ← Testing documentation
    ├── ape-config.yaml                  ← Ape project config
    ├── contracts/                       ← Fake contracts + symlinked executor
    │   ├── tstore_executor.vy           ← symlink → ../../tstore_executor.vy
    │   ├── fake_erc20.vy                ← Mock ERC-20
    │   ├── fake_weth.vy                 ← Mock WETH with deposit/withdraw
    │   ├── fake_uniswap_v2_pair.vy      ← Mock V2 pair + uniswapV2Call callback
    │   ├── fake_uniswap_v3_pool.vy      ← Mock V3 pool + callback
    │   ├── fake_uniswap_v4_pool_manager.vy ← Mock V4 PoolManager
    │   └── utility_functions.vy          ← ERC-55 checksum helper
    └── tests/                           ← Test files
        ├── test_tstore_executor_v4v4.py ← V4-V4 settlement tests
        ├── test_tstore_executor_v4v3.py ← V4-V3/V3-V4 hybrid tests
        ├── test_tstore_executor_v4v2.py ← V4-V2/V2-V4 hybrid tests
        ├── test_tstore_executor_v2v2.py ← V2-V2 path tests
        ├── test_tstore_executor_v3v3.py ← V3-V3 path tests
        ├── test_tstore_executor_v2v3.py ← V2-V3 and V3-V2 path tests
        ├── test_tstore_executor_edge_cases.py ← Callback variants, settlement branches, regressions
        └── test_tstore_executor_three_hop.py ← Three-hop path tests (V4-only and hybrid)
```

### How the Bytecode Files Relate

- **`tstore_executor_bytecode.txt`** — the output of `vyper -f bytecode`. Contains the init code (constructor) that, when executed by the EVM, returns the runtime code with immutables appended. Use this for on-chain deployment via `cast send --create`.
- **`tstore_executor_runtime_bytecode.txt`** — the deployed runtime code **with immutables already appended** (OWNER_ADDR and WETH_ADDR as 32-byte padded values in declaration order). Use this for `eth_simulateV1` code injection (`INJECT_EXECUTOR_CODE=1`) — injected code must include immutables because Vyper loads them from the code section via `CODECOPY`.

If you recompile the contract (e.g. after changing the Vyper source), you must regenerate **both** files and re-append the immutables to the runtime bytecode. See "Runtime Bytecode (for Code Injection)" below for the procedure.

### Cross-References

- **Bot script**: `examples/eth_backrun_v2_v3_v4_rust.py` — loads the runtime bytecode via `_load_executor_runtime_bytecode()` when `INJECT_EXECUTOR_CODE=1`
- **Executor config**: `EXECUTOR_ADDRESS`, `EXECUTOR_OWNER`, `INJECT_EXECUTOR_CODE` in the bot script and `examples/mainnet.env`
- **Architecture**: Plan 080 (`plans/completed/080-rust-bot-poc-path-to-profit.md`) documents the swap encoding, code injection mechanism, and V3 sign convention

## Contract Index

| Contract | Location | Vyper | Pool Types | Callbacks | Style |
|----------|----------|-------|-------------|-----------|-------|
| `tstore_executor.vy` | `contracts/` | 0.4.x | V2 + V3 + V4 | `uniswapV2Call`, `hook`, `pancakeCall`, `uniswapV3SwapCallback`, `pancakeV3SwapCallback`, `unlockCallback` | Generic payload queue |
| `v4_v2_executor.vy` | `examples/uniswap_v4_v2_executor/contracts/` | 0.4.1 | V4 + V2 | `unlockCallback`, `__default__` | Fixed-structure (V4↔V2) |
| `v4_v3_executor.vy` | `examples/uniswap_v4_v2_executor/contracts/` | 0.4.1 | V4 + V3 | `unlockCallback`, `uniswapV3SwapCallback` | Fixed-structure (V4↔V3) |
| `v4_v3_executor_multi.vy` | `examples/uniswap_v4_v3_executor/contracts/` | 0.4.1 | V4 + V3 | `unlockCallback`, `uniswapV3SwapCallback` | Command-bytecode VM |
| `v4_v4_executor.vy` | `examples/uniswap_v4_v4_executor/contracts/` | 0.4.1 | V4 + V4 | `unlockCallback` | Fixed-structure (2-pool) |
| `v4_v4_executor_dev.vy` | `examples/uniswap_v4_v4_executor/contracts/` | 0.4.1 | V4 (multi-hop) | `unlockCallback` | Chained pool keys |
| `tstore_executor_generic.vy` | `examples/tstore_executor/` | 0.3.10 | V3 only | `uniswapV3SwapCallback` | Generic payload queue |
| `tstore_executor.vy` | `examples/tstore_executor/` | 0.3.10 | V2 + V3 (packed) | `uniswapV3SwapCallback`, `__default__` | Packed payload encoding |
| `ethereum_executor_testing.vy` | `examples/basic_executor/` | 0.3.10 | V3 only | `uniswapV3SwapCallback` | Generic payload queue |

### Previously Deployed

| Address | Contract | Notes |
|---------|----------|-------|
| `0x543C7eF4...` | `tstore_executor_generic.vy` | V3-only, V2 callbacks silently dropped by `__default__()`. **Superseded** by `contracts/tstore_executor.vy` which has full V2/V3 callback support. |
| `0x6dF77532...` | `ethereum_executor_testing.vy` | V3-only, no flash borrows |

### Critical: V3 vs V4 amountSpecified Sign Convention

V3 and V4 use **opposite** sign conventions for `amountSpecified`. Using the wrong sign causes V3 to interpret the call as exact-output mode, computing an enormous input requirement and failing with "IIA" (Insufficient Input Amount). For the full table and V4 encoding details, see [V3 vs V4 Sign Conventions](#v3-vs-v4-sign-conventions) in the tstore_executor section.

---

## Contract Testing

The `contracts/tests/` directory contains an Ape + Foundry test suite with fake contracts that simulate V2/V3/V4 pool behavior. These tests verify the executor's settlement plumbing (delta ledger accounting, `sync()`/`settle()` ordering, callback routing) without requiring mainnet state. 27 tests across 8 test files covering V2/V3-only paths, V4-hybrid paths, callback variant selectors, settlement branches, three-hop paths, and encoding regressions.

**Quick start** (from `contracts/tests/`):

```bash
# Run all contract tests
uv run --with eth-ape --with ape-vyper --with ape-foundry ape test -v -n0

# Run a specific test
uv run --with eth-ape --with ape-vyper --with ape-foundry ape test tests/test_tstore_executor_v4v4.py -v -n0
```

Always use `-n0` (single worker) — Foundry's local EVM is a single process and parallel workers cause connection flakes.

See [`contracts/tests/README.md`](tests/README.md) for full documentation of the testing fakes, test patterns, and troubleshooting.

---

## tstore_executor.vy — V2/V3/V4 Generic Payload Executor

**Vyper version**: 0.4.x
**EVM version**: Cancun (uses TSTORE for transient storage)

Generic payload executor with Uniswap V2/V3/V4 callback support and V4 auto-settlement. The contract stores payloads in transient storage and delivers them sequentially. Callbacks resume queue delivery, enabling nested callback chains for multi-hop paths. V4 swaps execute via `extcall` in `unlockCallback` and auto-settle based on actual on-chain `BalanceDelta` return values.

### Compile

```bash
# Bytecode (for deployment)
uv run vyper -f bytecode contracts/tstore_executor.vy

# Runtime bytecode
uv run vyper -f bytecode_runtime contracts/tstore_executor.vy

# ABI
uv run vyper -f abi contracts/tstore_executor.vy

# All at once
uv run vyper -f abi,bytecode,bytecode_runtime contracts/tstore_executor.vy
```

### Deploy

```bash
# Deploy with cast, funding with 1 WETH
cast send --rpc-url http://node:8545 \
  --private-key $PRIVATE_KEY \
  --create \
  $(cat contracts/tstore_executor_bytecode.txt) \
  0xC02aaA39b223FE8D0A0e5C4f27eAD9083C756Cc2  # WETH address (constructor arg)
```

### Runtime Bytecode (for Code Injection)

The file `contracts/tstore_executor_runtime_bytecode.txt` contains the deployed runtime bytecode with immutables already baked in (OWNER_ADDR and WETH_ADDR). This is used by the bot's code injection feature (`INJECT_EXECUTOR_CODE=1`) to test the contract via `eth_simulateV1` without deploying on mainnet first.

**How it was generated:**
1. Compile with `uv run vyper -f bytecode_runtime contracts/tstore_executor.vy`
2. Append two 32-byte padded immutable values to the runtime bytecode:
   - `OWNER_ADDR` (12 zero bytes + `0x9C56a29c7231974c269E24F9FB3c29203039089E`, padded to 32 bytes)
   - `WETH_ADDR` (12 zero bytes + `0xC02aaA39b223FE8D0A0e5C4f27eAD9083C756Cc2`, padded to 32 bytes)
3. Vyper's runtime code uses `CODECOPY` with offset = `len(runtime_code)` to load immutables, so appending them at the end is equivalent to what the constructor does during deployment

**Vyper immutables are embedded in the runtime code**, not in storage. The bytecode already contains the throwaway OWNER_ADDR (`0x9C56a29c7231974c269E24F9FB3c29203039089E` — a randomly generated key, not a real deployment) and WETH_ADDR (`0xC02aaA39b223FE8D0A0e5C4f27eAD9083C756Cc2`). No storage slot overrides are needed for the executor itself. Override `EXECUTOR_OWNER_ADDRESS` at runtime with the real owner key.

### Callback Support

| Callback | Source | Selector |
|----------|--------|----------|
| `uniswapV2Call` | Uniswap V2, SushiSwap V2 | `0x10d1e85c` |
| `hook` | Velodrome, Aerodrome | `0x9a7bff79` |
| `pancakeCall` | PancakeSwap V2 | `0x84800812` |
| `uniswapV3SwapCallback` | Uniswap V3, SushiSwap V3 | `0xfa461e33` |
| `pancakeV3SwapCallback` | PancakeSwap V3 | `0x23a69e75` |
| `unlockCallback` | Uniswap V4 PoolManager | `0x91dd7346` |

### Contract Interface

#### `execute_payloads(payloads, v4_swaps, bribe_bips, skip_profit_check)`

Entry point for all arbitrage paths. Owner-only.

| Parameter | Type | Description |
|-----------|------|-------------|
| `payloads` | `DynArray[Payload, 16]` | Ordered list of generic payloads (V2/V3 swaps, transfers, V4 unlock, etc.) |
| `v4_swaps` | `DynArray[V4SwapPayload, 4]` | V4 swap parameters — executed via `extcall` in `unlockCallback`. `[]` for V2/V3-only paths |
| `bribe_bips` | `uint256` | Coinbase bribe as basis points of profit. Default `0` |
| `skip_profit_check` | `bool` | Skip the `combined_after >= combined_before` assertion. Default `False`. Use `True` for testing |

#### Struct: `Payload`

| Field | Type | Description |
|-------|------|-------------|
| `target` | `address` | Contract to call |
| `calldata` | `Bytes[832]` | Encoded function call data |
| `value` | `uint256` | ETH value to send with the call. Used for V4 `settle()` with `msg.value`. `0` for most payloads |
| `will_callback` | `bool` | If `True`, register `target` in `t_allowed_callback_addresses` before calling. Required for pools that will call back into the executor |

#### Struct: `V4SwapPayload`

| Field | Type | Description |
|-------|------|-------------|
| `key` | `PoolKey` | V4 pool identifier (currency0, currency1, fee, tick_spacing, hooks) |
| `params` | `SwapParams` | Swap parameters (zero_for_one, amount_specified, sqrt_price_limit_x96) |
| `dynamic_amount` | `bool` | If `True` AND `amount_specified == 0`, derive `amount_specified` from `t_v4_deltas` ledger. Used for the second swap in V4-V4 paths |

#### Struct: `PoolKey`

| Field | Type | Notes |
|-------|------|-------|
| `currency0` | `address` | Lower-addressed token. `address(0)` for native ETH |
| `currency1` | `address` | Higher-addressed token. `address(0)` for native ETH |
| `fee` | `uint24` | Pool fee tier |
| `tick_spacing` | `int24` | Tick spacing |
| `hooks` | `address` | Hook contract. `address(0)` for no hooks |

#### Struct: `SwapParams`

| Field | Type | Notes |
|-------|------|-------|
| `zero_for_one` | `bool` | `True` = input is currency0, output is currency1 |
| `amount_specified` | `int256` | V4 sign convention: **negative** for exact-input, **positive** for exact-output (opposite to V3!) |
| `sqrt_price_limit_x96` | `uint160` | Price limit. Min+1 for zfo=True, Max-1 for zfo=False |

#### ABI Encoding

The `V4SwapPayload` ABI type is `((address,address,uint24,int24,address),(bool,int256,uint160),bool)` — a nested tuple of `(PoolKey, SwapParams, dynamic_amount)`. In Python:

```python
V4SwapParam = tuple[str, str, int, int, str, bool, int, int, bool]  # 9-tuple

def _v4_swaps_to_abi(v4_swaps):
    return [
        (
            (c0, c1, fee, tick_spacing, hooks),       # PoolKey
            (zfo, amount_specified, sqrt_price_limit), # SwapParams
            dynamic_amount,                            # bool
        )
        for c0, c1, fee, tick_spacing, hooks, zfo, amount_specified, sqrt_price_limit, dynamic_amount
        in v4_swaps
    ]
```

### Other Functions

| Function | Description |
|----------|-------------|
| `withdraw(amount, destination)` | Withdraw ETH (unwrapping WETH if needed) to `destination`. Owner-only |
| `__default__()` | Accepts plain ETH transfers (`msg.data` empty); reverts on unknown function calls |

### V4 Auto-Settlement: The 4-Phase `unlockCallback`

The `unlockCallback` is the heart of V4 settlement. It runs inside `PoolManager.unlock()` and has four phases:

| Phase | Purpose | When it runs | Key logic |
|-------|---------|--------------|-----------|
| **Phase 0** | Pre-settle | Before V4 swaps | For V3→V4/V2→V4: calls `settle()` on PM to credit forward ERC-20 tokens that were transferred+synced before `unlock()`. Credits the settled amount to `t_v4_deltas`. Only runs when payloads were already delivered before the unlock (`t_queued_payload_index > 0`). Skips `dynamic_amount` swaps and native/WETH inputs. Skips duplicate settlements when the same input currency appears across multiple V4 swaps (checks if delta already credited) |
| **Phase 1** | V4 swaps | Core | Executes V4 swaps via `extcall`, reads `BalanceDelta` return values, tallies ALL currency deltas in `t_v4_deltas`. Handles `dynamic_amount`: if `amount_specified == 0` and `dynamic_amount == True`, derives the amount from the delta ledger instead of using a pre-computed value |
| **Phase 2** | Queued payloads | After V4 swaps | Delivers remaining queued payloads (take, transfer, V2/V3 swaps). If any payloads were delivered, zeros intermediate ERC-20 deltas so Phase 3 doesn't double-take/settle them. Native ETH and WETH deltas are never zeroed |
| **Phase 3** | Auto-settle | After payloads | Settles all nonzero `t_v4_deltas` entries: native ETH (unwrap if needed), WETH (sync+transfer+settle), and intermediate ERC-20s (sync+transfer+settle). Uses the `_v4_settle_currency` helper, which zeros each delta after settling to prevent double-settlement when the same ERC-20 appears in multiple pool keys |

**For V4-V4 paths**: Only Phase 1 and Phase 3 run (no payloads before unlock, no queued payloads after).

**For V4→V3 paths**: Phase 1 (V4 swap) → Phase 2 (take forward + transfer to V3 + V3 swap with callback) → Phase 3 (settle WETH/ETH).

**For V4→V2 paths**: Phase 1 (V4 swap) → Phase 2 (take forward + transfer to V2 + V2 flash swap with callback) → Phase 3 (settle WETH/ETH). V2 callback resumes payload delivery but does NOT auto-pay WETH — the WETH transfer to V2 pair must be an explicit payload.

**For V3→V4 paths**: V3 swap runs first (outside unlock). Then: sync+transfer to PM → `unlock()` → Phase 0 (settle forward ERC-20) → Phase 1 (V4 swap consumes forward) → Phase 3 (settle remaining).

**For V2→V4 paths**: V2 flash swap runs first (outside unlock). Then: sync+transfer to PM → `unlock()` → Phase 0 (settle forward ERC-20) → Phase 1 (V4 swap consumes forward) → Phase 3 (settle remaining). After unlock returns, a WETH transfer-to-V2 payload pays the V2 pair.

### Key Design Decisions

1. **V2 flash borrows work**: All three V2 callback types (`uniswapV2Call`, `hook`, `pancakeCall`) directly resume payload delivery via `_deliver_remaining_payloads()` — no intermediate wrapper.
2. **V3 auto-pay**: When a V3 pool is owed WETH, the callback auto-transfers it. Python encoders must NOT include separate WETH transfer payloads for V3 pools where auto-pay fires. The auto-pay computes `owed_token`/`owed_amount` first, then performs a single transfer if the owed token is WETH.
3. **V2 callbacks have NO auto-pay**: Unlike V3, V2 callbacks only resume payload delivery. WETH payment to V2 pairs must be an explicit payload in the queue (typically after V4 unlock produces WETH via `take`).
4. **Strict `__default__`**: Reverts on unknown function calls (swallows nothing).
5. **`will_callback` registration**: Before calling a pool with `will_callback=True`, the target address is registered in `t_allowed_callback_addresses`. Callback handlers assert `msg.sender` is registered. This prevents unauthorized callbacks.
6. **Transient storage**: All queue state (payloads, V4 swaps, delta ledger, callback addresses) uses TLOAD/TSTORE — automatically cleared between transactions.
7. **All-currency delta ledger**: `t_v4_deltas` tracks ALL currency deltas (not just ETH/WETH) across V4 swaps. This ensures intermediate ERC-20 tokens (e.g., USDC in a WETH→USDC→ETH path) are properly settled. Each delta is zeroed by `_v4_settle_currency` after settling, preventing double-settlement when the same ERC-20 currency appears in multiple pool keys.
8. **Dynamic amounts**: For V4-V4 paths, the second swap sets `dynamic_amount=True` and `amount_specified=0`. The contract reads the intermediate delta from `t_v4_deltas` and uses it as the `amountSpecified` for the second swap. This guarantees intermediate deltas cancel exactly.
9. **Sync before transfer**: When the executor transfers ERC-20 to PM for settlement, `sync()` MUST be called BEFORE the transfer, then `settle()` after. The sequence: sync (records PM's current balance) → transfer (adds tokens to PM) → settle (computes delta = new_balance - old_balance). Calling sync AFTER the transfer causes settle to see zero delta.
10. **Combined balance check**: After all payloads, the executor asserts `WETH_balance + ETH_balance` did not decrease. This correctly handles paths that unwrap WETH to ETH for V4 native settlement.
11. **int128 overflow guard**: V4's `BalanceDelta` uses `int128` per component. If `amountSpecified` exceeds ±2^127, V4 reverts with `SafeCastOverflow`. Python encoders guard against this with `fits_int128()` — skipping paths that would overflow.
12. **V4→V2 `amount_out` = `weth_out`**: V2's `swap(amount0Out, amount1Out, ...)` specifies what V2 SENDS to the recipient. For a USDC→WETH@V2 swap, `amount_out` must be the WETH output, NOT the USDC input. Using the wrong amount causes `INSUFFICIENT_LIQUIDITY`.
13. **`NATIVE_ADDRESS` constant**: `constant(address) = empty(address)` — used throughout the contract to identify native ETH, replacing inline `native: address = empty(address)` locals. Named constant is clearer and avoids stack variable overhead.
14. **`_decode_swap_delta` unified helper**: Replaces the former `_decode_swap_delta_amount0`/`_decode_swap_delta_amount1` pair with a single parameterized function `_decode_swap_delta(swap_delta, byte_offset)`, eliminating code duplication.

### V3 vs V4 Sign Conventions

V3 and V4 use **opposite** sign conventions for `amountSpecified`. This applies to both the V4 `SwapParams.amount_specified` struct field and the V3 swap calldata encoding.

| | Exact INPUT | Exact OUTPUT |
|---|---|---|
| **V3** | `amountSpecified > 0` | `amountSpecified < 0` |
| **V4** | `amountSpecified < 0` | `amountSpecified > 0` |

For arbitrage (always exact-input mode): V3 encoding uses **positive** values, V4 encoding uses **negative** values.

### Supported Path Types

For V4 paths, `payloads` contains only the `unlock()` call (and optional pre-unlock transfers/sync). V4 swap parameters go in `v4_swaps`. For V2/V3-only paths, `v4_swaps = []` and all operations go in `payloads`.

| Path | Payloads | `v4_swaps` | Flow |
|------|----------|------------|------|
| V2→V2 | 4 | `[]` | V2 flash borrow + direct V2 swap + WETH repayment |
| V3→V3 | 2 | `[]` | Nested V3 callbacks; double auto-pay for WETH debts |
| V2→V3 | 4 | `[]` | V2 flash borrow + V3 nested callback + WETH repayment |
| V3→V2 (zfo=True) | 2 | `[]` | V3 callback auto-pays WETH; V2 direct swap |
| V3→V2 (zfo=False) | 3 | `[]` | V3 callback auto-pays WETH; explicit WETH transfer to V2 |
| V4→V4 | 1 | 2 swaps | `unlock()` + two V4 swaps via `extcall` (second uses `dynamic_amount=True`). Auto-settled. |
| V4→V3 | 4–6 | 1 swap | `unlock()` → Phase 1: V4 swap → Phase 2: take forward + transfer + V3 swap → Phase 3: settle ETH/WETH |
| V4→V2 | 4–6 | 1 swap | `unlock()` → Phase 1: V4 swap → Phase 2: take forward + transfer + V2 flash swap → Phase 3: settle ETH/WETH. WETH to V2 is explicit payload. |
| V3→V4 | 4–7 | 1 swap | V3 swap → sync + transfer to PM → `unlock()` → Phase 0: settle forward → Phase 1: V4 swap → Phase 3: settle ETH/WETH |
| V2→V4 | 4–7 | 1 swap | V2 flash → sync + transfer to PM → `unlock()` → Phase 0: settle forward → Phase 1: V4 swap → Phase 3: settle ETH/WETH. Post-unlock: WETH transfer to V2 pair. |
| V4→V4→V4 | 1 | 3 swaps | Three V4 swaps via `extcall`. Swaps 2–3 use `dynamic_amount=True`. Two intermediate currencies cancel exactly via delta ledger. |
| V4→V4→V3 | 4 | 2 swaps | Two V4 swaps + V3 payload in Phase 2. Phase 2 zeroes both intermediate ERC-20 deltas. |
| V4→V3→V2 | 6 | 1 swap | V4 swap → take + V3 swap (auto-pay WETH) + V2 direct swap → settle WETH |
| V4→V2→V3 | 5 | 1 swap | V4 swap → take + V2 flash swap → V3 swap (auto-pay) → USDC to V2 → settle WETH |
| V2→V3→V4 | 7 | 1 swap | V2 flash → V3 swap → sync + transfer to PM → unlock → V4 swap → WETH to V2 |

---

## V4 Executors

Contracts designed for Uniswap V4 (PoolManager-based) arbitrage. All use the V4 `unlock()`/`unlockCallback()` pattern for token settlement.

### v4_v2_executor.vy — V4↔V2 Fixed-Structure Executor

**Location**: `examples/uniswap_v4_v2_executor/contracts/`
**Vyper**: 0.4.1 | **EVM**: Cancun

Fixed-structure executor for a single V4 pool ↔ single V2 pool arbitrage. The entry point `execute(v2_payload, v4_payload)` stores both payloads in transient storage, then calls `PoolManager.unlock()`. The callback handles all token movement based on which token (ETH, WETH, or ERC-20) is being settled vs. taken.

**Key design**: The callback inspects the V4 swap deltas to identify the `settle_currency` (owed to V4) and `take_currency` (owed by V4). Four cases cover all ETH/WETH/ERC-20 combinations:
- `settle_currency == NATIVE_ADDRESS` → V4→V2: take forward token from V4 to V2, swap V2 for WETH, settle ETH
- `take_currency == NATIVE_ADDRESS` → V2→V4: take ETH from V4, wrap, transfer WETH to V2, swap V2 for forward token, settle
- `settle_currency == WETH_ADDRESS` → V4→V2: take forward token from V4 to V2, swap V2 for WETH, sync+settle WETH
- `take_currency == WETH_ADDRESS` → V2→V4: take WETH from V4, transfer to V2, swap for forward token, settle

**V2 swap uses `data=b""`** (direct swap, no callback). The contract does NOT implement `uniswapV2Call` — it relies on WETH balance in the contract to prefund V2 swaps or uses `__default__()` to accept V2 callback as a no-op.

**Tests**: `examples/uniswap_v4_v2_executor/tests/test_v4_v2_executor.py` — WBTC-WETH mainnet fork tests.

### v4_v3_executor.vy — V4↔V3 Fixed-Structure Executor

**Location**: `examples/uniswap_v4_v2_executor/contracts/`
**Vyper**: 0.4.1 | **EVM**: Cancun

Fixed-structure executor for a single V4 pool ↔ single V3 pool arbitrage. Entry point `execute(v3_payload, v4_payload)`. V3 uses `uniswapV3SwapCallback` for payment; V4 uses `unlockCallback` for settlement.

**Key design**: The V3 callback pays the V3 pool debt. If the debt is in WETH, the executor wraps ETH and transfers directly. If the debt is an ERC-20 (forward token), the callback takes it from the V4 PoolManager (`take()`). The V4 unlock callback drives the swap and handles settlement.

Four cases parallel the V2 executor:
- `settle_currency == NATIVE_ADDRESS` → V4→V3: V3 exact-input swap (forward→WETH), settle ETH
- `take_currency == NATIVE_ADDRESS` → V3→V4: V3 exact-output swap (WETH→forward), settle ETH
- `settle_currency == WETH_ADDRESS` → V4→V3: V3 exact-input swap, sync+settle WETH
- `take_currency == WETH_ADDRESS` → V3→V4: V3 exact-input swap, settle forward token

**Tests**: `examples/uniswap_v4_v2_executor/tests/test_v4_v3_executor.py` — WBTC-WETH mainnet fork tests.

### v4_v3_executor_multi.vy — V4↔V3 Command-Bytecode VM Executor

**Location**: `examples/uniswap_v4_v3_executor/contracts/`
**Vyper**: 0.4.1 | **EVM**: Cancun

Advanced executor using a command-bytecode virtual machine pattern. Instead of fixed payloads, the caller provides an `addresses[]` array and a `commands` bytecode string. Each command byte selects an operation (V3 swap, V4 swap, V4 take, V4 sync, V4 settle, WETH transfer, etc.) with arguments encoded inline or referenced by index into the `addresses` array.

**Key design**: Commands are chained — remaining bytes after a V3 swap command are forwarded as `data` to the V3 callback, enabling multi-step execution within nested callbacks. The contract tracks `t_deltas[address][address]` (pool→token→balance) to resolve dynamic amounts at runtime. V4 swap deltas update the delta ledger; subsequent commands read from it for exact settlement amounts.

**Commands**:

| Byte | Command | Description |
|------|---------|-------------|
| `0x00` | `WETH_TRANSFER_DYNAMIC_AMOUNT` | Transfer WETH to a destination, amount from delta ledger |
| `0x01` | `V3_SWAP_DYNAMIC_AMOUNT` | V3 swap with amount from delta ledger (exact output preferred) |
| `0x02` | `V3_SWAP_SPECIFIED_AMOUNT` | V3 swap with explicit 32-byte amount |
| `0x03` | `V4_TAKE_DYNAMIC_AMOUNT` | V4 `take()` with amount from delta ledger |
| `0x04` | `V4_SWAP_DYNAMIC_AMOUNT` | V4 swap with amount from delta ledger |
| `0x05` | `V4_SWAP_SPECIFIED_AMOUNT` | V4 swap with explicit 32-byte amount |
| `0x06` | `V4_SYNC` | V4 `sync()` on a token |
| `0x07` | `V4_SETTLE_ERC` | V4 `settle()` for ERC-20 |
| `0x08` | `V4_SETTLE_NATIVE_DYNAMIC_AMOUNT` | V4 `settle()` for native currency, amount from delta |
| `0x09` | `V4_SETTLE_NATIVE_SPECIFIED_AMOUNT` | V4 `settle()` for native currency, explicit amount |
| `0xFF` | `SEPARATOR` | Command delimiter |

**Supporting contracts** (for testing):
- `fake_erc20.vy` — Mock ERC-20
- `fake_weth.vy` — Mock WETH with deposit/withdraw
- `fake_uniswap_v3_pool.vy` — Mock V3 pool with swap + callback
- `fake_uniswap_v4_pool_manager.vy` — Mock V4 PoolManager with unlock/swap/take/settle/sync
- `calldata_tester.vy` — Calldata verification helper
- `utility_functions.vy` — Shared test utilities

**Tests**: `examples/uniswap_v4_v3_executor/tests/test_v4_v3_executor_multi.py` — Comprehensive VM tests with mocked contracts.

### v4_v4_executor.vy — V4↔V4 Fixed-Structure (2-Pool)

**Location**: `examples/uniswap_v4_v4_executor/contracts/`
**Vyper**: 0.4.1 | **EVM**: Cancun

Fixed-structure executor for a single V4 pool ↔ single V4 pool arbitrage. Entry point `execute(v4_payload_a, v4_payload_b)`. Both swaps happen inside `unlockCallback`, then the net deltas determine settlement.

**Key design**: Both V4 swaps execute in the callback. The contract tallies `ether_delta` and `weth_delta` across both swaps. Positive deltas → `take()`. Negative deltas → `settle()` with automatic wrap/unwrap as needed.

**Tests**: `examples/uniswap_v4_v4_executor/tests/test_v4_v4_executor.py` — Deployed on Base (USDC-ETH pools).

### v4_v4_executor_dev.vy — V4 Multi-Hop Chained Executor

**Location**: `examples/uniswap_v4_v4_executor/contracts/`
**Vyper**: 0.4.1 | **EVM**: Cancun

Multi-hop V4 executor that chains N pools together. Entry point `execute(pool_keys[], initial_swap)`. The callback iterates over all pool keys, executing swaps in sequence. Each swap's output becomes the next swap's input (chained `amount_specified`).

**Key design**: `initial_swap` is the exact-input amount for the first pool. Each subsequent swap's `amount_specified` is derived from the previous swap's delta. The `zero_for_one` direction for each pool is determined by checking which currency matches the `currency_in` from the previous hop. After all swaps, the net WETH/ETH deltas are settled.

**Tests**: `examples/uniswap_v4_v4_executor/tests/test_v4_v4_executor.py` (shared with the 2-pool variant).

---

## Legacy Executors

The following contracts are maintained for reference but are superseded by newer versions.

### tstore_executor_generic.vy — V3-Only Payload Executor (Deployed)

**Location**: `examples/tstore_executor/`
**Vyper**: 0.3.10 | **EVM**: Cancun
**Deployed at**: `0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5`

V3-only payload executor. **No V2 callback support** — V2 callbacks hit `__default__()` which silently returns, causing flash borrow repayment failures. This is the root cause of V2-V2/V2-V3 simulation failures in Plan 080's bot. Superseded by `contracts/tstore_executor.vy` (0.4.x, full V2/V3/V4 callbacks, strict `__default__`).

### tstore_executor.vy (legacy) — V2+V3 Packed Payload Executor

**Location**: `examples/tstore_executor/`
**Vyper**: 0.3.10 | **EVM**: Cancun

Payload executor using close-packed binary encoding (`execute_packed_payloads`) and an address index table. Supports V2 and V3 swap commands plus ERC-20 transfers. Commands are 32 bytes each (1 byte command + 20 bytes address + 1 byte destination index + 10 bytes amount). V2 swaps use `data=b""` (direct swap only, no flash borrows). V3 pools use `will_callback` via the `t_allowed_callback_addresses` transient map.

### ethereum_executor_testing.vy — Basic V3 Executor

**Location**: `examples/basic_executor/`
**Vyper**: 0.3.10
**Deployed at**: `0x6dF77532...`

V3-only payload executor similar to `tstore_executor_generic.vy`. No V2 callbacks.

### tstore_executor_testing.vy

**Location**: `examples/tstore_executor/`
**Vyper**: 0.3.10

Testing variant of the packed payload executor.
