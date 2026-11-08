# Contract Testing with Ape + Foundry

This directory contains an isolated test environment for the `tstore_executor.vy` contract using [Ape](https://apeworx.io/) (the Python smart contract framework) with the Foundry provider and Vyper compiler plugin.

Tests run against **fake contracts** on a local Foundry instance — no mainnet fork, no real liquidity, no real swaps. The fakes simulate the Uniswap V4 PoolManager, V2/V3 pools, and ERC-20 tokens with just enough behavior to verify the executor's settlement logic, callback routing, and delta ledger accounting.

## Directory Layout

```
contracts/tests/
├── ape-config.yaml                        ← Ape project config (foundry + vyper plugins)
├── contracts/
│   ├── tstore_executor.vy                 ← symlink → ../../tstore_executor.vy
│   ├── fake_erc20.vy                      ← Mock ERC-20 with owner-only mint()
│   ├── fake_weth.vy                       ← Mock WETH with deposit()/withdraw()
│   ├── fake_uniswap_v2_pair.vy            ← Mock V2 pair with swap + K-invariant + reset + configurable callback dispatch + configurable fee
│   ├── fake_uniswap_v3_pool.vy            ← Mock V3 pool with swap + configurable callback dispatch
│   ├── fake_uniswap_v4_pool_manager.vy   ← Mock V4 PoolManager with unlock/swap/take/settle/sync
│   ├── utility_functions.vy               ← ERC-55 checksum conversion (used by PM for error messages)
│   └── interfaces/
│       ├── UniswapV2/
│       │   ├── IUniswapV2Pair.vyi         ← swap(), token0(), token1(), reset()
│       │   ├── IUniswapV2Callee.vyi       ← uniswapV2Call()
│       │   ├── IHookCallee.vyi            ← hook() (Aerodrome/Velodrome)
│       │   └── IPancakeCallee.vyi         ← pancakeCall() (PancakeSwap V2)
│       ├── UniswapV3/
│       │   ├── IUniswapV3Pool.vyi         ← swap(), token0(), token1()
│       │   ├── IUniswapV3SwapCallback.vyi ← uniswapV3SwapCallback()
│       │   └── IPancakeV3SwapCallback.vyi ← pancakeV3SwapCallback() (PancakeSwap V3)
│       └── UniswapV4/
│           ├── IPoolManager.vyi           ← swap(), take(), settle(), sync(), unlock()
│           └── IUnlockCallback.vyi        ← unlockCallback()
└── tests/
    ├── test_tstore_executor_v4v4.py       ← V4-V4 same/different currency + dynamic amount + V4→V4→V4 three-hop tests
    ├── test_tstore_executor_v4v3.py       ← V4→V3 and V3→V4 hybrid path tests
    ├── test_tstore_executor_v4v2.py       ← V4→V2, V2→V4, and V4→V2 amount_out regression tests
    ├── test_tstore_executor_v2v2.py       ← V2→V2 flash borrow + direct swap + WETH repayment
    ├── test_tstore_executor_v3v3.py       ← V3→V3 nested callbacks with double auto-pay
    ├── test_tstore_executor_v2v3.py       ← V2→V3 and V3→V2 path tests
    ├── test_tstore_executor_edge_cases.py ← Callback variants, settlement branches, regressions
    ├── test_tstore_executor_three_hop.py  ← Three-hop path tests (V4-only and hybrid)
    ├── test_cmd_executor_v3_three_hop.py ← V3 three-hop tests (V3_SWAP_COMPACT, V3_SWAP_DELTA, auto-pay variants, reverse-order direct custody)
    ├── test_cmd_executor_three_hop_permutations.py ← All 27 V2/V3/V4 three-hop permutations (naive)
    ├── test_cmd_executor_three_hop_optimized.py ← All 27 V2/V3/V4 three-hop permutations with optimized (minimal-transfer) routing
    ├── test_cmd_executor_three_pool_v2.py ← Three-pool V2 triangular arbitrage (3 methods, gas comparison)
    └── ... (cmd_executor tests: compact, dynamic, edge cases, V2-V2/V3-V3, V2-V3, V4-V2, V4-V3, V4-V4, gas benchmarks)
```

The `tstore_executor.vy` in `contracts/` is a **symlink** to the real contract at `contracts/tstore_executor.vy` (two directories up). This means any edits to the real contract are immediately picked up by the test suite — no manual copying.

## Running Tests

From this directory:

```bash
# Run all contract tests (single-worker to avoid foundry crashes)
cd contracts/tests
uv run --with eth-ape --with ape-vyper --with ape-foundry ape test -v -n0

# Run a specific test file
uv run --with eth-ape --with ape-vyper --with ape-foundry ape test tests/test_tstore_executor_v4v4.py -v -n0

# Run a single test by full node ID
uv run --with eth-ape --with ape-vyper --with ape-foundry ape test \
  "tests/test_tstore_executor_v4v4.py::TestV4V4DifferentCurrency::test_v4_v4_usdc_intermediate_weth_eth" -v
```

**Important**: Always pass `-n0` to disable pytest-xdist parallel workers. The Foundry provider creates a single local EVM instance; parallel workers compete for the same port and cause `ConnectionResetError` flakes.

### Prerequisites

The `uv run --with` flags install the required packages into an ephemeral environment. No permanent installation is needed. The packages are:

| Package | Purpose |
|---------|---------|
| `eth-ape` | Test framework, contract interaction, account management |
| `ape-vyper` | Compiles `.vy` contracts using Vyper 0.4.3 |
| `ape-foundry` | Provides local Foundry EVM instance (auto-started/stopped) |

### Compiling Only

To compile without running tests:

```bash
uv run --with eth-ape --with ape-vyper --with ape-foundry ape compile
```

## Design Philosophy

### Why Fake Contracts?

The executor's correctness hinges on **settlement** — making sure every V4 currency delta is zero by the time `unlock()` returns. This is a property of the executor's internal accounting (the `t_v4_deltas` ledger, the take/settle/sync call ordering, the ETH↔WETH conversions), not of real pool math.

Fake contracts let us:

1. **Control swap outputs exactly** — no reliance on real pool state, no flaky tests from changing reserves
2. **Test settlement in isolation** — verify the delta ledger cancels, `sync()`/`settle()` are called in the right order, `CurrencyNotSettled` never fires
3. **Disable the profit check** — the executor has `skip_profit_check=True` for testing, so we can verify settlement correctness on arbitrarily-chosen amounts without worrying about profitability
4. **Run fast and deterministically** — Foundry local is instant, no network calls, no mainnet state dependencies

### What the Fakes Do NOT Simulate

- **Real AMM math** — the fake V4 PM uses pre-configured `amount_in`/`amount_out` values, not constant-product or concentrated-liquidity curves
- **Price impact** — every swap executes at the exact pre-set amounts regardless of size
- **Slippage** — no `sqrtPriceLimitX96` enforcement
- **Protocol fees** — no fee-on-transfer or fee deductions
- **Hook logic** — hooks address is always `address(0)`, no hook callbacks

This is by design: the tests verify the executor's settlement plumbing, not DeFi math.

## The Fake Contracts

### unlockCallback Phases

The executor's `unlockCallback` now has four phases. Understanding these is essential for writing and debugging tests:

| Phase | Purpose | When it runs | What it does |
|-------|---------|--------------|--------------|
| **Phase 0** | Pre-settle | Before V4 swaps | For V3→V4/V2→V4: calls `PM.settle()` for input ERC-20 currencies that were transferred+synced before unlock. Credits the settled amount to `t_v4_deltas`. Only runs when payloads were delivered before the unlock (i.e., non-V4-first paths). Skips `dynamic_amount` swaps and native/WETH inputs. |
| **Phase 1** | V4 swaps | Core | Executes V4 swaps via `extcall`, reads `BalanceDelta` return values, tallies all currency deltas in `t_v4_deltas`. Handles `dynamic_amount` derivation from ledger. |
| **Phase 2** | Queued payloads | After V4 swaps | Delivers remaining queued payloads (take, transfer, V3 swap, etc.). If any payloads were delivered, zeros intermediate ERC-20 deltas so Phase 3 doesn't double-take/settle. |
| **Phase 3** | Auto-settle | After payloads | Settles all remaining nonzero deltas: native ETH, WETH, and any intermediate ERC-20 tokens from V4 swap PoolKeys. Uses `take()` for positive deltas, `sync+transfer+settle()` for negative deltas. The `_v4_settle_currency` helper zeros each delta after settling to prevent double-settlement when the same ERC-20 appears in multiple pool keys. |

**For V4-V4 paths**: Only Phase 1 and Phase 3 run (no pre-settle, no queued payloads in callback).

**For V4→V3 paths**: Phase 1 (V4 swap) → Phase 2 (take forward + transfer to V3 + V3 swap with callback) → Phase 3 (settle WETH/ETH).

**For V3→V4 paths**: Sync+transfer happen before unlock (queued payloads in callback). Phase 0 (settle forward token) → Phase 1 (V4 swap consumes forward) → Phase 3 (settle remaining deltas).

**For V4→V2 paths**: Phase 1 (V4 swap) → Phase 2 (take forward + transfer to V2 + V2 flash swap with callback → V2 callback resumes payloads, none left) → Phase 3 (settle WETH/ETH). V2 pair gets forward tokens from pre-transfer before swap.

**For V2→V4 paths**: V2 flash swap first (callback). Phase 0 (settle forward ERC-20 after sync+transfer to PM) → Phase 1 (V4 swap) → Phase 3 (settle remaining deltas). After unlock returns, a WETH transfer-to-V2 payload pays the V2 pair (V2 callback has no auto-pay).

**V4→V2 amount_out regression**: `TestV4ToV2WrongAmountOut` verifies that passing `forward_out` (USDC amount) instead of `weth_out` (WETH amount) to the V2 swap causes a revert. V2's `swap(amount0Out, amount1Out, ...)` specifies what V2 SENDS — for USDC→WETH@V2, the output is WETH, not USDC.

### `fake_erc20.vy` — Mock ERC-20 Token

A minimal ERC-20 with:
- Standard `transfer()`, `transferFrom()`, `approve()`, `balanceOf()`
- `mint()` — owner-only (deployer), used by test setup to fund pools and the executor
- Constructor takes `(name, symbol, decimals, nominal_initial_supply)` — the `nominal_initial_supply` is multiplied by `10**decimals` automatically, so pass human-readable amounts (e.g., `100_000_000` for 100M tokens)

**Key**: The deployer becomes the `MINTER` immutable. Test fixtures deploy tokens so the test account (Foundry's `accounts[0]`) is the minter, enabling `usdc.mint(pool_manager.address, amount, sender=owner_account)`.

### `fake_weth.vy` — Mock WETH

Wraps `fake_erc20` and adds:
- `deposit()` — payable, mints 1:1 WETH to `msg.sender` (mimics real WETH)
- `withdraw(amount)` — burns WETH, sends `amount` ETH to `msg.sender`

Used as the WETH contract in all tests. The executor is deployed with ETH that gets wrapped in the constructor.

### `fake_uniswap_v4_pool_manager.vy` — Mock V4 PoolManager

The most complex fake. Implements the V4 settlement lifecycle:

| Method | Behavior |
|--------|----------|
| `unlock(data)` | Sets `t_unlocked=True`, calls `msg.sender.unlockCallback(data)`, then checks all `t_deltas` are zero (raises `CurrencyNotSettled:addr:amount` if not) |
| `swap(key, params, hookData)` | **Requires unlock.** Reads pre-configured `swap_amounts[poolId]` to determine `amount_in`/`amount_out`. Applies correct signs based on `zero_for_one`. Updates `t_deltas[msg.sender]` for both currencies. Returns packed `int128 × 2` as `int256`. |
| `take(currency, to, amount)` | **Requires unlock.** Decrements `t_deltas[msg.sender][currency]` by `amount`. Transfers ERC-20 or sends ETH. |
| `sync(currency)` | Records the PM's current ERC-20 balance for that currency (used by `settle()` to compute the delta). **Callable anytime** — no unlock required, matching the real PoolManager. |
| `settle()` | **Requires unlock.** For ERC-20: computes `current_balance - sync_balance` and adds to `t_deltas[msg.sender][currency]`. For ETH: credits `msg.value`. |
| `set_next_swap(pool_key, amount_in, amount_out, zero_for_one, hook_data)` | Pre-configures the next swap on a given pool. Validates that the PM holds enough output tokens. |

#### How `set_next_swap` Works

Before calling the executor's `execute_payloads`, the test must:

1. **Fund the PM with output tokens** — call `usdc.mint(pm.address, amount_out)` or add ETH to `pm.balance`
2. **Configure the swap** — call `pm.set_next_swap(pool_key, amount_in, amount_out, zero_for_one, b"")`

When the executor later calls `pm.swap()`, the PM reads the pre-configured amounts, applies the direction signs, updates its delta ledger, and returns the packed `BalanceDelta`.

#### Pool ID Computation

Pool IDs are computed as `keccak256(abi.encode(currency0, currency1, fee, tick_spacing, hooks))` — matching the real V4 PoolManager. Tests create distinct pool IDs by varying `fee` or `tick_spacing`.

#### Delta Checking in `unlock()`

After `unlockCallback` returns, the fake PM iterates all currencies that appeared in `t_currencies_used` (populated by `swap()` and `take()`). For each currency, it checks `t_deltas[msg.sender][currency] == 0`. If any nonzero delta remains, it raises with a descriptive error like `CurrencyNotSettled:0x5FbDB...:-1000000000000000000`.

This is the exact check that catches the bugs we're testing for.

#### `BalanceDelta` Encoding

The fake PM returns swap deltas as a packed `int256` with two `int128` values:
- Upper 128 bits → `amount0` delta (currency0)
- Lower 128 bits → `amount1` delta (currency1)

The executor decodes this using `slice()` on the `bytes32` representation, matching the real V4 `BalanceDelta` layout. The sign convention:
- **Negative** = the caller owes this currency (debit after swap)
- **Positive** = the caller is owed this currency (credit after swap)

For `zero_for_one=True`: `amount0` = -`amount_in` (owe currency0), `amount1` = +`amount_out` (owed currency1).
For `zero_for_one=False`: `amount0` = +`amount_out`, `amount1` = -`amount_in`.

### `fake_uniswap_v2_pair.vy` — Mock V2 Pair

Simulates a V2 pair's optimistic swap lifecycle with **real constant-product invariant enforcement**. V2 is simple enough (two reserves, no tick math) that the fake pair can run the actual K-check at runtime — no pre-configured swap amounts needed.

| Method | Behavior |
|--------|----------|
| `sync()` | Snapshots current ERC-20 balances as reserves. Call after minting liquidity to the pair. |
| `reset()` | Drains all tokens to address(0) and clears reserves/swap config. Call before re-minting in multi-use test setups. |
| `set_next_swap(amount_in, amount_out, zero_for_one)` | Pre-configures the next swap (backward compat). When configured, also verifies exact output match. |
| `swap(amount0Out, amount1Out, to, data)` | Transfers output tokens to `to`. If `data` is non-empty, invokes callback on `to`. After callback, enforces K-invariant. |

#### Liquidity Setup

V2 pools just need tokens and a `sync()`. No complex position management like V3/V4:

```python
# 1. Mint both tokens to the pair
usdc.mint(pair.address, 4_000_000 * 10**6, sender=owner)
weth.mint(pair.address, 2_000 * 10**18, sender=owner)

# 2. Snapshot as reserves
pair.sync(sender=owner)

# 3. Ready — swap() with any amount up to reserves will work
```

For multi-use pair setups (e.g., gas benchmarks that call setup_fn multiple times), call `reset()` before re-minting to drain leftover tokens from prior swaps:

```python
def setup():
    pair.reset(sender=owner)          # drain tokens + clear state
    usdc.mint(pair.address, ..., sender=owner)
    weth.mint(pair.address, ..., sender=owner)
    pair.sync(sender=owner)
```

For pre-configured swaps (backward compat with existing tests):
```python
pair.set_next_swap(amount_in, amount_out, zero_for_one, sender=owner)
```

When `set_next_swap` is used, `swap()` additionally verifies the exact output matches the configured value. When `sync()` is used alone, the K-invariant is the sole check — any amount satisfying constant-product math is accepted, which is how real V2 works.

#### K-Invariant Enforcement

When no swap is pre-configured (`set_next_swap` not called), `swap()` enforces the real V2 constant-product invariant after each swap:

```
(balance0 * 10000 - amount0In * fee) * (balance1 * 10000 - amount1In * fee) >= reserve0 * reserve1 * 10000²
```

This matches `UniswapV2Pair.sol` exactly. It means:
- `V2_SWAP_CALC` works correctly — it computes `amount_out` from the same `_v2_get_amount_out` formula that satisfies K
- Direct-custody paths (sending tokens to a pair rather than to the executor) work because K is checked via balance delta, not callback data
- No need to pre-compute exact amounts — the on-chain math is the source of truth

When `set_next_swap` is used (for tests that need exact output matching), the K check is skipped — the pre-configured amounts may not satisfy constant-product math because test amounts are often arbitrary. This preserves backward compatibility with existing V2 tests.

**Configurable callback dispatch**: The constructor takes a `_callback_variant` parameter:
- `0` (default) → `uniswapV2Call()` (Uniswap/SushiSwap)
- `1` → `hook()` (Aerodrome/Velodrome)
- `2` → `pancakeCall()` (PancakeSwap V2)

This allows testing all three V2 callback entry points with the same fake pair logic.

The V2 swap is an **optimistic transfer**: output tokens are sent BEFORE any input is received. If `data` is non-empty, a callback gives the caller a chance to pay the input tokens. After the callback (or immediately if no callback), the pair checks its balance has increased by the required input amount.

#### V2 Direct Custody Between Pairs

A key property of V2: `pair.swap()` sends output tokens to the `to` address AND invokes the callback on `to`. For `V2_SWAP_COMPACT` (which uses forward_data), the recipient **must** be the executor so the callback fires there. But `V2_SWAP_CALC` calls `swap()` with `data=b""` (no callback), so it **can** send output directly to the next pool in the chain.

This creates a gas optimization opportunity for multi-pool V2 paths:

| Method | Recipient restriction | Callback? |
|--------|----------------------|-----------|
| `V2_SWAP_COMPACT` | Must be executor (callback target) | Yes — executes forward_data; auto-pay if sentinel |
| `V2_SWAP_CALC` | Any address (no callback needed) | No — pair already has excess balance |
| `V2_SWAP_DIRECT` | Any address (no callback needed) | No — pair enforces K-invariant |

When V2_SWAP_CALC sends output to the next V2 pair, that pair accumulates **excess balance** (balanceOf > reserves). The next V2_SWAP_CALC reads this excess as its swap input — no executor custody, no extra transfer. This chains naturally: Pool A→Pool B→Pool C all send directly between themselves, with only the first and last pools touching the executor.

**Unlike V3**, the executor's V2 callback now supports auto-pay when the sentinel byte (`0xFE`) is passed as `forward_data` to `V2_SWAP_COMPACT`. The sentinel triggers `_v2_auto_pay`, which reads the per-swap fee from `t_v2_pair_fee[pool]` (written by `V2_SWAP_COMPACT` before the `swap()` call) and computes the owed input amount via `_v2_get_amount_in()`. When `forward_data` contains explicit commands, the caller must still transfer tokens to the V2 pair during the callback. When `forward_data` is empty, no callback fires and the pair enforces K-invariant with its own fee. Key design differences:

| Feature | V3 Callback | V2 Callback |
|---------|-------------|-------------|
| Auto-pay | Always (when `forward_data` is empty) | Sentinel-based (`0xFE` as `forward_data` triggers auto-pay; explicit commands remain manual) |
| Fee source | N/A (deltas from callback params) | Inline `fee:2` per `V2_SWAP_COMPACT`/`V2_SWAP_CALC`, written to `t_v2_pair_fee[pool]` |
| Fee validation | N/A | `0 < fee < 10000` (asserted on decode) |
| Callback data | `amount0Delta, amount1Delta` (signed) | `amount0Out, amount1Out` (unsigned) |
| Direct custody direction | **Reverse order only** (IIA checks balance delta during callback) | **Reverse order** (callback-to-recipient constraint; reverse-order flash borrow + V2_SWAP_CALC) |
| Direct custody command | `V3_SWAP_COMPACT(recipient=next_pool)` | `V2_SWAP_CALC(recipient=next_pool)` |

**V4→V2 paths**: The forward token is taken from PM and transferred to the V2 pair BEFORE the V2 swap. V2 then sends WETH to executor via the swap, and checks its forward token balance (already has it from pre-transfer).

**V2→V4 paths**: V2 flash-borrows the forward token to executor. After V4 unlock/settlement produces WETH (via take), a WETH transfer-to-V2 pair payload pays the V2 pair. This payload must come AFTER the unlock so the executor has WETH to transfer.

**V2 swap encoding**: `swap(uint256,uint256,address,bytes)`:
- `zfo=True → (0, amountOut)` → token1 comes out, token0 goes in
- `zfo=False → (amountOut, 0)` → token0 comes out, token1 goes in

### Three-Pool V2 Triangular Arbitrage (`test_cmd_executor_three_pool_v2.py`)

Tests three V2 pools forming a circular arbitrage path: WETH→USDC (A) → USDC→WBTC (B) → WBTC→WETH (C). Pool C is mispriced (more WETH per WBTC than the cross-rate from pools A and B implies), creating profit.

Three approaches are compared, illustrating the trade-off between callback overhead and intermediate custody:

| Approach | Method | Transfers | Callbacks | Gas |
|----------|--------|-----------|-----------|-----|
| 1 | 3× `V2_SWAP_COMPACT` (nested callbacks) | 6 | 3 | ~186,500 |
| 2 | 1× `V2_SWAP_COMPACT` + 2× `V2_SWAP_CALC` (flash + direct custody) | 5 | 1 | ~181,250 |
| 3 | 3× `V2_SWAP_CALC` (zero callbacks, pre-fund first pool) | 4 | 0 | ~177,350 |

#### Approach 1: Naive Nested Callbacks (6 transfers, 3 callbacks)

Every pool sends output to the executor. Each callback pays the next pool. Because V2 calls are blocking, inner callbacks complete first — by the time we need to pay an outer pool, the executor has already received the payment token from an inner pool.

```
Pool A.swap(→executor) → callback:
  Pool B.swap(→executor) → callback:
    Pool C.swap(→executor) → callback: pay WBTC to C
  pay USDC to B
pay WETH to A
```

6 ERC-20 transfers (3 optimistic + 3 callback payments), 3 callbacks.

#### Approach 2: Flash + Direct Custody (5 transfers, 1 callback)

Optimal for real arbitrage. Pool A uses `V2_SWAP_COMPACT` (flash borrow — no WETH required upfront). The callback transfers USDC to Pool B, then uses `V2_SWAP_CALC` for pools B and C. Because `V2_SWAP_CALC` sends output directly to the next pool (no callback needed), Pool B sends WBTC directly to Pool C, and Pool C sends WETH directly to the executor. Only 1 intermediate transfer is routed through the executor (USDC: executor→Pool B).

```
Pool A.swap(→executor) → callback:
  transfer USDC to Pool B (creates excess)
  V2_SWAP_CALC Pool B (→Pool C) — sends WBTC directly, no callback
  V2_SWAP_CALC Pool C (→executor) — sends WETH directly, no callback
  pay WETH to Pool A (flash repayment)
```

5 ERC-20 transfers, 1 callback. Saves ~5,250 gas vs Approach 1 by eliminating 2 callbacks and 1 intermediate custody.

**Why Pool B→Pool C is direct but Pool A→Pool B is not:** V2's callback goes to the `to` address. `V2_SWAP_COMPACT` passes forward_data as callback data, so `to` must be the executor. `V2_SWAP_CALC` uses `data=b""` (no callback), so `to` can be any address — including the next pool.

#### Approach 3: All V2_SWAP_CALC (4 transfers, 0 callbacks, pre-fund required)

The most gas-efficient but requires the executor to hold WETH before executing. The executor pre-funds Pool A with WETH (creates excess balance), then chains V2_SWAP_CALC through all three pools with direct custody between each.

```
transfer WETH to Pool A (creates excess)
V2_SWAP_CALC Pool A (→Pool B) — sends USDC directly, no callback
V2_SWAP_CALC Pool B (→Pool C) — sends WBTC directly, no callback
V2_SWAP_CALC Pool C (→executor) — sends WETH directly, no callback
```

4 ERC-20 transfers, 0 callbacks. Best gas but requires upfront WETH — not suitable for flash arbitrage.

#### When to Use Each Approach

- **Approach 2** is the default for arbitrage — no capital required (flash borrow from Pool A), minimal callbacks, near-optimal gas
- **Approach 3** is best when the executor already holds the input token (e.g., from a previous trade in the same transaction) — saves the flash callback overhead
- **Approach 1** (naive) should be avoided — it works but wastes gas on unnecessary intermediate custody and extra callbacks

Note: V2 supports forward-order direct custody (Approach 2/3) because K-invariant checks total balances after the swap. V3 direct custody requires **reverse-order** execution due to IIA timing constraints — see the V3 three-hop section below.

### Three-Pool V3 Three-Hop (`test_cmd_executor_v3_three_hop.py`)

Tests three V3 pools forming a triangular path: WETH→USDC (V3a) → USDC→WBTC (V3b) → WBTC→WETH (V3c). Exercises both V3 command types and all auto-pay combinations.

#### V3 Commands

| Command | Opcode | Size | Amount Source | Callback |
|---------|--------|------|---------------|----------|
| `V3_SWAP_COMPACT` | 0x30 | 22+N bytes | Explicit `uint128` | Auto-pay if `forward_data` is empty |
| `V2_SWAP_COMPACT` | 0x20 | 24+N bytes | Explicit `uint128` + `fee:2` | Auto-pay via sentinel; explicit commands in forward_data |
| `V3_SWAP_DELTA` | 0x31 | 4 bytes | PM exttload delta | Auto-pay (always empty `forward_data`) |

`V3_SWAP_DELTA` derives the swap amount from the V4 PoolManager's transient storage delta, saving 18+ bytes of calldata encoding per swap. However, it has a **fundamental constraint**: after `V4_TAKE_DELTA` consumes the delta (transferring ERC-20 tokens to the executor), the delta is zero and `V3_SWAP_DELTA` reads 0. Conversely, without `V4_TAKE_DELTA`, the executor lacks the ERC-20 tokens needed for the V3 auto-pay callback. `V3_SWAP_DELTA` therefore only works when the executor holds the input token from an independent source *and* there's a PM delta for that currency.

#### Auto-Pay Combinations

V3's callback handler auto-pays the owed token from the executor's running ERC-20 balance when `forward_data` is empty. In a three-hop V3→V3→V3 path, auto-pay can be applied at different levels:

| Approach | V3a | V3b | V3c | Transfers | Callbacks |
|----------|-----|-----|-----|-----------|-----------|
| Nested callbacks | forward_data | forward_data | forward_data | 6 | 3 |
| Inner auto-pay | forward_data | forward_data | auto-pay | 5 | 3 |
| Middle auto-pay | forward_data | auto-pay | forward_data | 5 | 3 |
| Double auto-pay | forward_data | auto-pay | auto-pay | 4 | 3 |
| **Reverse-order direct custody** | auto-pay | forward_data | forward_data | **4** | 3 |

V3a (outermost in forward order) always requires `forward_data` because the callback must chain subsequent V3 swaps and pay V3a. V3b can auto-pay if the executor has USDC (from V3a's optimistic transfer). V3c can auto-pay if the executor has WBTC (from V3b's optimistic transfer).

The **reverse-order direct custody** row is unique — it eliminates all intermediate executor custody. See the dedicated section below.

#### Reverse-Order Direct Custody

V2's `V2_SWAP_CALC` can send output directly to the next pool, but in a multi-V2 chain the **callback-to-recipient constraint** prevents forward-order chaining with `V2_SWAP_COMPACT` (the first V2 flash swap's callback would land on V2b, which can't process it). The solution is **reverse-order execution**: flash borrow from the last pool (V2c/V3c/V4), then chain V2a→V2b via V2_SWAP_CALC inside the executor's callback. V3 direct custody is also reverse-order-only, but for a different reason: its IIA check requires tokens to arrive *during* the callback, not before `swap()`.

However, V3 direct custody *is* possible by executing swaps in **reverse order**. Each nested swap's output naturally arrives during the outer pool's callback, satisfying IIA timing:

```
Step 1: V3c.swap(→executor) — sends WETH to executor, callbacks
  Step 2:   V3b.swap(→V3c) — sends WBTC directly to V3c, callbacks   ← IIA: WBTC arrives DURING V3c callback ✓
    Step 3:   V3a.swap(→V3b) — sends USDC directly to V3b, callbacks ← IIA: USDC arrives DURING V3b callback ✓
      Step 4:   auto-pay WETH to V3a                                  ← executor pays from its balance ✓
```

As callbacks unwind, each pool's balance-delta check passes because the inner pool sent tokens directly into it during the callback window. The executor only ever holds WETH — no USDC or WBTC custody needed.

**Transfers: 4** (V3c→executor, V3b→V3c, V3a→V3b, executor→V3a) vs 6 in the naive nested-callback approach.

**Why forward-order doesn't work**: If V3a sends USDC to V3b *before* V3b.swap() is called, the USDC is included in V3b's `balance_before` snapshot. The IIA check then requires *additional* USDC to arrive during the callback, which doesn't happen. This is the fundamental difference between V2 (K-invariant checks total balances → forward-order excess balance works) and V3 (IIA checks balance delta during callback → only reverse-order works).

#### Cross-Protocol Three-Hop Paths

The test file also covers mixed V4/V3 paths:

| Path | Flow | Key Commands |
|------|------|-------------|
| V4→V3→V3 | V4 swap → V3b → V3c | `V4_SWAP_COMPACT` + `V4_TAKE` + 2× `V3_SWAP_COMPACT` |
| V4→V3→V3 (delta) | V4 swap → V3b → V3c | `V4_SWAP_COMPACT` + `V3_SWAP_DELTA` + `V3_SWAP_COMPACT` |
| V3→V3→V4 | V3a → V3b → V4 swap | 2× `V3_SWAP_COMPACT` + `V4_UNLOCK` |

The PancakeSwap V3 callback variant (`callback_variant=1`) is also tested, confirming that `pancakeV3SwapCallback` works identically to `uniswapV3SwapCallback` for auto-pay.

### Three-Pool V4 Three-Hop (`test_cmd_executor_v4v4.py`)

Tests three V4 pools forming a triangular path within a single PoolManager: WETH→USDC (V4a) → USDC→WBTC (V4b) → WBTC→WETH (V4c). This is the simplest three-hop variant because V4's delta accounting eliminates all intermediate custody.

All three swaps happen inside a single `unlock()` call — no nested callbacks, no intermediate transfers. Deltas accumulate in the PM's transient storage and net out automatically:

- V4a: −WETH +USDC
- V4b: −USDC +WBTC (USDC cancels)
- V4c: −WBTC +WETH (WBTC cancels)

**Net: −WETH + WETH_profit** — only `V4_TAKE(WETH, executor, profit)` + `V4_SETTLE_DELTA(WETH)` needed.

This contrasts with V2 (6 transfers, K-invariant requires custodial routing) and V3 (4–6 transfers depending on strategy, IIA timing requires reverse-order direct custody). V4 achieves the same result with 2 PM-level operations: one take and one settle.

### Three-Hop Permutation Matrix (`test_cmd_executor_three_hop_permutations.py`)

Tests all 3³ = 27 combinations of V2/V3/V4 pool types for each position in a WETH→USDC→WBTC→WETH three-hop path. Each pool (A, B, C) can be independently V2, V3, or V4.

**Callback nesting patterns** (determined by the outermost pool type):

| Pool A Type | Top-level structure | Callbacks |
|-------------|---------------------|-----------|
| V2 | V2 callback wraps B + C + pay V2 | Nested: A→B→C |
| V3 | V3 callback wraps B + C + pay V3 | Nested: A→B→C |
| V4 | V4 unlock wraps A + B + C + settle | Synchronous: all inside unlock |

**Key constraints discovered:**

1. **V4-in-unlock**: When V4 is Pool A, all swaps execute inside a single `unlock()`. V2/V3 swaps triggered during the unlock fire callbacks normally — they're just synchronous calls within the unlock context. No nested `unlock()` needed (and `AlreadyUnlocked` prevents it).

2. **V4-V2-V4 / V4-V3-V4**: V4-V2-V4 uses a single V4 unlock with V4_TAKE→V2b direct + V2_SWAP_CALC (no V2 callback). V4-V3-V4 uses V3b→PM direct with V4_SYNC+V4_SETTLE for delta netting, eliminating the executor WBTC intermediary.

3. **Fixture layout**: Three currency pairs (WETH/USDC, USDC/WBTC, WBTC/WETH) × three protocol types = 9 pool fixtures. Each permutation uses the appropriate fixture for each position.

**Permutation matrix** (✓ = passes):

| A↓ B→ | V2 | V3 | V4 |
|--------|-----|-----|-----|
| **V2** | ✓ | ✓ | ✓ |
| **V3** | ✓ | ✓ | ✓ |
| **V4** | ✓ | ✓ | ✓ |

All 27 pass. The naming convention is `Test{A}{B}{C}` — e.g., `TestV4V2V3` for V4→V2→V3.

### Three-Hop Optimized Permutations (`test_cmd_executor_three_hop_optimized.py`)

Same 27 permutations as above, but each test applies the **optimal (minimum-transfer) routing** for its combination. The key optimization rules, derived from `docs/pool-mechanics.md`:

| Edge | Optimization | Transfer savings |
|------|-------------|-----------------|
| V2→V2 | Reverse-order flash borrow + V2_SWAP_CALC chain (no callbacks on V2 pairs) | 1/pair |
| V2→V3 | Forward: IIA ✗; **Reverse-order**: V2a V2_SWAP_CALC→V3b during V3b callback (IIA ✓) | 1* |
| V2→V4 | sync+send+settle to PM inside V2 callback | 1 |
| V3→V2 | V3 sends output directly to V2 pair (excess → V2_SWAP_CALC) | 1 |
| V3→V3 | Reverse-order direct custody (inner V3 fires first, outer sends during callback) | 2/pair |
| V3→V4 | V4_TAKE→V3a directly (IIA ✓ during callback) | 1-2 |
| V4→V2 | V4_TAKE sends tokens directly to V2 pair (excess → V2_SWAP_CALC) | 1 |
| V4→V3 | **Reverse-order callback IIA**: V4_TAKE→V3 during V3's own callback satisfies IIA (balance delta between snapshots) | 1 |
| V4→V4 | Delta netting — 0 internal transfers in same unlock | 2/pair |

**Key discovery — V2's callback-to-recipient constraint (resolved):** V2's `swap()` calls `uniswapV2Call` on the `to` address (not `msg.sender`). Forward-order V2→V2 flash swaps with `to=V2b` fail because the callback lands on V2b. However, **reverse-order execution bypasses this entirely**: flash borrow from the last pool (V2c), then chain V2a→V2b via `V2_SWAP_CALC` inside the callback on the executor. `V2_SWAP_CALC` calls `swap(data=b"")` — no callback on the output recipient. See `docs/pool-mechanics.md` §5.1.

**Key discovery — V4→V3 IIA satisfied during V3's own callback:** V3's IIA blocks V4→V3 in forward-order (V4_TAKE deposits tokens before V3.swap() starts, so they're in balance_before). But if V3's swap has already started (we're in V3's callback), V4_TAKE→V3 deposits tokens during the callback window — they appear in balance_after but not balance_before, satisfying IIA. ✓ This unlocks 6→4 for V2-V4-V3, V4-V2-V3, and 5→4 for V4-V3-V2, V4-V3-V3, V4-V3-V4, V3-V4-V3.

**Optimized transfer counts** (naive → optimized):

| A↓ B→ C→ | V2 | V3 | V4 |
|-----------|-----|-----|-----|
| **V2-V2** | 6→4 | 6→4 | 6→4 |
| **V2-V3** | 6→4 | 6→4 | 6→4 |
| **V2-V4** | 6→4 | 6→4 | 6→3 |
| **V3-V2** | 6→4 | 6→4 | 6→4 |
| **V3-V3** | 6→4 | 6→3 | 6→4 |
| **V3-V4** | 6→4 | 6→4 | 6→3 |
| **V4-V2** | 6→4 | 6→4 | 6→4 |
| **V4-V3** | 6→4 | 6→4 | 6→4 |
| **V4-V4** | 6→3 | 5→4 | 2→1 |

25 of 27 at ≤4 transfers. 2 paths at 5 (V2-V3-V2, V2-V4-V2) due to profit-capture constraints
(V2c→V2a would trap WETH profit at the V2 pair instead of the executor).

All 27 optimized tests reach ≤4 transfers. The last two "impossible" paths
(V2-V3-V2, V2-V4-V2) reached 4 via reverse-order from V2c with profit-capture
decoupling: V2c flash swap sends WETH profit to executor, while a separate
ERC20 transfer creates WETH excess at V2a for K-invariant.

All 27 optimized tests pass alongside the 27 naive permutation tests. Total test count: 245 passing.

### `fake_uniswap_v3_pool.vy` — Mock V3 Pool

Simulates a V3 pool's swap lifecycle:

| Method | Behavior |
|--------|----------|
| `swap(recipient, zero_for_one, amount_specified, sqrtPriceLimitX96, data)` | Transfers output to `recipient`, then invokes callback on `msg.sender`. After the callback returns, asserts the input tokens have been paid. |

**Configurable callback dispatch**: The constructor takes a `_callback_variant` parameter:
- `0` (default) → `uniswapV3SwapCallback()` (Uniswap/SushiSwap)
- `1` → `pancakeV3SwapCallback()` (PancakeSwap V3)

The V3 sign convention: `amount_specified > 0` = exact-input, `amount_specified < 0` = exact-output.

The callback invocation triggers the executor's callback handler, which either processes `forward_data` (explicit commands) or auto-pays the owed token from the executor's running ERC-20 balance. Auto-pay is the default when `forward_data` is empty — the callback handler reads the signed deltas from the V3 callback parameters and transfers the positive (owed) amount to the pool.

After the callback, the fake V3 pool asserts `balance_before + amount_owed <= balance_after`. This balance-delta check validates that the executor actually paid the V3 pool, matching the real V3 `IIA` (Insufficient Input Amount) check.

### `utility_functions.vy` — ERC-55 Checksum Helper

A single internal function `_convert_address_to_checksummed_addr_str(address)` that converts an address to its ERC-55 mixed-case checksum string. The fake PM uses this to build descriptive `CurrencyNotSettled` error messages with the unsettled currency's address and delta.

You never call this directly — it's imported by `fake_uniswap_v4_pool_manager.vy`.

## Writing New Tests

### Test Structure

Tests use ape's pytest fixtures. The standard pattern:

```python
class TestMyNewPath:
    def test_scenario_name(
        self,
        usdc: ContractInstance,       # fixture
        weth: ContractInstance,       # fixture
        owner_account: TestAccount,   # fixture
        executor: ContractInstance,   # fixture
        v4_pool_manager: ContractInstance,  # fixture
    ):
        # 1. Define swap amounts
        v4_amount_in = 1 * 10**18
        forward_out = 2000 * 10**6
        v4_amount_out = 2 * 10**18

        # 2. Set up V4 swap
        v4_key = _make_pool_key(weth.address, usdc.address, fee=3000, tick_spacing=60)
        v4_zfo = v4_key[0] == weth.address
        _setup_v4_swap(pm, owner, v4_key, v4_amount_in, forward_out, v4_zfo, output_token=usdc)

        # 3. Build payloads and v4_swaps
        payloads = [...]
        v4_swaps = [...]

        # 4. Execute with skip_profit_check=True
        tx = executor.execute_payloads(
            payloads, v4_swaps, 0, True,
            sender=owner_account, raise_on_revert=False,
        )
        if tx.status == 0:
            tx.show_trace()
            raise ValueError("Transaction reverted")
```

### Key Patterns

#### V4 Pool Key Construction

Always sort currency addresses numerically (ascending) to match V4's convention:

```python
def _make_pool_key(currency0, currency1, fee=0, tick_spacing=60, hooks=ZERO_ADDRESS):
    c0, c1 = sorted([currency0, currency1], key=lambda addr: addr.lower())
    return (c0, c1, fee, tick_spacing, hooks)
```

The `zero_for_one` direction is determined by which sorted position the input token occupies:

```python
zfo = pool_key[0] == input_token_address  # True if input is currency0
```

#### Fund Before Configure

The PM checks that it holds enough output tokens before accepting `set_next_swap`. Always fund first:

```python
# For ERC-20 output:
usdc.mint(pm.address, amount_out, sender=owner)
pm.set_next_swap(pool_key, amount_in, amount_out, zfo, b"", sender=owner)

# For native ETH output:
pm.balance += amount_out
pm.set_next_swap(pool_key, amount_in, amount_out, zfo, b"", sender=owner)
```

#### V4 Sign Convention

V4 uses the **opposite** sign from V3 for `amountSpecified`:

| Mode | V3 | V4 |
|------|----|----|
| Exact INPUT | `amountSpecified > 0` | `amountSpecified < 0` |
| Exact OUTPUT | `amountSpecified < 0` | `amountSpecified > 0` |

For arbitrage (always exact-input): V3 encoding uses **positive**, V4 uses **negative**.

#### Sqrt Price Limits

| Direction | Min (zfo=True) | Max (zfo=False) |
|-----------|----------------|------------------|
| V3 | `MIN_SQRT_RATIO + 1` | `MAX_SQRT_RATIO - 1` |
| V4 | `MIN_SQRT_PRICE_X96 + 1` | `MAX_SQRT_PRICE_X96 - 1` |

The actual values are the same; the names differ between V3 and V4 test constants.

#### `raise_on_revert=False`

Always pass `raise_on_revert=False` to `execute_payloads()` and check `tx.status` manually. This lets you call `tx.show_trace()` on failure to see the full call trace with revert reasons, instead of getting an opaque `ContractLogicError`.

#### `skip_profit_check=True`

The executor's profit check (`combined_after >= combined_before`) is disabled for testing because the fake contracts use arbitrary swap amounts that may not be profitable. The test goal is verifying settlement correctness, not profitability.

### Adding a New Fake Contract

If you need to test a new callback type (e.g., V2 flash borrows):

1. Create `contracts/fake_uniswap_v2_pair.vy` implementing the V2 swap + callback pattern
2. Add the interface under `contracts/interfaces/<Protocol>/`
3. Write tests under `tests/` using the same fixture + helper pattern

The fake should follow the same principles:
- Pre-configure swap outputs via a `set_next_swap()` method
- Invoke the real callback (e.g., `uniswapV2Call()`) on the recipient (not `msg.sender` — V2 calls back on `to`)
- After callback, assert the input tokens were paid
- Use `implements:` the relevant interface for type safety

### Adding a New Path Type

To test a new path type:

1. Add a test class in a new or existing test file
2. Use existing fixtures for tokens, PM, executor
3. Add pool/pair-specific fixtures (e.g., `v2_pair`, `v3_pool`)
4. Follow the payload construction pattern from the Python encoder (`examples/eth_backrun_v2_v3_v4_rust.py`)
5. Use `skip_profit_check=True` and `raise_on_revert=False`
6. For V2→V4 paths: remember the WETH transfer-to-V2 payload after unlock (V2 callback has no auto-pay)

## Common Issues

### Foundry Connection Errors

If you see `ConnectionResetError` or `Connection refused` on port 8545:

- Make sure you're passing `-n0` to disable parallel workers
- Kill any stale foundry processes: `pkill -f anvil`
- Clear the ape cache: `rm -rf contracts/tests/.build`

### `CurrencyNotSettled` in Tests

This is the primary bug we're testing for. If you see it:

1. Call `tx.show_trace()` to see which currency and what delta is unsettled
2. Check the `sync()`/`settle()` call ordering — `sync()` MUST run before the token transfer, then `settle()` after
3. Check that the `t_v4_deltas` ledger is correctly zeroed for pre-settled currencies (explicit take in Phase 2 must be reflected in Phase 3)
4. Check for ETH/WETH currency mismatch — V4 uses `address(0)` for native ETH, but V2/V3 use WETH

### Compilation Errors

- Vyper 0.4.0 does NOT have a `continue` keyword — use nested `if/else` instead
- `int128` max is `2^127 - 1` — V4 `amountSpecified` values that exceed this will overflow
- Transient storage variables (`transient(...)`) are automatically cleared between transactions

### V3 Callback Auto-Pay

The executor's `v3_swap_callback` auto-pays WETH to the V3 pool if owed. The callback computes `owed_token`/`owed_amount` first, then performs a single transfer if the owed token is WETH. This means:
- Do NOT include a separate WETH transfer payload for V3 WETH debts
- The auto-pay only fires when the owed token is WETH — non-WETH debts (e.g., USDC) require explicit transfers in the payload queue
- The fake V3 pool must declare `implements: IUniswapV3Pool` so the executor's `staticcall` to `token0()`/`token1()` works

## Relationship to Mainnet Testing

The ape + foundry fake tests validate the executor's **settlement plumbing** — that deltas cancel, callbacks fire, and tokens move correctly. They complement but do not replace:

1. **Python encoding tests** (`tests/arbitrage/test_v4v4_encoding.py`, `test_swap_encoder.py`) — verify the Python-side ABI encoding and `V4SwapParam` tuple construction
2. **Mainnet simulation tests** (`eth_simulateV1` with code injection) — verify the full pipeline against real pool state and AMM math
3. **Live deployment** — the final verification step

The fake tests catch settlement bugs early (Red phase), before investing in mainnet simulation (which requires contract redeployment with baked-in immutables for code injection).
