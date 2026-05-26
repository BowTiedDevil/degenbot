# Contracts

On-chain executor contracts for MEV arbitrage.

## Directory Structure

```
contracts/
├── README.md                            ← you are here
├── tstore_executor.vy                   ← Vyper 0.4.x source (V2+V3+V4)
├── tstore_executor_bytecode.txt         ← Init bytecode (includes constructor; for deployment)
├── tstore_executor_runtime_bytecode.txt ← Deployed bytecode (runtime + immutables; for code injection)
└── tstore_executor_abi.json            ← ABI (for web3.py contract objects)
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

### Critical: V3 amountSpecified Sign Convention

When building swap calldata for V3 pools, note that V3 and V4 use **opposite** sign conventions for `amountSpecified`:

| | exact INPUT | exact OUTPUT |
|---|---|---|
| **V3** | `amountSpecified > 0` | `amountSpecified < 0` |
| **V4** | `amountSpecified < 0` | `amountSpecified > 0` |

For arbitrage paths using the `tstore_executor.vy` (V2+V3 only), always use **positive** `amountSpecified` for exact-input V3 swaps. Using the wrong sign causes V3 to interpret the call as exact-output mode, computing an enormous input requirement and failing with "IIA" (Insufficient Input Amount).

---

## tstore_executor.vy — V2/V3/V4 Generic Payload Executor

**Vyper version**: 0.4.x
**EVM version**: Cancun (uses TSTORE for transient storage)

Generic payload executor with Uniswap V2/V3 callback support. The contract stores payloads in transient storage and delivers them sequentially. Callbacks resume queue delivery, enabling nested callback chains for multi-hop paths.

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
  0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2  # WETH address (constructor arg)
```

### Runtime Bytecode (for Code Injection)

The file `contracts/tstore_executor_runtime_bytecode.txt` contains the deployed runtime bytecode with immutables already baked in (OWNER_ADDR and WETH_ADDR). This is used by the bot's code injection feature (`INJECT_EXECUTOR_CODE=1`) to test the contract via `eth_simulateV1` without deploying on mainnet first.

**How it was generated:**
1. Compile with `uv run vyper -f bytecode_runtime contracts/tstore_executor.vy`
2. Append two 32-byte padded immutable values to the runtime bytecode:
   - `OWNER_ADDR` (12 zero bytes + `0x9C56a29c7231974c269E24F9FB3c29203039089E`, padded to 32 bytes)
   - `WETH_ADDR` (12 zero bytes + `0xC02aaA39b223FE8D0A0e5C4f27eAD9083C756Cc2`, padded to 32 bytes)
3. Vyper's runtime code uses `CODECOPY` with offset = `len(runtime_code)` to load immutables, so appending them at the end is equivalent to what the constructor does during deployment

**Vyper immutables are embedded in the runtime code**, not in storage. The bytecode already contains the throwaway OWNER_ADDR (`0x9C56a29c7231974c269E24F9FB3c29203039089E` — a randomly generated key, not a real deployment) and WETH_ADDR (`0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2`). No storage slot overrides are needed for the executor itself. Override `EXECUTOR_OWNER_ADDRESS` at runtime with the real owner key.

### Callback Support

| Callback | Source | Selector |
|----------|--------|----------|
| `uniswapV2Call` | Uniswap V2, SushiSwap V2 | `0x10d1e85c` |
| `hook` | Velodrome, Aerodrome | `0x9a7bff79` |
| `pancakeCall` | PancakeSwap V2 | `0x84800812` |
| `uniswapV3SwapCallback` | Uniswap V3, SushiSwap V3 | `0xfa461e33` |
| `pancakeV3SwapCallback` | PancakeSwap V3 | `0x23a69e75` |
| `unlockCallback` | Uniswap V4 PoolManager | `0x9a7bff79` |

### Key Design Decisions

1. **V2 flash borrows work**: All three V2 callback types are implemented and resume payload delivery.
2. **V3 auto-pay**: When a V3 pool is owed WETH, the callback auto-transfers it. Python encoders must NOT include separate WETH transfer payloads for V3 pools where auto-pay fires.
3. **Strict `__default__`**: Reverts on unknown function calls (unlike the old deployed contract which silently returned, swallowing V2 callbacks).
4. **Bribe support**: `execute_payloads(payloads, bribe_bips)` sends a coinbase bribe proportional to WETH profit.
5. **V4 unlock/settle/take**: The executor's `unlockCallback` resumes payload delivery inside PoolManager's `unlock()` context. Python encodes all V4 operations (swap, sync, settle, take) as raw calldata payloads — the executor treats them identically to V2/V3 payloads. The PoolManager address must be registered via `will_callback=True` on the unlock payload.
6. **`will_callback` registration**: Before calling a pool with `will_callback=True`, the target address is registered in `t_allowed_callback_addresses`. Callback handlers assert that `msg.sender` is registered.
7. **Transient storage**: All queue state uses TLOAD/TSTORE, automatically cleared between transactions.
8. **Increased limits**: `MAX_PAYLOADS=16` (was 8), `MAX_PAYLOAD_BYTES=832` (was 196) to accommodate V4 `PoolManager.swap(PoolKey, SwapParams, bytes32)` calldata.

### Supported Path Types

| Path | Payloads | Mechanism |
|------|----------|-----------|
| V3→V2 (Case 1: zfo=True) | 2 | V3 callback auto-pays WETH; V2 direct swap |
| V3→V2 (Case 2: zfo=False) | 3 | V3 callback auto-pays WETH; explicit WETH transfer to V2 |
| V3→V3 | 4 | Nested V3 callbacks; auto-pay for WETH debts |
| V2→V2 | 4 | V2 flash borrow + direct V2 swap + WETH repayment |
| V2→V3 | 4 | V2 flash borrow + V3 nested callback + WETH repayment |
| V4→V4 | 6 | `unlock()` + V4 swap A + V4 swap B + sync + transfer + settle |
| V4→V3 | 7 | `unlock()` + V4 swap + V3 swap + callback + sync + transfer + settle |
| V4→V2 | 7 | `unlock()` + V4 swap + V2 swap + sync + transfer + settle + take |
| V3→V4 | 7 | V3 swap + callback → `unlock()` + V4 swap + sync + transfer + settle |
| V2→V4 | 7 | V2 flash + callback → `unlock()` + V4 swap + sync + transfer + settle |

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
