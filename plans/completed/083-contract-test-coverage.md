# Plan 083: Expand tstore_executor Contract Test Coverage

## Overview

Fill the test coverage gaps in the `contracts/tests/` Ape + Foundry suite for `tstore_executor.vy`. Currently 10 tests cover only V4-hybrid two-hop paths (V4→V4, V4→V3, V3→V4, V4→V2, V2→V4). This plan adds tests for the four untested V2/V3-only paths, the untested V2/V3 callback variant selectors, the untested settlement branches (native ETH, direct swap), and the entirely-untested three-hop paths — validating that the contract's payload queue and V4 delta ledger handle all configurations correctly.

## Problem

### Deletion test

If you deleted the V2/V3 callback handlers (the inline `_deliver_remaining_payloads()` calls in `uniswapV2Call`/`hook`/`pancakeCall`, and `v3_swap_callback`) from the contract, no test would fail — they have zero coverage when V4 is not involved. If you deleted the `hook` or `pancakeCall` entry points, no test would fail. If you removed native ETH settlement from `_v4_settle_currency`, only the V4→V4 tests would catch it (and only one of the five exercises the native branch). The test suite gives a false sense of coverage for the non-V4-hybrid paths.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| V2/V3-only paths have zero coverage | `contracts/tests/tests/` — no test files | Any regression in V2/V3 callback paths (non-V4 paths) goes undetected |
| V2 variant callbacks (`hook`, `pancakeCall`) untested | `tstore_executor.vy` lines 360–380 | The `t_allowed_callback_addresses` guard is unique per selector; a bug in `hook`/`pancakeCall` routing would not be caught |
| V3 variant callback (`pancakeV3SwapCallback`) untested | `tstore_executor.vy` lines 412–417 | Same issue: selector-specific guard untested |
| V2 direct swap (no flash borrow) untested | `fake_uniswap_v2_pair.vy` `swap()` with `data=b""` | V3→V2(zfo=True) path uses direct swap; no test validates this encoding |
| Native ETH settlement branch partially tested | `_v4_settle_currency` native branch | Only V4→V4 tests use `fund_eth=True`; V4→V3/V4→V2 paths only test WETH settlement |
| V3 auto-pay only tested for WETH | `v3_swap_callback` lines 370–385 | No test where V3 pool is owed a non-WETH ERC-20 (verifying auto-pay does NOT fire) |
| Three-hop paths entirely untested | No test anywhere | Contract limits (16 payloads, 4 V4 swaps, 8 currencies) already support 3+ hops; Python encoder does not; no test validates settlement correctness |
| All tests use WETH as base currency | Every test fixture | No coverage for USDC-base or ETH-base arbitrage paths (different zfo, different reserve ordering) |
| V4→V2 `amount_out` regression is one-directional | `TestV4ToV2WrongAmountOut` | No corresponding V2→V4 encoding regression test |

## Solution

### Step 1: Add V2/V3-only two-hop path tests

Create `test_tstore_executor_v2v2.py`, `test_tstore_executor_v3v3.py`, `test_tstore_executor_v2v3.py`, `test_tstore_executor_v3v2.py`. These exercise the pure payload queue path (`v4_swaps=[]`) with nested callbacks and no V4 involvement.

**V2→V2**: V2 flash borrow (callback) + V2 direct swap (no callback) + WETH repayment payload.

```python
# V2_A flash borrows USDC to executor →
# executor transfers USDC to V2_B →
# V2_B.swap(data=b"") (direct, no callback) sends WETH to executor →
# executor transfers WETH to V2_A to repay flash borrow
payloads = [
    (v2_pair_a.address, v2_swap_data, 0, True),            # V2_A flash borrow
    (usdc.address, transfer_to_v2_b, 0, False),            # Transfer USDC to V2_B
    (v2_pair_b.address, v2_swap_direct, 0, False),          # V2_B direct swap (no callback)
    (weth.address, weth_transfer_to_v2_a, 0, False),       # Repay WETH to V2_A
]
v4_swaps = []
```

**V3→V3**: Nested V3 callbacks with auto-pay.

