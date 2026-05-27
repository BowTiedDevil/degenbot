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
│   ├── fake_uniswap_v2_pair.vy            ← Mock V2 pair with swap + configurable callback dispatch
│   ├── fake_uniswap_v3_pool.vy            ← Mock V3 pool with swap + configurable callback dispatch
│   ├── fake_uniswap_v4_pool_manager.vy   ← Mock V4 PoolManager with unlock/swap/take/settle/sync
│   ├── utility_functions.vy               ← ERC-55 checksum conversion (used by PM for error messages)
│   └── interfaces/
│       ├── UniswapV2/
│       │   ├── IUniswapV2Pair.vyi         ← swap(), token0(), token1()
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
    ├── test_tstore_executor_v4v4.py       ← V4-V4 same/different currency + dynamic amount tests
    ├── test_tstore_executor_v4v3.py       ← V4→V3 and V3→V4 hybrid path tests
    ├── test_tstore_executor_v4v2.py       ← V4→V2, V2→V4, and V4→V2 amount_out regression tests
    ├── test_tstore_executor_v2v2.py       ← V2→V2 flash borrow + direct swap + WETH repayment
    ├── test_tstore_executor_v3v3.py       ← V3→V3 nested callbacks with double auto-pay
    ├── test_tstore_executor_v2v3.py       ← V2→V3 and V3→V2 path tests
    ├── test_tstore_executor_edge_cases.py ← Callback variants, settlement branches, regressions
    └── test_tstore_executor_three_hop.py  ← Three-hop path tests (V4-only and hybrid)
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
| `swap(key, params, hookData)` | Reads pre-configured `swap_amounts[poolId]` to determine `amount_in`/`amount_out`. Applies correct signs based on `zero_for_one`. Updates `t_deltas[msg.sender]` for both currencies. Returns packed `int128 × 2` as `int256`. |
| `take(currency, to, amount)` | Decrements `t_deltas[msg.sender][currency]` by `amount`. Transfers ERC-20 or sends ETH. |
| `sync(currency)` | Records the PM's current ERC-20 balance for that currency (used by `settle()` to compute the delta). |
| `settle()` | For ERC-20: computes `current_balance - sync_balance` and adds to `t_deltas[msg.sender][currency]`. For ETH: credits `msg.value`. |
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

Simulates a V2 pair's optimistic swap lifecycle:

| Method | Behavior |
|--------|----------|
| `swap(amount0Out, amount1Out, to, data)` | Transfers output tokens to `to`. If `data` is non-empty, invokes callback on `to`. After callback, asserts input tokens were paid. |

**Configurable callback dispatch**: The constructor takes a `_callback_variant` parameter:
- `0` (default) → `uniswapV2Call()` (Uniswap/SushiSwap)
- `1` → `hook()` (Aerodrome/Velodrome)
- `2` → `pancakeCall()` (PancakeSwap V2)

This allows testing all three V2 callback entry points with the same fake pair logic.

The V2 swap is an **optimistic transfer**: output tokens are sent BEFORE any input is received. If `data` is non-empty, a callback gives the caller a chance to pay the input tokens. After the callback (or immediately if no callback), the pair checks its balance has increased by the required input amount.

**Unlike V3**, there is no auto-pay of WETH in the executor's V2 callback — the caller must explicitly transfer tokens to the V2 pair during or before the swap. This is a key design difference:

| Feature | V3 Callback | V2 Callback |
|---------|-------------|-------------|
| Auto-pay WETH | Yes (after payload delivery) | No |
| Token payment | Implicit via `transfer()` in callback | Must be explicit payload or pre-transfer |
| Callback data | `amount0Delta, amount1Delta` (signed) | `amount0Out, amount1Out` (unsigned) |

**V4→V2 paths**: The forward token is taken from PM and transferred to the V2 pair BEFORE the V2 swap. V2 then sends WETH to executor via the swap, and checks its forward token balance (already has it from pre-transfer).

**V2→V4 paths**: V2 flash-borrows the forward token to executor. After V4 unlock/settlement produces WETH (via take), a WETH transfer-to-V2 pair payload pays the V2 pair. This payload must come AFTER the unlock so the executor has WETH to transfer.

**V2 swap encoding**: `swap(uint256,uint256,address,bytes)`:
- `zfo=True → (0, amountOut)` → token1 comes out, token0 goes in
- `zfo=False → (amountOut, 0)` → token0 comes out, token1 goes in

### `fake_uniswap_v3_pool.vy` — Mock V3 Pool

Simulates a V3 pool's swap lifecycle:

| Method | Behavior |
|--------|----------|
| `swap(recipient, zero_for_one, amount_specified, sqrtPriceLimitX96, data)` | Transfers output to `recipient`, then invokes callback on `msg.sender`. After the callback returns, asserts the input tokens have been paid. |

**Configurable callback dispatch**: The constructor takes a `_callback_variant` parameter:
- `0` (default) → `uniswapV3SwapCallback()` (Uniswap/SushiSwap)
- `1` → `pancakeV3SwapCallback()` (PancakeSwap V3)

The V3 sign convention: `amount_specified > 0` = exact-input, `amount_specified < 0` = exact-output.

The callback invocation triggers the executor's callback handler, which resumes payload delivery and auto-pays WETH if owed.

The V3 sign convention: `amount_specified > 0` = exact-input, `amount_specified < 0` = exact-output.

The callback invocation is critical: it triggers the executor's `uniswapV3SwapCallback()` handler, which resumes payload delivery and auto-pays WETH.

After the callback, the fake V3 pool asserts that its input token balance equals `amount_in`. This validates that the executor actually paid the V3 pool.

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
