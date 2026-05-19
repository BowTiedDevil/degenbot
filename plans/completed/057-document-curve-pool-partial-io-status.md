# Plan 057: Document Curve Pool's Partial I/O Status

## Overview

Make explicit the I/O boundary of `CurveStableswapPool` by renaming I/O-performing internal methods to signal they may perform on-chain calls, adding class-level documentation about which pool variants require I/O at calculation time, and aligning the ADR-001 "I/O-free" claim with the reality that Curve pools are I/O-free at *construction time* but not at *calculation time*.

## Problem

### Deletion test

If you deleted the `_data_provider` attribute and all its call sites from `CurveStableswapPool`, the pool could not compute `get_dy()` for crypto, live-admin, lending, or metapool pools — it would only work for plain STANDARD pools with no A ramping and no metapool. The I/O is genuinely needed for those variants. The issue is not that I/O exists, but that it's hidden behind a method name (`_build_calculation_inputs`) that doesn't signal I/O.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| `_build_calculation_inputs` may perform I/O but its name doesn't say so | `curve_stableswap_liquidity_pool.py` line ~490 | A reader tracing `get_dy()` → `_build_calculation_inputs()` → `_data_provider.D()` discovers I/O by accident, not by design. V2/V3/V4 pool calculation paths have zero I/O; a reader familiar with those pools would not expect I/O inside a pool's swap calculation. |
| ADR-001 claims all pools are "I/O-free" | `docs/adr/ADR-001-io-free-pools.md` | The ADR states: "All pool types (Curve, V2, V3, V4, Aerodrome, Camelot) are I/O-free." This is true at construction time for all pools, and true at calculation time for V2/V3/V4/Aerodrome/Camelot — but false at calculation time for non-plain Curve pools. The ADR should distinguish these two I/O boundaries. |
| `MissingCurveData` exceptions scattered through calculation methods | `get_dy()`, `calc_token_amount()`, `calc_withdraw_one_coin()`, `_a()` | 8 `raise MissingCurveData(...)` sites in the pool class signal "I/O is needed but not available." These are runtime I/O requirements encoded as error handling, not as type-level guarantees. |
| No distinction between construction-I/O-free and calculation-I/O-free in code | `CurveStableswapPool` class docstring | The class docstring says "Constructed from pre-fetched data only" — true for immutable pool parameters (A, fee, tokens), but `get_dy()` may still call `_data_provider` for per-block on-chain data (D, gamma, price_scale, lending rates, admin balances). |

## Solution

### Step 1: Rename `_build_calculation_inputs` to `_resolve_calculation_inputs_via_io`

The current name `_build_calculation_inputs` suggests pure data assembly. The new name `_resolve_calculation_inputs_via_io` makes the I/O possibility explicit. Update all callers.

```python
# Before
inputs = self._build_calculation_inputs(block_number, override_state)

# After
inputs = self._resolve_calculation_inputs_via_io(block_number, override_state)
```

Similarly rename `_build_metapool_inputs` → `_resolve_metapool_inputs_via_io`.

### Step 2: Add class-level documentation of I/O boundary

Add a class-level docstring section to `CurveStableswapPool` that documents the I/O boundary:

```python
class CurveStableswapPool(...):
    """
    A Curve V1 (StableSwap) pool.

    Constructed from pre-fetched data only. Use Bot.build_pool() to fetch from chain.

    I/O boundary:
        Construction is I/O-free — all immutable parameters (A, fee, tokens, strategies)
        are provided by the builder. However, get_dy() and related calculation methods
        may call CurveDataProvider methods for per-block on-chain data when needed.

        I/O-free at calculation time for: plain pools (SwapStyle.STANDARD, RAW_BALANCE)
        I/O-required at calculation time for: lending pools, crypto pools, live-admin pools,
        metapools (virtual_price, redemption_price, D, gamma, price_scale, admin_balances,
        lending rates, block timestamps for A ramping).

        The data_provider must be available for calculation-time I/O. Pools constructed
        without a data_provider can only perform calculations that don't require on-chain data.
    """
```