```python
# V3_A.swap() → callback resumes payloads → V3_B.swap() → inner callback auto-pays WETH
payloads = [
    (v3_pool_a.address, v3_swap_a, 0, True),    # V3_A swap (triggers outer callback)
    (v3_pool_b.address, v3_swap_b, 0, True),    # V3_B swap (triggers inner callback, auto-pays WETH)
]
v4_swaps = []
```

The V3→V3 case is interesting because the inner callback auto-pays WETH to V3_B, then control returns to the outer callback which auto-pays WETH to V3_A. This double-auto-pay has no test.

**V2→V3**: V2 flash borrow + V3 swap with callback + WETH repayment to V2.

**V3→V2**: V3 callback as flash borrow + V2 direct swap. Two sub-cases: zfo=True (V2 direct swap, no callback needed) and zfo=False (requires explicit WETH transfer to V2).

### Step 2: Add callback variant tests

Extend `fake_uniswap_v2_pair.vy` with a configurable callback selector so tests can validate `hook` and `pancakeCall` entry points.

```vyper
# Add to fake_uniswap_v2_pair.vy:
callback_selector: public(String[32])  # "uniswapV2Call", "hook", or "pancakeCall"

@deploy
def __init__(token0: address, token1: address, callback_selector: String[32] = "uniswapV2Call"):
    ...
    self.callback_selector = callback_selector

# In swap(), dispatch based on callback_selector:
# - "uniswapV2Call" → IUniswapV2Callee(to).uniswapV2Call(...)
# - "hook" → IHookCallee(to).hook(...)
# - "pancakeCall" → IPancakeCallee(to).pancakeCall(...)
```

Add interface files for the new callback types under `contracts/interfaces/UniswapV2/`.

For V3 variant: add a `callback_selector` field to `fake_uniswap_v3_pool.vy` that dispatches `uniswapV3SwapCallback` vs `pancakeV3SwapCallback`.

Tests: one test per variant selector on a V2→V2 path (minimal — validates callback routing and address guard) and one V3→V3 path with `pancakeV3SwapCallback`.

### Step 3: Add settlement-branch and edge-case tests

**Native ETH output in V4→V3 path**: V4 swap produces native ETH (not WETH). V3 swap consumes WETH. Phase 3 must unwrap + settle ETH, not sync+transfer WETH. This is the only way to test the unwrap branch alongside a V3 callback.

**V2 direct swap (no flash borrow)**: V3→V2(zfo=True) path. V2 swap is called with `data=b""`, no callback. Output goes directly to executor.

**V3 auto-pay does NOT fire for non-WETH**: A V3→V3 path where pool B is owed USDC (not WETH). The auto-pay code checks `token0()/token1() == WETH_ADDR` and should skip. Without this test, a bug where auto-pay fires for all tokens would go undetected.

**V2→V4 encoding regression**: Simulate the Python-side encoding bug where the V4 `amountSpecified` uses V3 sign convention (positive instead of negative for exact-input). V4 would interpret it as exact-output mode, causing a revert.

### Step 4: Add three-hop path tests

These validate that the 4-phase `unlockCallback` correctly handles multi-hop settlement. The contract already supports this — `MAX_V4_SWAPS=4`, `MAX_PAYLOADS=16`. We add tests that exercise it.

**V4→V4→V4 (3-pool V4-only)**: Three V4 swaps in `v4_swaps`. Swap 1: WETH→USDC. Swap 2: USDC→WBTC (dynamic_amount=True, reads USDC delta). Swap 3: WBTC→WETH (dynamic_amount=True, reads WBTC delta). Phase 3 settles the net WETH delta (debit from swap 1, credit from swap 3), while USDC and WBTC cancel exactly.

```python
v4_swaps = [
    _encode_v4_swap_payload(*pool_a_key, zfo_a, -amount_in, sqrt_limit_a, dynamic_amount=False),
    _encode_v4_swap_payload(*pool_b_key, zfo_b, 0, sqrt_limit_b, dynamic_amount=True),
    _encode_v4_swap_payload(*pool_c_key, zfo_c, 0, sqrt_limit_c, dynamic_amount=True),
]
```

**V4→V3→V2 (3-pool hybrid)**: Most complex. Phase 1: V4 swap (WETH→USDC). Phase 2: take USDC from PM → transfer to V3 → V3 swap (USDC→WETH, callback auto-pays?) → transfer WETH to V2 → V2 direct swap (WETH→token). Phase 3: settle net deltas. This tests the interaction between V4 auto-settle and two nested callback types in a single unlock.

