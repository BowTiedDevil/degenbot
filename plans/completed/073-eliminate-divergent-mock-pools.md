# Plan 073: Eliminate Divergent Mock Pools

## Overview

Replace all ad-hoc mock/fake pool classes in tests with direct construction of production pool objects (`UniswapV2Pool`, `UniswapV3Pool`, `UniswapV4Pool`, `AerodromeV2Pool`, `CamelotLiquidityPool`, `CurveStableswapPool`) using `FakeToken` and `FakeCurveDataProvider` as lightweight test doubles. Consolidate token/address constants and duplicated swap math. The I/O-free pool refactor (Plans 039–049, ADR-001 Phase 3) made this possible — production pool constructors now accept all data as parameters with no RPC calls. The existing mocks reimplement production math with divergent approximations (float arithmetic in V2, simplified sqrt-price math in V3, duplicated Newton's method in Curve) and rely on brittle `unittest.mock.patch` calls that break when constructor signatures change.

Out of scope: Aave mock consolidation (extracted to a separate plan) and builder FakeProvider/FakeAsyncProvider consolidation (audited — five implementations use three genuinely different matching strategies; the ~120 lines of duplication does not justify a shared abstraction).

## Problem

### Deletion test

If you deleted all mock pool classes (`MockV2Pool`, `MockV3Pool`, `MockV4Pool`, `MockLiquidityPool`, `MockV3LiquidityPool`, `FakeV3PoolWithTicks`, `FakeCurveStableswapPool`, `FakeUniswapV2Pool`, `FakeConcentratedLiquidityPool`, `FakeAerodromeV2Pool`, `FakeCamelotPool`, `MockCurveSwapper`, `OfflineV2Pool`, `OfflineV3Pool`), the tests that import them would fail to compile. But these tests are testing mock behavior, not production behavior. If the tests used production pools instead, they would test real code paths and any mock-specific test failures would reveal real bugs in production code — which is the desired outcome.

The same logic applies to the other divergent test doubles identified in this plan: duplicated token constants and duplicated swap math produce tests that pass against local approximations rather than production code.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| MockV3Pool uses `float` arithmetic for V2 constant-product math | `tests/arbitrage/mock_pools.py` (`calculate_tokens_out_from_tokens_in`: `fee_multiplier = 1 - float(self.fee)`) | Produces incorrect swap amounts vs. production integer math; tests pass against wrong behavior |
| MockV3Pool uses approximate sqrt-price math instead of tick-crossing | `tests/arbitrage/mock_pools.py` (`calculate_tokens_out_from_tokens_in`: comment: "WARNING: simplified implementation") | Tests that depend on V3 swap output are testing an approximation, not the real invariant |
| MockLiquidityPool bypasses constructor via attribute smashing | `tests/fakes/pools.py:76-96`, `tests/arbitrage/integration/test_uniswap_lp_cycle.py` | Breaks silently when constructor adds required parameters (same failure mode as commit 1783c742) |
| OfflineV2Pool/OfflineV3Pool bypass constructor via attribute smashing | `tests/arbitrage/test_offline_integration.py:56-177` | Same antipattern as MockLiquidityPool — the offline classes themselves are ~120 lines of vestigial constructor-bypass code. The remaining ~830 lines of test functions are valuable and will be rewritten against production pools with proper V2/V3 update types. |
| FakeCurveStableswapPool reimplements Newton's method with divergent precision | `tests/arbitrage/fake_curve_pool.py` (`_get_d`, `_get_y`) | Computes in XP-space where fee truncation causes zero-fee output for typical pool sizes (e.g. `_get_dy(0,1,1000e18)` returns 999000000 vs production 999599950 — 0.06% divergence). Production math works in raw-balance space avoiding this truncation. Replacing with `CurveStableswapPool` + `FakeCurveDataProvider` eliminates the divergence entirely. |
| `create_cycle_with_mocks()` applies 4 `unittest.mock.patch` calls to `_UniswapLpCycle` | `tests/arbitrage/mock_pools.py` | Fragile — patches break when the legacy cycle class changes; impossible to test concurrent cycles |
| `FakeV3PoolWithTicks` reimplements tick bitmap construction | `tests/arbitrage/fake_pools.py` (`_build_tick_bitmap`: uses `tick >> 8` compression) | Bug-prone: uses `tick >> 8` compression instead of `get_tick_word_and_bit_position`, diverges from production bitmap logic |
| conftest.py defines 5 fake pool classes + 2 fake state types | `tests/arbitrage/test_path/conftest.py` | Each fake duplicates `to_hop_state()`, `build_swap_amount()`, `simulate_swap()`, `subscribe/unsubscribe` — any production change requires manual sync across all fakes |
| MockCurveSwapper reimplements Curve D and get_y inline | `tests/arbitrage/integration/test_curve_equivalence.py:34-100` | Third copy of Newton's method (after `stableswap.py` and `fake_curve_pool.py`); triple maintenance burden |
| 5 duplicated FakeProvider/FakeAsyncProvider classes | `tests/builders/test_*.py` | Out of scope — audited and found to use three genuinely different matching strategies (selector-keyed, full-calldata-keyed, `(to, data)`-tuple-keyed) plus one call-counting variant. The ~120 lines of duplication does not justify a shared abstraction. |
| 26 duplicated MockScaledEvent/MockOperation across Aave tests | `tests/aave/enrichment/handlers/test_*.py` (13 files × 2 classes) | Out of scope — extracted to a separate plan. `MockScaledEvent`/`MockOperation` are structural factory helpers, not divergent math. |
| WETH/USDC/DAI address strings appear 246/478/38 times across test files | Various | Every test file that needs mainnet tokens re-declares the same address string; no single source of truth |
| `OfflineErc20Token` is a third FakeToken variant | `tests/arbitrage/test_offline_integration.py:179` | Functionally identical to `FakeToken` (address, symbol, decimals, address-based equality) — unnecessary duplication |
| `bot_test_harness_prototype.py` (346 lines) has zero imports | `tests/helpers/bot_test_harness_prototype.py` | Dead code superseded by `FakeCurveDataProvider` pattern |
| Constant-product swap math re-implemented 5 times in tests | `tests/arbitrage/mock_pools.py` (float), `verify_legacy_equivalence.py` (correct form), `test_path/conftest.py` (correct form), `test_path/test_swap_amounts.py` (correct form), `test_v3_only_legacy_equivalence.py` (correct form) | Only `mock_pools.py` uses divergent float arithmetic; the other 4 use the correct single-expression form `amount_in * (fee.denominator - fee.numerator)` matching `constant_product_calc_exact_in`. All should be replaced for single-source-of-truth, but only `mock_pools.py` has a correctness bug. |
| `calculations/constant_product.get_amount_out` is buggy and unused by production | `src/degenbot/calculations/constant_product.py` | Diverges from Solidity formula for small `amount_in` due to intermediate integer truncation in fee deduction; only imported by `tests/test_calculations.py`; `v2_functions.constant_product_calc_exact_in` is the correct implementation used by production pools |

## Solution

### Step 1: Upgrade FakeToken + replace OfflineErc20Token

Add `name` attribute and `AddressComparable` inheritance to `FakeToken` so it satisfies `AbstractErc20Token` structurally. This is the enabler for all subsequent pool-replacement steps — production pools access only `.address`, `.chain_id`, `.decimals`, and `.symbol` on token objects. `AddressComparable` provides `__eq__`, `__hash__`, `__lt__` by address, matching `Erc20Token`'s behavior and eliminating `FakeToken`'s custom `__eq__` that uses `hasattr` duck-typing.

With `FakeToken` upgraded, `OfflineErc20Token` in `test_offline_integration.py` can be replaced by `FakeToken` directly — they provide the same interface.

```python
# Before (tests/fakes/tokens.py)
@dataclass(frozen=True)
class FakeToken:
    address: "ChecksumAddress"
    symbol: str = "TKN"
    decimals: int = 18
    chain_id: int = 1

# After
@dataclass(frozen=True)
class FakeToken(AddressComparable):
    address: "ChecksumAddress"
    name: str = "Token"
    symbol: str = "TKN"
    decimals: int = 18
    chain_id: int = 1
```

### Step 2: Consolidate test token/address constants

Create `tests/constants.py` to centralize the frequently-duplicated mainnet addresses and token objects. Both `tests/uniswap/v2/conftest.py` and `v3/conftest.py` construct identical `Erc20Token` objects (WBTC, WETH) — these become shared fixtures.

```python
# tests/constants.py
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import WRAPPED_NATIVE_TOKENS

WETH_ETH = WRAPPED_NATIVE_TOKENS[1]
WBTC_ETH = get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
DAI_ETH = get_checksum_address("0x6B175474E89094C44Da98b954EedeAC495271d0F")
USDC_ETH = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
```

### Step 3: Replace MockLiquidityPool and MockV3LiquidityPool with production pools

These classes subclass `UniswapV2Pool`/`UniswapV3Pool` and bypass the constructor by setting `_state_cache` and `_subscribers` directly. Callers then smash in `.address`, `._token0`, `._fee_token0`, etc. one attribute at a time.

```python
# Before (tests/arbitrage/integration/test_uniswap_lp_cycle.py)
lp_1 = MockLiquidityPool()
lp_1.name = "WBTC-WETH (V2, 0.30%)"
lp_1.address = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
lp_1.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
lp_1._fee_token0 = Fraction(3, 1000)
lp_1._fee_token1 = Fraction(3, 1000)
lp_1.external_update(UniswapV2PoolExternalUpdate(block_number=1, reserves_token0=..., reserves_token1=...))
lp_1._token0 = wbtc_token
lp_1._token1 = weth_token

# After
lp_1 = UniswapV2Pool(
    address="0xBb2b8038a1640196FbE3e38816F3e67Cba72D940",
    token0=wbtc_token,
    token1=weth_token,
    factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
    fee_token0=Fraction(3, 1000),
    fee_token1=Fraction(3, 1000),
    reserves_token0=...,
    reserves_token1=...,
    state_block=1,
)
```

### Step 4: Replace MockV2Pool, MockV3Pool, MockV4Pool + OfflineV2Pool, OfflineV3Pool

`MockV2Pool` (530-line standalone class) reimplements constant-product math using float arithmetic. Production `UniswapV2Pool` does the same math with integer precision in `UniswapV2PoolCalc`.

`MockV3Pool` uses approximate sqrt-price math (`price = sqrt_price * sqrt_price`, `amount_out = int(token_in_quantity * price * 0.997)`). The production pool's `to_hop_state()` produces `BoundedProductHop` with virtual reserves that correctly represent the concentrated-liquidity position.

`MockV4Pool` inherits from `MockV3Pool` and adds `pool_id`. The production `UniswapV4Pool` accepts `pool_id` as a constructor parameter.

`OfflineV2Pool` and `OfflineV3Pool` (in `test_offline_integration.py`) are the same antipattern — they subclass production pools and bypass the constructor with attribute smashing. Their stated purpose ("run without RPC") is now natively served by I/O-free constructors. Replace with direct production constructor calls.

The `create_cycle_with_mocks()` function and its 4 `unittest.mock.patch` calls on `_UniswapLpCycle` become unnecessary when using real pools — the production pools satisfy the cycle's interface directly.

### Step 5: Replace FakeV3PoolWithTicks with UniswapV3Pool + apply_liquidity_mapping_update

`FakeV3PoolWithTicks` extends `MockV3Pool` (Step 4) and adds its own tick data construction via `_add_tick_liquidity()` and `_build_tick_bitmap()`. These reimplement what the production pool does through `apply_liquidity_mapping_update` (extracted in commit 1783c742).

```python
# After: build tick state via production pure function, then construct pool
from degenbot.calculations.concentrated_liquidity import apply_liquidity_mapping_update

tick_bitmap: dict[int, BitmapAtWord] = {}
tick_data: dict[int, LiquidityAtTick] = {}
liquidity = 0

for range_def in tick_ranges:
    result = apply_liquidity_mapping_update(
        tick_bitmap=tick_bitmap,
        tick_data=tick_data,
        tick_spacing=tick_spacing,
        tick=current_tick,
        liquidity=liquidity,
        initial_state_block=2**256 - 1,  # skip in-range adjustment on build
        update_block=0,
        tick_lower=range_def.tick_lower,
        tick_upper=range_def.tick_upper,
        liquidity_delta=range_def.liquidity,
    )
    tick_bitmap = result.tick_bitmap
    tick_data = result.tick_data
    liquidity = result.liquidity

pool = UniswapV3Pool(
    address=address, token0=token0, token1=token1, factory=factory,
    fee=fee, tick_spacing=tick_spacing, sqrt_price_x96=current_sqrt_price_x96,
    tick=current_tick, liquidity=liquidity,
    tick_bitmap=tick_bitmap, tick_data=tick_data,
)
```

### Step 6: Replace test_path/conftest.py fake pools with production pools

Replace `FakeUniswapV2Pool`, `FakeConcentratedLiquidityPool`, `FakeAerodromeV2Pool` with production pool construction. Delete `FakeCamelotPool` (dead code — zero uses outside its definition in conftest.py). Each fake's `to_hop_state()` and `build_swap_amount()` already match the production implementations — the fakes are pure duplication.

- `FakeUniswapV2Pool` → `UniswapV2Pool`
- `FakeConcentratedLiquidityPool` → `UniswapV3Pool`
- `FakeAerodromeV2Pool` → `AerodromeV2Pool`
- `FakeCamelotPool` → delete (dead code)
- `FakeV2PoolState` / `FakeCLPoolState` → delete (replaced by production state types)

### Step 7: Replace FakeCurveStableswapPool and MockCurveSwapper

`FakeCurveStableswapPool` reimplements Curve invariant math (368 lines). The production `CurveStableswapPool` + `FakeCurveDataProvider` pattern already exists as a working example in `tests/curve/test_curve_io_free_example.py`.

`MockCurveSwapper` (inline in `test_curve_equivalence.py`) reimplements `_get_d` and `_get_y`. Replace with `CurveStableswapPool` + `FakeCurveDataProvider`, or call `stableswap_get_d`/`stableswap_get_y` directly from `degenbot.calculations.stableswap`.

`FakeCurveDataProvider` must be extracted from `test_curve_io_free_example.py` to `tests/fakes/curve_data_provider.py` so it can be imported by the consumers in this step (`test_to_hop_state_pair_selection.py`, `test_curve_legacy_equivalence.py`, `test_curve_equivalence.py`).

```python
# After
pool = CurveStableswapPool(
    address="0xcurve",
    tokens=(dai, usdc),  # FakeToken works — verified
    a_coefficient=1000,
    fee=4_000_000,
    admin_fee=5_000_000_000,
    balances=(10_000_000 * 10**18, 10_000_000 * 10**6),
    data_provider=FakeCurveDataProvider(block_timestamp=1_700_000_000),
)
```

### Step 8: Replace inline constant-product math with production imports

Multiple test files re-implement the constant-product swap formula. Replace all inline versions with `from degenbot.uniswap.v2_functions import constant_product_calc_exact_in`.

### Step 9: Delete buggy `calculations/constant_product.py`

This is production code deletion, isolated for clarity. `get_amount_out` is buggy — it diverges from the Solidity `UniswapV2Library.getAmountOut` formula for small `amount_in` values due to intermediate integer truncation in the fee deduction step (`amount_in * fee.numerator // fee.denominator` truncates to zero when `amount_in` is small, leaving the fee unapplied). `constant_product_calc_exact_in` uses the same single-expression form as Solidity (`amount_in * (fee.denominator - fee.numerator)`) and matches on-chain behavior in all cases. The buggy function is only imported by `tests/test_calculations.py` (5 test methods in `TestGetAmountOut` + 1 cross-validation in `TestSolidlyCalcExactInVolatile`); redirect these to `constant_product_calc_exact_in`.

### Step 10: Delete dead test harness code + adopt shared constants

`tests/helpers/bot_test_harness_prototype.py` (346 lines) has zero test-suite imports. It contains `FakeDataSource`, `RecordingDataSource`, and Curve-specific test data types that were superseded by `FakeCurveDataProvider`. Delete entirely.

Replace hardcoded address strings in V2/V3 conftest files with imports from `tests/constants.py`.

### Design decisions

- **FakeToken inherits AddressComparable vs. standalone**: AddressComparable provides `__eq__`/`__hash__`/`__lt__` by address, matching `Erc20Token`'s behavior. This makes `FakeToken` compatible as a dict key and set member in the same way as `Erc20Token`, and eliminates `FakeToken`'s custom `__eq__` that uses `hasattr` duck-typing.
- **`# type: ignore` on FakeToken args vs. widen constructor annotations**: Production pool constructors declare `token0: Erc20Token` (V2/V3/V4) or `tokens: Sequence[Erc20Token]` (Curve). Widening to include `FakeToken` would pollute the public API with a test type. Using `# type: ignore[arg-type]` keeps the change local to test files. This is acceptable because Python doesn't enforce annotations at runtime and FakeToken provides all attributes that production pools access (`.address`, `.chain_id`, `.decimals`, `.symbol`, `.name`). A committed canary test in Slice 1 (constructing each production pool class with FakeToken) will catch any future attribute drift.
- **FakeToken `__eq__` semantic change is safe**: Current `__eq__` uses `hasattr(other, "address")` duck-typing which would match any object with `.address` (including pools). `AddressComparable.__eq__` restricts matching to `AddressComparable` instances, which is correct — a token should never equal a pool. Existing `AddressComparable` on `Erc20Token` already permits `Erc20Token("0x1") == UniswapV2Pool("0x1")` (same address), so this is not a new concern. The Slice 1 interoperability test covers the positive cases (FakeToken↔FakeToken, FakeToken↔Erc20Token, dict-key interchangeability, hash compatibility).
- **Keep `FakeV2Pool`/`FakeV3Pool` in `tests/fakes/pools.py`**: These minimal protocol-fakes (20 lines each) capture `external_update` calls — they serve as test spies, not mock math engines. They remain useful for testing log decoders and registry code that doesn't need real pool math. Do not delete.
- **Extract `FakeCurveDataProvider` to `tests/fakes/curve_data_provider.py`**: Currently defined inside `test_curve_io_free_example.py` and not importable by other test files. Step 7 consumers need it. Move to shared location; the original `test_curve_io_free_example.py` imports from the new location.
- **Builder test inner fakes (`FakeDbRow`, etc.) not consolidated**: `tests/builders/test_v3_builder_base.py` and `test_v4_builder_base.py` define ~15 inner classes each for `extract_db_values` and `load_tick_snapshot` tests. These are inherently local to each test method (different field values per test case) and are the correct pattern for single-use objects. No consolidation.
- **`MockV3PoolWithCache` in tick cache tests kept as-is**: This is a structural mock for the cache API, not divergent pool math.
- **Legacy test files (archive/, cvxpy tests)**: Low priority. These test deprecated code paths. Migrate only if they fail.
- **Delete `calculations/constant_product.py`**: `get_amount_out` is buggy (diverges from Solidity for small `amount_in` due to intermediate truncation in fee deduction) and unused by production code. Only imported by `tests/test_calculations.py`. Redirect its 6 test references to `constant_product_calc_exact_in`, which matches on-chain behavior in all cases. Isolated in its own slice (Slice 7) since this is production code deletion, not test refactoring.
- **Builder FakeProvider/FakeAsyncProvider not consolidated**: Audited — the five implementations use three genuinely different matching strategies (selector-keyed in `test_from_chain.py` and `test_async_v2_builder.py`; full-calldata-keyed in `test_async_erc20_builder_io.py`; `(to, data)`-tuple-keyed in `test_type_resolution.py`) plus one call-counting variant in `test_pool_io.py`. The total ~120 lines of duplication across 5 files does not justify a shared abstraction that would need to accommodate all four strategies. These fakes are thin I/O stubs, not divergent math.
- **Aave mock consolidation extracted to separate plan**: `MockScaledEvent`/`MockOperation` are structural factory helpers (not divergent math), and including them in this plan adds risk disproportionate to the benefit.

## Files Involved

**Primary:**
- `tests/fakes/tokens.py` — Add `AddressComparable` inheritance, `name` field; remove custom `__eq__`/`__hash__`
- `tests/fakes/pools.py` — Remove `MockLiquidityPool`, `MockV3LiquidityPool`; keep `FakeV2Pool`, `FakeV3Pool`, `FakeUniswapV4Pool`
- `tests/fakes/curve_data_provider.py` (new) — Extracted `FakeCurveDataProvider` from `test_curve_io_free_example.py`
- `tests/constants.py` (new) — Centralized mainnet addresses
- `tests/arbitrage/mock_pools.py` — Remove `MockV2Pool`, `MockV3Pool`, `MockV4Pool`, `build_mock_pool_from_state`, `build_mock_pools_from_fixture`, `create_cycle_with_mocks`, `cleanup_mock_patches`
- `tests/arbitrage/fake_pools.py` — Remove entirely; `FakeV3PoolWithTicks` and `FakeTickInfo` replaced by production `UniswapV3Pool` + `apply_liquidity_mapping_update`
- `tests/arbitrage/fake_curve_pool.py` — Remove entirely; replaced by `CurveStableswapPool` + `FakeCurveDataProvider`
- `tests/arbitrage/test_path/conftest.py` — Replace fake pools + fake state types with production pool construction
- `tests/arbitrage/integration/test_curve_equivalence.py` — Remove `MockCurveSwapper`, use production Curve math
- `tests/arbitrage/test_offline_integration.py` — Replace `OfflineErc20Token`, `OfflineV2Pool`, `OfflineV3Pool` with `FakeToken` + production pool constructors; rewrite test functions against production pools with proper V2/V3 update types
- `tests/helpers/bot_test_harness_prototype.py` — Delete entirely (346 lines, zero test-suite imports)
- `src/degenbot/calculations/constant_product.py` — Delete; buggy and unused by production
- `tests/test_calculations.py` — Replace `from degenbot.calculations.constant_product` import; redirect 6 test references to `constant_product_calc_exact_in`

**Secondary:**
- `tests/arbitrage/integration/test_uniswap_lp_cycle.py` — Replace `MockLiquidityPool`/`MockV3LiquidityPool` usage with production constructors; use `UniswapV2PoolExternalUpdate` for V2 pools and `UniswapV3PoolExternalUpdate` (with `liquidity`, `sqrt_price_x96`, `tick` fields) for V3 pools
- `tests/arbitrage/test_mock_pools.py` — Delete mock-factory tests (`TestBuildMockPoolFromState`, `TestBuildMockPoolsFromFixture`, `TestUniswapLpCycleIntegration`); delete `TestFakeToken` (subsumed by `TestGetAmountOut` redirect); remaining V2/V3/V4 construction scenarios already covered by `test_v2_offline.py`, `test_v3_offline.py`, `test_v2_pool_io_free.py`, `test_v3_pool_io_free.py`
- `tests/arbitrage/test_fake_curve_pool.py` — Delete tests for fake-only behavior (construction validation, metapool construction, `FakeCurvePoolState`); rewrite solver integration tests (`TestSimulationFunctions`) against production `CurveStableswapPool`; remaining scenarios already covered by `test_curve_io_free_example.py` and `test_to_hop_state_pair_selection.py`
- `tests/arbitrage/test_optimizers/test_cvxpy_optimizer.py` — Replace `MockLiquidityPool` with `UniswapV2Pool`
- `tests/arbitrage/test_optimizers/test_cvxpy_multipool.py` — Replace `MockLiquidityPool` with `UniswapV2Pool`
- `tests/arbitrage/test_optimizer_comparison.py` — Replace `MockV2Pool` with `UniswapV2Pool`; remove inline `build_mock_pools_from_fixture`
- `tests/arbitrage/integration/test_v3_only_legacy_equivalence.py` — Replace locally-defined `FakeV3Pool` (NOT the one from `mock_pools.py` — this is a separate class with exact V3 math via `v3_virtual_reserves`) with `UniswapV3Pool`; eliminate the file's own `_make_patched_legacy_cycle` helper and its 4 `unittest.mock.patch` calls (production pools pass `isinstance(pool, Pool.__value__)` natively, making patches on `_validate_pools` and `_pool_is_viable` unnecessary)
- `tests/arbitrage/test_optimizers/test_fake_v3_pools.py` — Rewrite against production `UniswapV3Pool`
- `tests/arbitrage/verify_legacy_equivalence.py` — Replace inline `_v2_exact_out` with `constant_product_calc_exact_in`; update `FakeUniswapV2Pool`/`FakeV2PoolState` imports from conftest.py to production pool construction (handles in Slice 4)
- `tests/curve/test_to_hop_state_pair_selection.py` — Replace `FakeCurveStableswapPool` with production `CurveStableswapPool`
- `tests/arbitrage/integration/test_curve_legacy_equivalence.py` — Replace `FakeCurveStableswapPool` with production `CurveStableswapPool`
- `tests/curve/test_curve_io_free_example.py` — Import `FakeCurveDataProvider` from new shared location instead of defining inline
- `tests/arbitrage/test_offline_integration.py` — Replace `OfflineErc20Token`, `OfflineV2Pool`, `OfflineV3Pool` with `FakeToken` + production pool constructors; rewrite ~830 lines of test functions against production pools, using proper `UniswapV2PoolExternalUpdate`/`UniswapV3PoolExternalUpdate` types
- `tests/curve/test_curve_data_provider.py` — No change; `FakeProviderBackend` is a different abstraction from builder fakes and stays in place
- `tests/arbitrage/test_path/test_pool_adapter.py` — Replace `FakeAerodromeV2Pool` with production `AerodromeV2Pool`
- `tests/arbitrage/test_path/test_arbitrage_path.py` — Replace `FakeAerodromeV2Pool` with production `AerodromeV2Pool`
- `tests/uniswap/v2/conftest.py` — Use `tests.constants` + shared token fixtures
- `tests/uniswap/v3/conftest.py` — Same
- `tests/arbitrage/test_optimizers/archive/test_v2_v3_optimizer.py` — Replace `MockV2Pool`/`MockV3Pool` (low priority, archived)

**No change needed:**
- `tests/fakes/subscribers.py` — Proper test double for `Subscriber` protocol
- `tests/fakes/web3.py` — I/O boundary fakes for `ProviderAdapter` seam
- `tests/curve/detection/fake_provider.py`, `tests/curve/detection/fake_w3.py` — Builder/probing I/O boundary fakes
- `tests/builders/test_v3_builder_base.py`, `tests/builders/test_v4_builder_base.py` — Inner fakes for DB row shapes; correct pattern for single-use test doubles
- `tests/arbitrage/test_optimizers/test_v3_tick_cache.py` — `MockV3PoolWithCache` is a structural mock for the cache API, not pool math
- `src/degenbot/arbitrage/_legacy/_uniswap_multipool_cycle_testing.py` — `FakeToken`/`FakePool` stubs for cvxpy problem construction, not pool math
- `tests/curve/test_curve_data_provider.py` — `FakeProviderBackend` wraps a different abstraction (ProviderBackend) than the builder `FakeProvider`/`FakeAsyncProvider` (ProviderAdapter); not in scope for consolidation
- `tests/types/test_pool_protocols.py` — `FakePoolSimulation`/`FakeArbitragePool` are protocol conformance tests, not mock pool math
- `tests/arbitrage/test_optimizers/test_solver_integration.py` — Does not use any mock pool classes; orthogonal to this plan
- `tests/arbitrage/generator/` — `FixtureFactory`/`ArbitrageCycleFixture` are still used by cvxpy tests and other consumers after mock_pools.py deletion; no dead code risk

## Implementation Order

### Slice 1: Upgrade FakeToken + add test constants

1. Add `AddressComparable` as base class, `name` field to `FakeToken`; remove custom `__eq__`/`__hash__`
2. Add test verifying `FakeToken` ↔ `Erc20Token` substitutability (equality by address, hash compatibility, dict-key interchangeability)
3. Add canary test constructing each production pool class with `FakeToken` arguments (+ `# type: ignore[arg-type]`), ensuring future attribute drift is caught
4. Create `tests/constants.py` with centralized mainnet addresses (WETH, WBTC, DAI, USDC, factory addresses)
5. Run: `just test-python` — all existing tests must pass (FakeToken used broadly)

Note: `OfflineErc20Token` replacement is deferred to Slice 2 alongside `OfflineV2Pool`/`OfflineV3Pool` replacement. Replacing the token type in Slice 1 while `OfflineV2Pool.__init__` still declares `token0: OfflineErc20Token` creates a transient broken state that would require also updating the OfflineV2Pool constructor — pointless since the entire class is deleted in Slice 2.

### Slice 2: Replace MockLiquidityPool, MockV3LiquidityPool, OfflineV2Pool, OfflineV3Pool

1. Migrate `tests/arbitrage/integration/test_uniswap_lp_cycle.py` off `MockLiquidityPool`/`MockV3LiquidityPool` to production constructors. Use `UniswapV2PoolExternalUpdate` (reserves) for V2 pools and `UniswapV3PoolExternalUpdate` (liquidity, sqrt_price_x96, tick) for V3 pools — the update types are not interchangeable.
2. Migrate `tests/arbitrage/test_optimizers/test_cvxpy_optimizer.py` and `test_cvxpy_multipool.py` off `MockLiquidityPool`
3. Migrate `tests/arbitrage/test_offline_integration.py` — replace `OfflineErc20Token` with `FakeToken`, replace `OfflineV2Pool`/`OfflineV3Pool` with production constructors. Rewrite ~830 lines of test functions against production pools using proper update types. Delete `OfflineErc20Token`, `OfflineV2Pool`, `OfflineV3Pool` classes.
4. Remove `MockLiquidityPool` and `MockV3LiquidityPool` from `tests/fakes/pools.py`
5. Run: `just test-python`

### Slice 3: Replace MockV2Pool, MockV3Pool, MockV4Pool + fake_pools.py

Note: `MockV2Pool.calculate_tokens_out_from_tokens_in` uses `float` arithmetic (fee_multiplier = 1 - float(self.fee)) while MockV3Pool uses approximate sqrt-price math. Both produce incorrect outputs vs. production pools. Tests asserting on exact mock outputs will need updated expected values to match production math.

1. Migrate `tests/arbitrage/test_mock_pools.py` — delete mock-factory tests (`TestBuildMockPoolFromState`, `TestBuildMockPoolsFromFixture`, `TestUniswapLpCycleIntegration`, `TestFakeToken`); remaining V2/V3/V4 construction scenarios already covered by offline/io-free test suites
2. Migrate `tests/arbitrage/test_optimizer_comparison.py` off `MockV2Pool`; remove inline `build_mock_pools_from_fixture`
3. Migrate `tests/arbitrage/integration/test_v3_only_legacy_equivalence.py` — this file defines its **own** `FakeV3Pool` (not imported from `mock_pools.py`) that uses exact V3 math via `v3_virtual_reserves`. It also defines its own `_make_patched_legacy_cycle` helper with 4 `unittest.mock.patch` calls. When replaced with `UniswapV3Pool`, the patches on `_validate_pools` and `_pool_is_viable` become unnecessary because production `UniswapV3Pool` passes the legacy cycle's `isinstance(pool, Pool.__value__)` check natively. Rewrite using production pool constructors; eliminate all `unittest.mock.patch` calls.
4. Migrate `tests/arbitrage/test_optimizers/test_fake_v3_pools.py` off `FakeV3PoolWithTicks`
5. Remove `tests/arbitrage/mock_pools.py` (MockV2Pool, MockV3Pool, MockV4Pool, create_cycle_with_mocks, cleanup_mock_patches, build_mock_pool_from_state, build_mock_pools_from_fixture)
6. Remove `tests/arbitrage/fake_pools.py` (FakeV3PoolWithTicks, FakeTickInfo, TickRangeDefinition, create_two_range_v3_pool)
7. Run: `just test-python`

### Slice 4: Replace test_path/conftest.py fake pools + FakeAerodromeV2Pool consumers

1. Replace `FakeV2PoolState`/`FakeCLPoolState` with production state types
2. Replace `FakeUniswapV2Pool` with `UniswapV2Pool`
3. Replace `FakeConcentratedLiquidityPool` with `UniswapV3Pool`
4. Replace `FakeAerodromeV2Pool` with `AerodromeV2Pool` — also migrate consumers in `test_pool_adapter.py` and `test_arbitrage_path.py`
5. Delete `FakeCamelotPool` (dead code — zero uses outside its definition)
6. Update helper functions (`_make_v2_pool`, `_make_v3_pool`, etc.); remove `_make_camelot_pool` if present
7. Migrate `tests/arbitrage/verify_legacy_equivalence.py` — this file imports `FakeUniswapV2Pool` and `FakeV2PoolState` from conftest.py; update to use production pool construction (must happen in this slice, not deferred — the conftest.py replacements break its imports)
8. Run: `just test-python`

### Slice 5: Replace FakeCurveStableswapPool and MockCurveSwapper

1. Extract `FakeCurveDataProvider` from `tests/curve/test_curve_io_free_example.py` to `tests/fakes/curve_data_provider.py`; update `test_curve_io_free_example.py` to import from new location
2. Migrate `tests/arbitrage/test_fake_curve_pool.py` — delete fake-only tests (construction validation, metapool, `FakeCurvePoolState`); rewrite solver integration tests (`TestSimulationFunctions`) against production `CurveStableswapPool`
3. Migrate `tests/arbitrage/integration/test_curve_equivalence.py` off `MockCurveSwapper` to production `CurveStableswapPool` or `stableswap_get_d`/`stableswap_get_y`
4. Migrate `tests/curve/test_to_hop_state_pair_selection.py` off `FakeCurveStableswapPool`
5. Migrate `tests/arbitrage/integration/test_curve_legacy_equivalence.py` off `FakeCurveStableswapPool`
6. Remove `tests/arbitrage/fake_curve_pool.py`
7. Run: `just test-python`

### Slice 6: Replace inline constant-product math with production imports

1. Replace inline `_v2_exact_out` and constant-product re-implementations in test files with imports from `constant_product_calc_exact_in`
2. Run: `just test-python`

### Slice 7: Delete buggy `calculations/constant_product.py`

This is production code deletion, isolated in its own slice.

1. Delete `src/degenbot/calculations/constant_product.py`; redirect `tests/test_calculations.py` (6 test references: 5 in `TestGetAmountOut` + 1 in `TestSolidlyCalcExactInVolatile`) to `v2_functions.constant_product_calc_exact_in`
2. Run: `just test-python`

### Slice 8: Delete dead code + adopt shared constants

1. Delete `tests/helpers/bot_test_harness_prototype.py`
2. Replace hardcoded address strings in V2/V3 conftest files with imports from `tests/constants.py`
3. Run: `just test-python`

### Slice 9: Validate and clean up

1. Run `just lint` + `just test-all`
2. Run `just test-rust-python` — verify hop state data flowing through Rust extension is unchanged
3. Verify no remaining imports of deleted mock classes
4. Move this plan to `plans/completed/`
5. Update `plans/README.md`

## Testing

### Per-slice test runs

Each slice runs `just test-python`. No compatibility period needed — each slice replaces mocks entirely in one step. Slice 8 additionally runs `just test-rust-python` to verify the Rust optimizer cache receives identical hop state data from production pools.

### New unit tests

- Slice 1: Add `FakeToken` ↔ `Erc20Token` interoperability test (equality by address, `hash()` compatibility, dict-key interchangeability) — this validates the `AddressComparable` migration which changes `FakeToken`'s equality semantics. Also add canary test constructing each production pool class with `FakeToken` to catch future attribute drift.

All other slices rewrite existing test fixtures and assertions to use production pools. The test coverage of production code increases by definition — we're replacing mock math with real math.

### Integration tests

- `tests/arbitrage/integration/test_uniswap_lp_cycle.py` — covers V2/V3 pool construction + `UniswapLpCycle` interaction
- `tests/arbitrage/integration/test_v3_only_legacy_equivalence.py` — covers V3-only ArbitragePath vs legacy equivalence
- `tests/arbitrage/integration/test_curve_legacy_equivalence.py` — covers Curve ArbitragePath vs legacy equivalence
- `tests/curve/test_curve_io_free_example.py` — already validates `CurveStableswapPool` + `FakeCurveDataProvider` pattern

### Coverage gaps from mock-file deletion

When deleting mock-testing-their-own-mocks files, the following scenarios are already covered by production test suites:

| Deleted test | Scenario | Covered by |
|---|---|---|
| `test_mock_pools.py` → `TestMockV2Pool` | V2 construction, swap calculation, state override, viability | `tests/uniswap/v2/test_v2_offline.py`, `test_v2_pool_io_free.py` |
| `test_mock_pools.py` → `TestMockV3Pool`/`TestMockV4Pool` | V3/V4 construction, hashability | `tests/uniswap/v3/test_v3_offline.py`, `test_v3_pool_io_free.py` |
| `test_v3_only_legacy_equivalence.py` → locally-defined `FakeV3Pool` (exact V3 math) | V3 legacy cycle equivalence with `_make_patched_legacy_cycle` (4 `unittest.mock.patch` calls) | Production `UniswapV3Pool` passes `isinstance` check natively, eliminating the need for patches; legacy cycle's other patched methods (`_pool_is_viable`, `_pre_calculation_check`, `_build_swap_amounts`) need evaluation on a case-by-case basis |
| `verify_legacy_equivalence.py` → imports `FakeUniswapV2Pool`/`FakeV2PoolState` from conftest.py | Legacy ↔ new path equivalence | Coupled to conftest.py internals; will break when Slice 4 replaces conftest classes |
| `test_mock_pools.py` → `TestBuildMockPoolFromState`/`TestBuildMockPoolsFromFixture` | Factory dispatch | Deleted with the factory — no production equivalent needed |
| `test_mock_pools.py` → `TestUniswapLpCycleIntegration` | Legacy cycle with mock pools | Deleted — legacy-specific pattern; production pools satisfy the cycle interface directly |
| `test_fake_curve_pool.py` → `TestFakeCurvePoolConstruction` | 2-coin, 3-coin, validation | `tests/curve/test_curve_io_free_example.py` |
| `test_fake_curve_pool.py` → `TestCurveMath` | D calculation, swaps, round-trip | `tests/curve/test_curve_stableswap_pool.py:test_get_d`, `test_curve_io_free_example.py` |
| `test_fake_curve_pool.py` → `TestHopStateGeneration` | Direction, swap_fn, fields | `tests/curve/test_to_hop_state_pair_selection.py` (rewritten in Slice 5) |
| `test_fake_curve_pool.py` → `TestSimulationFunctions` | `_simulate_path`, `_simulate_mixed_path` | **Rewrite** against production `CurveStableswapPool` — no other coverage |
| `test_fake_curve_pool.py` → `TestSimulationResult` | Address-based swap | `tests/curve/test_curve_stableswap_pool.py` (fork-based) |
| `test_fake_curve_pool.py` → `TestMetapoolSupport` | Construction with base_pool | `test_curve_io_free_example.py:test_curve_metapool_with_data_provider` |
| `test_fake_curve_pool.py` → `TestStateOverride` | Override in `to_hop_state` | `test_to_hop_state_pair_selection.py` (rewritten in Slice 5) |

## Benefits

- **Depth**: Eliminates shallow mock interfaces that duplicate production logic. Tests exercise the real code path end-to-end.
- **Leverage**: One `FakeToken` class replaces 6+ mock pool classes + `OfflineErc20Token`. One `FakeCurveDataProvider` replaces 2 Curve math re-implementations.
- **Locality**: Test setup goes from "construct mock, smash 8 attributes, apply 4 mock.patch calls" to "call production constructor with test data".
- **Accuracy**: MockV2Pool's `float` arithmetic and MockV3Pool's approximate sqrt-price math are replaced by production integer-precision calculations. No more passing tests that would fail against real chain data.
- **Single source of truth**: Token addresses and factory constants flow from `tests/constants.py` and `src/degenbot/constants.py` instead of being duplicated across 40+ call sites.
- **Bug removal**: Deleting `calculations/constant_product.py` eliminates a function that diverges from the on-chain Solidity formula for small `amount_in` values.

## Risks

- **FakeToken not isinstance-compatible with Erc20Token**: Mitigated by `AddressComparable` inheritance and runtime verification. Production pool constructors only access `.address`, `.chain_id`, `.decimals`, `.symbol` — all provided by FakeToken. The `# type: ignore[arg-type]` comments are explicit and localized to test files.
- **FakeToken equality semantics change with AddressComparable**: Current `__eq__` uses `hasattr(other, "address")` duck-typing (matches any object with `.address`, including pools). `AddressComparable.__eq__` restricts matching to `AddressComparable` instances. This is the correct behavior — a token should never equal a pool — and is consistent with how `Erc20Token` already works via `AddressComparable`. Mitigated by the Slice 1 canary test.
- **Legacy cycle tests (cvxpy) may need MockV2Pool's float-swapped interface**: These tests use `calculate_tokens_out_from_tokens_in` which doesn't exist as a standalone method on `UniswapV2Pool` (production pools use `simulate_swap`). Mitigated by showing that the legacy cycle's `_build_swap_amounts` can use `simulate_swap` instead, or by keeping a thin adapter. If cvxpy tests are in `archive/`, low priority.
- **V3 tick data tests depend on FakeV3PoolWithTicks's tick construction**: Mitigated by using `apply_liquidity_mapping_update` (commit 1783c742) which is the same pure function the production pool uses internally. Verified to produce identical results.
- **Curve test divergence**: `FakeCurveStableswapPool` produces materially different results from `CurveStableswapPool` for the same inputs. Verified: for `a_coefficient=2000`, `_get_dy(0,1,1000e18)` returns 999000000 (fake) vs 999599950 (production) — a 0.06% divergence caused by fee truncation in XP-space computation (fee of 0.04% × 999 XP units = 0 due to integer truncation). Production math works in raw-balance space and avoids this. Tests asserting on exact FakeCurveStableswapPool outputs will need updated expected values to match production math. This is a feature, not a bug — the production math matches on-chain behavior.
- **Rust optimizer cache receives different data**: If `to_hop_state()` output differs subtly between fake and production pools (e.g., `BoundedProductHop` field values), the Rust extension's `IntHopState` cache could receive different data. Mitigated by running `just test-rust-python` in Slice 8.
- **Builder FakeProvider/FakeAsyncProvider not consolidated**: Audited — the five implementations use three genuinely different matching strategies (selector-keyed in `test_from_chain.py`/`test_async_v2_builder.py`; full-calldata-keyed in `test_async_erc20_builder_io.py`; `(to, data)`-tuple-keyed in `test_type_resolution.py`) plus a call-counting variant in `test_pool_io.py`. Total ~120 lines of duplication does not justify a shared abstraction. These fakes are thin I/O stubs, not divergent math.
- **`test_curve_data_provider.py`'s `FakeProviderBackend` is a different abstraction**: `FakeProviderBackend` wraps a `ProviderBackend` (selector-keyed raw call dispatch + block metadata), while the builder fakes wrap `ProviderAdapter`/async `ProviderAdapter`. These are different abstraction levels and must NOT be consolidated. `FakeProviderBackend` stays in `test_curve_data_provider.py` and is out of scope.
- **`verify_legacy_equivalence.py` coupled to conftest.py internals**: This file imports `FakeUniswapV2Pool` and `FakeV2PoolState` from `test_path/conftest.py`. When Slice 4 replaces these with production pools, `verify_legacy_equivalence.py` must be updated in the same slice. Slice 4 step 7 calls this out explicitly — it must not be deferred.

## Relationship to Other Plans

- **Plan 039** (DyCalculator seam): Completed. This plan is a downstream consumer — `FakeCurveStableswapPool`'s duplicated math is no longer needed because `CurveStableswapPool` + `FakeCurveDataProvider` provides a clean I/O-free test path.
- **Plan 045** (DyCalculationInputs): Completed. This plan eliminates `FakeCurveStableswapPool` which had its own `pool`-coupled math.
- **Plan 049** (CurveDataProviderImpl): Completed. This plan leverages the `FakeCurveDataProvider` seam to test Curve pools without mocks.
- **Plan 066** (type_resolution dedup): Completed. Unrelated.
- **Plan 072** (build_managed_pool extraction): Completed. Unrelated.
- **Plan 014** (Async REPL): Active. Orthogonal — no overlap.
- **Commit 1783c742** (replace mock pool helpers with pure function): This plan is the direct continuation. That commit replaced `MockV3LiquidityPool` and `MockV4LiquidityPool` in `src/degenbot/cli/pool.py` with `apply_liquidity_mapping_update`. This plan extends the same pattern to all remaining mock pool usage and broadens the scope to all divergent test doubles.

## Status

[x] Slice 1: Upgrade FakeToken + add test constants + canary test
[x] Slice 2: Replace MockLiquidityPool, MockV3LiquidityPool, OfflineV2Pool, OfflineV3Pool
[x] Slice 3: Replace MockV2Pool, MockV3Pool, MockV4Pool + fake_pools.py
[x] Slice 4: Replace test_path/conftest.py fake pools + FakeAerodromeV2Pool consumers
[x] Slice 5: Replace FakeCurveStableswapPool and MockCurveSwapper
[x] Slice 6: Replace inline constant-product math with production imports
[x] Slice 7: Delete buggy calculations/constant_product.py
[x] Slice 8: Delete dead code + adopt shared constants
[x] Slice 9: Validate and clean up