### Step 3: Update ADR-001 to distinguish construction-time vs calculation-time I/O

Amend ADR-001's status section to clarify the I/O boundary:

```markdown
## I/O-Free Status by Pool Family

| Pool Family | Construction I/O-Free | Calculation I/O-Free | Notes |
|-------------|----------------------|----------------------|-------|
| V2/V3/V4/Aerodrome/Camelot | ✅ | ✅ | Builders fetch all data; pools are pure logic |
| Curve (plain, RAW_BALANCE) | ✅ | ✅ | Rate multipliers are static |
| Curve (lending/crypto/live-admin/metapool) | ✅ | ❌ | get_dy() may call CurveDataProvider for per-block data |
```

### Step 4: Add a `_requires_io` property to `CurveStableswapPool`

Add a boolean property that indicates whether this pool instance requires I/O at calculation time, based on its `PoolStrategies`:

```python
@property
def requires_io_at_calculation_time(self) -> bool:
    """Whether this pool may call data_provider during swap calculations."""
    if self._strategies.swap_style in {
        SwapStyle.CRYPTO,
        SwapStyle.LIVE_ADMIN,
        SwapStyle.LIVE_ADMIN_DYNAMIC,
        SwapStyle.LIVE_ADMIN_DYNAMIC_PRECISION,
        SwapStyle.LIVE_ADMIN_ORACLE,
    }:
        return True
    if self._strategies.lending_rate_style != LendingRateStyle.NONE:
        return True
    if self.base_pool is not None:
        return True  # metapools need virtual_price
    if any([
        self.future_a_coefficient is not None,
        self.initial_a_coefficient is not None,
    ]):
        return True  # A ramping needs block_timestamp
    return False
```

This property enables callers to check I/O requirements without tracing through calculation code.

### Design decisions

- **Rename, don't refactor the I/O pattern**: This plan does not change the I/O pattern itself — `CurveStableswapPool` will still call `_data_provider` at calculation time. The goal is to make the I/O boundary *visible*, not to eliminate it. The I/O is genuinely needed for non-plain Curve variants; eliminating it would require eagerly fetching all per-block data at state-update time, which is a different architectural choice (potentially a future plan).

- **Property rather than method for `requires_io`**: A `@property` allows callers to check I/O requirements as an attribute read, consistent with other boolean properties on pool classes. No performance concern — the check is a few enum comparisons.

- **Don't change `DyCalculationInputs` interface**: The calculator-facing interface remains unchanged. The I/O boundary is between the pool and the data provider, not between the pool and the calculators.

## Files Involved

**Primary:**
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — rename methods, add docstring, add `requires_io_at_calculation_time` property
- `docs/adr/ADR-001-io-free-pools.md` — add I/O status table

**Secondary:**
- `src/degenbot/curve/types.py` — no change (already has `SwapStyle`, `LendingRateStyle`)

**No change needed:**
- `src/degenbot/curve/calculators/` — calculators are pure consumers of `DyCalculationInputs`
- `src/degenbot/curve/data_provider_impl.py` — provider's interface unchanged
- `src/degenbot/uniswap/` — V2/V3/V4 pools are already fully I/O-free at calculation time

## Implementation Order

### Slice 1: Rename methods + add docstring

1. Rename `_build_calculation_inputs` → `_resolve_calculation_inputs_via_io`
2. Rename `_build_metapool_inputs` → `_resolve_metapool_inputs_via_io`
3. Add class-level I/O boundary documentation to `CurveStableswapPool`
4. Run: `just test-python` — expect all tests green (rename-only change)

### Slice 2: Add `requires_io_at_calculation_time` property

1. Implement the `requires_io_at_calculation_time` property on `CurveStableswapPool`
2. Write tests verifying the property returns correct values for each pool variant
3. Run: `just test-python` — expect all tests green