**V2→V3→V4 (3-pool hybrid, reverse direction)**: V2 flash borrow → V3 nested callback → sync+transfer+settle → unlock → V4 swap. Tests Phase 0 pre-settle after two layers of V2/V3 callbacks, and post-unlock WETH transfer to V2.

**V4→V4→V3 (3-pool, V4 multi-hop + V3)**: Two V4 swaps in `v4_swaps` (WETH→USDC→WBTC), then payloads for take WBTC + V3 swap (WBTC→WETH). Phase 2 zeroes both USDC and WBTC deltas. Phase 3 settles net WETH.

### Step 5: Update documentation

Update `contracts/README.md` supported path types table, `contracts/tests/README.md` test catalogue, and the AGENTS.md test count reference. Add three-hop path documentation.

### Design decisions

- **Fake V2 pair extensibility vs. new fake contracts**: Extend the existing `fake_uniswap_v2_pair.vy` with a configurable `callback_selector` constructor parameter. This avoids duplicating the entire pair logic for each callback variant. The fake already supports `IUniswapV2Callee`; add `IHookCallee` and `IPancakeCallee` interfaces with the same signature but different selector.
- **Test file organization**: One file per path family (V2V2, V3V3, V2V3, V3V2, three-hop) rather than one monolithic file. This keeps test classes small and runnable independently.
- **Three-hop tests use `skip_profit_check=True`**: Same as existing tests — we are testing settlement plumbing, not AMM math.
- **`_zero_intermediate_deltas` with 3+ V4 swaps**: The existing function already handles multiple intermediates (it iterates `MAX_V4_SWAPS`). No contract changes needed.
- **No contract changes in this plan**: All improvements are on the test side. The contract already supports everything we're testing. If a test reveals a contract bug, that becomes a separate fix.

## Files Involved

**Primary:**
- `contracts/tests/tests/test_tstore_executor_v2v2.py` — new: V2→V2 path tests
- `contracts/tests/tests/test_tstore_executor_v3v3.py` — new: V3→V3 path tests
- `contracts/tests/tests/test_tstore_executor_v2v3.py` — new: V2→V3 and V3→V2 path tests
- `contracts/tests/tests/test_tstore_executor_three_hop.py` — new: 3-hop path tests
- `contracts/tests/tests/test_tstore_executor_edge_cases.py` — new: settlement branches, variants, regressions

**Secondary:**
- `contracts/tests/contracts/fake_uniswap_v2_pair.vy` — add `callback_selector` constructor param and dispatch logic
- `contracts/tests/contracts/fake_uniswap_v3_pool.vy` — add `callback_selector` constructor param for pancakeV3
- `contracts/tests/contracts/interfaces/UniswapV2/IHookCallee.vyi` — new: hook callback interface
- `contracts/tests/contracts/interfaces/UniswapV2/IPancakeCallee.vyi` — new: pancakeCall callback interface
- `contracts/tests/contracts/interfaces/UniswapV3/IPancakeV3SwapCallback.vyi` — new: pancake V3 callback interface

**Documentation:**
- `contracts/README.md` — update supported path types, add three-hop path docs
- `contracts/tests/README.md` — update test catalogue, add three-hop patterns
- `AGENTS.md` — update test count from "10 tests across 3 files"

**No change needed:**
- `contracts/tstore_executor.vy` — contract already supports all paths being tested
- `contracts/tests/contracts/fake_erc20.vy` — sufficient as-is
- `contracts/tests/contracts/fake_weth.vy` — sufficient as-is
- `contracts/tests/contracts/fake_uniswap_v4_pool_manager.vy` — sufficient as-is

## Implementation Order

### Slice 1: V2→V2 path

1. Create `test_tstore_executor_v2v2.py` with a single test: WETH→USDC@V2_A (flash borrow), USDC→WETH@V2_B (direct swap, no callback), WETH repayment to V2_A
2. Run: `cd contracts/tests && uv run --with eth-ape --with ape-vyper --with ape-foundry ape test tests/test_tstore_executor_v2v2.py -v -n0` — expect 1 pass

### Slice 2: V3→V3 path

1. Create `test_tstore_executor_v3v3.py` with a single test: WETH→USDC@V3_A (callback resumes V3_B), USDC→WETH@V3_B (inner callback auto-pays WETH). Validates double auto-pay.
2. Run: `cd contracts/tests && uv run --with eth-ape --with ape-vyper --with ape-foundry ape test tests/test_tstore_executor_v3v3.py -v -n0` — expect 1 pass

### Slice 3: V2→V3 and V3→V2 paths

1. Create `test_tstore_executor_v2v3.py` with two tests:
   - `test_v2_v3_weth_usdc_to_weth`: V2 flash borrows USDC, executor transfers USDC to V3, V3 swap produces WETH (callback auto-pays), post-callback WETH transfer to V2 repays flash
   - `test_v3_v2_weth_usdc_to_weth`: V3 callback resumes V2 swap. Two sub-tests: (a) V2 direct swap (zfo=True), (b) V2 with explicit WETH transfer (zfo=False)
2. Run: `cd contracts/tests && uv run --with eth-ape --with ape-vyper --with ape-foundry ape test tests/test_tstore_executor_v2v3.py -v -n0` — expect 3 pass

### Slice 4: Callback variant selector support

1. Add `IHookCallee.vyi` and `IPancakeCallee.vyi` interfaces
2. Extend `fake_uniswap_v2_pair.vy` with `callback_selector` constructor param and dispatch in `swap()`
3. Add `IPancakeV3SwapCallback.vyi` interface
4. Extend `fake_uniswap_v3_pool.vy` with `callback_selector` constructor param and dispatch in `swap()`
5. Add tests to `test_tstore_executor_edge_cases.py`:
   - `test_hook_callback_settles`: V2→V2 path using `hook` selector
   - `test_pancake_call_settles`: V2→V2 path using `pancakeCall` selector
   - `test_pancake_v3_callback_settles`: V3→V3 path using `pancakeV3SwapCallback` selector
6. Run: `cd contracts/tests && uv run --with eth-ape --with ape-vyper --with ape-foundry ape test -v -n0` — expect all existing + 3 new pass

### Slice 5: Settlement-branch and edge-case tests

Add to `test_tstore_executor_edge_cases.py`:
1. `test_v4_v3_native_eth_output`: V4→V3 path where V4 swap sends native ETH (not WETH). Phase 3 unwraps WETH and settles with `msg.value`. Requires V4 pool key with NATIVE_ADDRESS.
2. `test_v2_direct_swap_no_callback`: V3→V2(zfo=True) path where V2 is called with `data=b""`.
3. `test_v3_no_autopay_for_non_weth`: V3→V3 path where pool B is owed USDC. Verifies auto-pay does NOT fire (no spurious WETH transfer).
4. `test_v2_v4_wrong_sign_convention_reverts`: V2→V4 path with V3 sign convention for `amountSpecified` (positive instead of negative). Must revert.
5. Run: `cd contracts/tests && uv run --with eth-ape --with ape-vyper --with ape-foundry ape test tests/test_tstore_executor_edge_cases.py -v -n0` — expect 7 pass (3 from Slice 4 + 4 new)

### Slice 6: Three-hop V4-only path

1. Add to `test_tstore_executor_three_hop.py`:
   - `test_v4_v4_v4_three_pool`: WETH→USDC@V4_A → USDC→WBTC@V4_B (dynamic) → WBTC→WETH@V4_C (dynamic). Three V4 swaps in one `v4_swaps`. Validates `_zero_intermediate_deltas` with two intermediate currencies.
2. Run: `cd contracts/tests && uv run --with eth-ape --with ape-vyper --with ape-foundry ape test tests/test_tstore_executor_three_hop.py -v -n0` — expect 1 pass

### Slice 7: Three-hop hybrid paths