### Slice 3: Update ADR-001

1. Add I/O status table to ADR-001
2. Update the "I/O-free" claim to be precise about construction-time vs calculation-time
3. Run: `just test-python` — expect all tests green (documentation change)

### Slice 4: Validate and clean up

1. Run `just lint` + `just test-all`
2. Update `src/degenbot/curve/CONTEXT.md` if needed — add `calculation-time I/O` term
3. Verify ADR-001 renders correctly

## Testing

### Per-slice test runs

Each slice runs `just test-python`. All slices should be green — this plan makes I/O visible, not new.

### New unit tests

```python
# tests/curve/test_curve_pool_io_boundary.py


def test_plain_pool_no_calculation_io():
    """STANDARD swap style pool with no lending/metapool/A ramping doesn't require I/O."""
    ...


def test_lending_pool_requires_calculation_io():
    """CTOKEN/YTOKEN lending pool requires I/O for rate fetching."""
    ...


def test_crypto_pool_requires_calculation_io():
    """CRYPTO swap style pool requires I/O for D, gamma, price_scale."""
    ...


def test_metapool_requires_calculation_io():
    """Metapool requires I/O for virtual_price of base pool."""
    ...


def test_a_ramping_requires_calculation_io():
    """Pool with A ramping needs block_timestamp via data provider."""
    ...


def test_resolve_inputs_method_name_signals_io():
    """Method name contains 'via_io' to signal I/O possibility."""
    ...
```

### Integration tests

No changes needed. Existing tests that construct pools with `data_provider=None` and call `get_dy()` for plain pools work fine. Tests that call `get_dy()` for crypto/lending/metapool pools already use `FakeCurveDataProvider`.

## Benefits

- **Locality**: The I/O boundary is documented at the class level, not scattered across 8 `MissingCurveData` exception sites. A maintainer reads the class docstring and understands the I/O requirements immediately.
- **Depth**: The `requires_io_at_calculation_time` property provides one boolean answer for "does this pool need I/O at calculation time?" instead of requiring the caller to trace through strategy enums and pool configuration.
- **ADR accuracy**: ADR-001's claim of "I/O-free" becomes precise and honest. Future architectural reviews won't be misled.

## Risks

- **ADRs should be immutable**: Amending ADR-001 violates the convention that ADRs are record-only. Mitigation: add an "Amendment" section rather than editing the original decision. The original decision (I/O-free at construction time) is unchanged; the amendment clarifies the calculation-time boundary.
- **Method rename is a breaking change for subclassers**: Any code that calls `pool._build_calculation_inputs()` directly would break. Mitigation: the method is private (underscore prefix), so external callers shouldn't exist. Verify with grep.
- **False sense of precision**: The `requires_io_at_calculation_time` property is a static check based on `PoolStrategies`. It may return `False` for a pool that doesn't currently need I/O but could if called with specific `block_identifier` arguments. Mitigation: document this caveat in the property docstring.

## Relationship to Other Plans

- **Plan 013** (Curve StableSwap I/O-Free Architecture): Completed. This plan clarifies the boundary of that architecture — I/O-free at construction, I/O-possible at calculation for non-plain variants.
- **Plan 040** (Curve Data Provider): Completed. Established the `CurveDataProvider` seam. This plan documents the I/O boundary of that seam.
- **Plan 054** (Consolidate Curve On-Chain Caches): Complementary. Plan 054 organizes the cache fields that store I/O results; this plan documents when I/O is triggered.
- **Plan 055** (Delete Deprecated Fetcher Protocols): Orthogonal. That plan deletes dead protocol types; this plan documents the live I/O boundary.

## Status

[x] Slice 1: Rename methods + add docstring
[x] Slice 2: Add `requires_io_at_calculation_time` property
[x] Slice 3: Update ADR-001
[x] Slice 4: Validate and clean up