Add to `test_tstore_executor_three_hop.py`:
1. `test_v4_v3_v2_three_hop`: WETH→USDC@V4 → USDC→WBTC@V3 → WBTC→WETH@V2. Phase 1: V4 swap. Phase 2: take + V3 swap (callback) + V2 swap (callback or direct). Phase 3: settle deltas. Tests payload delivery after V4 swaps with two different callback types.
2. `test_v2_v3_v4_three_hop`: USDC→WETH@V2 (flash borrow) → WETH→WBTC@V3 (callback) → WBTC→USDC@V4 (unlock + swap). V2/V3 callbacks run before unlock, forward token is pre-settled in Phase 0, V4 swap in Phase 1, Phase 3 settles. Post-unlock: WETH transfer to V2.
3. `test_v4_v4_v3_three_hop`: WETH→USDC@V4_A → USDC→WBTC@V4_B (dynamic) → WBTC→WETH@V3. Two V4 swaps in `v4_swaps` + V3 payload. Phase 2 zeroes both USDC and WBTC deltas. Phase 3 settles net WETH.
4. Run: `cd contracts/tests && uv run --with eth-ape --with ape-vyper --with ape-foundry ape test tests/test_tstore_executor_three_hop.py -v -n0` — expect 4 pass

### Slice 8: Update documentation and validate

1. Update `contracts/README.md` — add three-hop rows to supported path types table
2. Update `contracts/tests/README.md` — add new test files to directory layout, add three-hop patterns section
3. Update `AGENTS.md` — change "10 tests across 3 files" to new count
4. Run full suite: `cd contracts/tests && uv run --with eth-ape --with ape-vyper --with ape-foundry ape test -v -n0` — expect all pass
5. Run `just lint` — expect clean

## Testing

### Per-slice test runs

Each slice runs `cd contracts/tests && uv run --with eth-ape --with ape-vyper --with ape-foundry ape test -v -n0`. The contract test suite is isolated from the Python test suite.

### New unit tests

```python
# test_tstore_executor_v2v2.py
class TestV2V2:
    def test_v2_v2_flash_borrow_direct_swap_repayment(
        self, usdc, weth, owner_account, executor, v2_pair_a, v2_pair_b
    ):
        """V2_A flash borrows USDC to executor, V2_B direct swap for WETH, repay V2_A with WETH."""

# test_tstore_executor_v3v3.py
class TestV3V3:
    def test_v3_v3_nested_callback_double_autopay(
        self, usdc, weth, owner_account, executor, v3_pool_a, v3_pool_b
    ):
        """V3_A callback resumes V3_B swap, inner callback auto-pays WETH to V3_B, outer auto-pays to V3_A."""

# test_tstore_executor_v2v3.py
class TestV2ToV3:
    def test_v2_v3_weth_usdc_to_weth(
        self, usdc, weth, owner_account, executor, v2_pair, v3_pool
    ):
        """V2 flash borrows USDC, V3 swap produces WETH, post-callback WETH repays V2."""

class TestV3ToV2:
    def test_v3_v2_direct_swap_zfo_true(
        self, usdc, weth, owner_account, executor, v3_pool, v2_pair
    ):
        """V3 callback resumes V2 direct swap (no callback)."""
    def test_v3_v2_explicit_weth_transfer_zfo_false(
        self, usdc, weth, owner_account, executor, v3_pool, v2_pair
    ):
        """V3 callback resumes V2 swap, explicit WETH transfer payload required."""

# test_tstore_executor_edge_cases.py
class TestCallbackVariants:
    def test_hook_callback_settles(self, ...):
        """V2→V2 path using hook() callback selector — validates Aerodrome/Velodrome routing."""
    def test_pancake_call_settles(self, ...):
        """V2→V2 path using pancakeCall() callback selector — validates PancakeSwap V2 routing."""
    def test_pancake_v3_callback_settles(self, ...):
        """V3→V3 path using pancakeV3SwapCallback() — validates PancakeSwap V3 routing."""

class TestSettlementBranches:
    def test_v4_v3_native_eth_output(self, ...):
        """V4 swap produces native ETH (not WETH), V3 swap consumes WETH — Phase 3 unwraps."""
    def test_v2_direct_swap_no_callback(self, ...):
        """V2 swap called with data=b'' — no callback, direct output."""
    def test_v3_no_autopay_for_non_weth(self, ...):
        """V3 pool owed USDC (not WETH) — auto-pay must not fire."""
    def test_v2_v4_wrong_sign_convention_reverts(self, ...):
        """V4 amountSpecified uses V3 sign (positive not negative) — must revert."""

# test_tstore_executor_three_hop.py
class TestThreeHopV4Only:
    def test_v4_v4_v4_three_pool(self, ...):
        """3 V4 swaps: WETH→USDC→WBTC→WETH. Two intermediates cancelled via dynamic_amount."""

class TestThreeHopHybrid:
    def test_v4_v3_v2_three_hop(self, ...):
        """V4→V3→V2: Phase 1 V4, Phase 2 V3 callback + V2, Phase 3 settle."""
    def test_v2_v3_v4_three_hop(self, ...):
        """V2→V3→V4: V2/V3 callbacks, Phase 0 pre-settle, Phase 1 V4, post-unlock WETH to V2."""
    def test_v4_v4_v3_three_hop(self, ...):
        """V4→V4→V3: Two V4 swaps + V3 payload. Two intermediates zeroed in Phase 2."""
```

### Integration tests

Existing tests in `test_tstore_executor_v4v4.py`, `test_tstore_executor_v4v3.py`, `test_tstore_executor_v4v2.py` remain unchanged and must continue passing. The Python-side encoding tests (`tests/arbitrage/test_v4v4_encoding.py`, `test_swap_encoder.py`) are not modified — they test a different layer.

## Benefits

- **Depth**: V2/V3-only paths test the callback chain at its deepest (nested callbacks with no V4 safety net). V4 auto-settle is forgiving (reads actual deltas); V2/V3 settlement is unforgiving (must transfer exact amounts).
- **Leverage**: Configurable callback selector on fake V2 pair gives `hook`/`pancakeCall` testing for free — one infrastructure change, three tested entry points.
- **Locality**: Three-hop tests prove the contract already handles multi-hop paths without changes, closing the open question from the audit.
- **Seam testing**: Settlement-branch tests exercise each branch of `_v4_settle_currency` (native ETH, WETH, ERC-20) in combination with V2/V3 callbacks, not just in isolation.

## Risks

- **Fake contract complexity**: Adding `callback_selector` to fake V2/V3 contracts increases their surface area. Mitigation: keep the dispatch logic trivial (one `if/elif/else` on the string) and add a test that verifies the default (`"uniswapV2Call"`) still works identically to the current fake.
- **Three-hop test fragility**: Complex payload chains may have edge cases in delta ledger accounting (e.g., `_zero_intermediate_deltas` iterating currencies from V4 PoolKeys only, not from V3/V2 intermediaries). Mitigation: start with V4-only 3-hop (all currencies in PoolKeys), then add hybrid paths one at a time with `tx.show_trace()` on failure.
- **Foundry flakiness**: The `-n0` constraint already exists. More tests = longer runs. Mitigation: current suite runs in <30s; even doubling it stays well under 2 minutes.
- **Ape/Vyper compiler version drift**: The fake contracts use Vyper 0.4.3. Adding `implements:` for new interfaces may surface compilation issues. Mitigation: test compilation after each fake contract change (Slice 4).

## Relationship to Other Plans

- **Plan 080** (Rust bot POC): The `tstore_executor.vy` was built under Plan 080 with the initial 10 contract tests. This plan extends the test coverage that Plan 080 established.
- **Plan 081** (V4 extension): Added V4 swap support and the 4-phase `unlockCallback`. This plan tests V4 paths more thoroughly (3-hop, native ETH branch) but does not change the contract.
- **Plan 082** (Rust-owned state pipeline): Added Mint/Burn/ModifyLiquidity event handling to the Rust engine. Orthogonal — this plan is about the on-chain executor contract, not the off-chain state pipeline.
- **Future Python encoder plan**: This plan's three-hop tests validate the *contract* can handle 3+ hops. A separate future plan would extend the Python `encode_payloads()` function to produce 3-hop payloads, using these contract tests as the validation target.

## Status

- [x] Slice 1: V2→V2 path test
- [x] Slice 2: V3→V3 path test
- [x] Slice 3: V2→V3 and V3→V2 path tests
- [x] Slice 4: Callback variant selector support and tests
- [x] Slice 5: Settlement-branch and edge-case tests
- [x] Slice 6: Three-hop V4-only path test
- [x] Slice 7: Three-hop hybrid path tests
- [x] Slice 8: Update documentation and full-suite validation
