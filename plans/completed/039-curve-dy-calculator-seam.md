# Plan 039: Extract Swap-Style Computation Behind a DyCalculator Seam

## Overview

Replace the 11-branch `match self._strategies.swap_style` inside `CurveStableswapPool.get_dy()` and the 3-branch `match self._strategies.metapool_rate_style` / `match self._strategies.metapool_underlying_style` in `_get_dy_underlying()` with injectable calculator objects. Each `SwapStyle` / `MetapoolRateStyle` / `MetapoolUnderlyingStyle` value maps to a frozen dataclass that encapsulates that formula.

**Before extracting calculators, extract the invariant-solver methods (`_get_d`, `_get_y`, `_get_y_d`, `_newton_y`) as standalone pure functions in `calculations/stableswap.py`.** This creates two seams instead of one: the calculator seam (which formula) and the invariant-solver seam (which numerical algorithm). Calculators call pure functions, not pool methods. The pool's depth increases: `get_dy()` becomes a thin dispatcher, and the math that was hidden behind `self` references becomes testable with plain integers.

This deepens the Curve StableSwap pool's shallowest seam: a single method that exposes 14 branches of variant computation to every reader.

## Files Involved

**Primary:**
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — replace 14 match branches in `get_dy()` and `_get_dy_underlying()` with delegation to injected calculators; extract `_get_d`, `_get_y`, `_get_y_d`, `_newton_y` to pure functions; pool class shrinks by ~50%
- `src/degenbot/calculations/stableswap.py` — new: standalone pure functions for Curve invariant solving (`stableswap_get_d`, `stableswap_get_y`, `stableswap_get_y_d`, `stableswap_newton_y`)
- `src/degenbot/curve/types.py` — add `DyCalculator` protocol
- `src/degenbot/curve/_pool_strategies.py` — map addresses to calculator instances instead of (or alongside) enum values
- `src/degenbot/curve/calculators/` (new) — one module per calculator family: `standard.py`, `rate_adjusted.py`, `crypto.py`, `live_admin.py`, `metapool.py`

**Secondary:**
- `src/degenbot/builders/curve_pool_builder.py` — construct calculator instances and pass via `PoolStrategies`
- `src/degenbot/curve/stableswap_pool_state.py` — no change (state stays where it is; calculator extraction is orthogonal to state extraction)
- `tests/curve/` — new per-calculator unit test files; new pure-function tests in `tests/calculations/test_stableswap.py`; update pool construction in existing tests
- `src/degenbot/curve/CONTEXT.md` — document calculator terms

## Problem

### Deletion test

If you deleted `get_dy()`, ~600 lines of truly distinct computation would need to be re-implemented across every caller. The method is earning its keep. But the complexity is fully visible — a reader must scroll past 300 lines of irrelevant `SwapStyle` branches to understand any one path. The 11 `SwapStyle` branches and 3 `MetapoolUnderlyingStyle` branches are already identified by strategy enums (Plan 026), but the enums are used as dispatch labels inside a single method, not as seams behind which implementation hides.

If you deleted the enum and replaced it with a calculator object, the same computation still needs to happen — but it would be testable in isolation, and `get_dy()` would be a 5-line dispatcher.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| **11-branch match in get_dy** | `get_dy()` lines 522–847 | Understanding one swap formula requires scrolling past 300 lines of unrelated formulas |
| **3-branch match in _get_dy_underlying** | `_get_dy_underlying()` lines 848–1068 | Metapool calculation paths are similarly interleaved |
| **No isolated testing** | `get_dy()` | Testing the CYTOKEN path requires constructing a full pool with 13 fetcher arguments |
| **Future variants = new branches** | `get_dy()` | Every new Curve contract version adds another `case SwapStyle.XXX` branch to the method |
| **Mixed concerns per branch** | `get_dy()` each branch | Rate resolution, live-balance fetching, admin-balance subtraction, invariant solving, fee calculation, and rate conversion are interleaved in each branch |
| **Invariant solvers are pool methods** | `_get_y`, `_get_d`, `_get_y_d`, `_newton_y` | These are direct ports of Vyper contract functions — numerical algorithms with no I/O. Making them pool methods was a historical accident, not an architectural choice. They access `self` only for data that should be parameters: `n_coins`, `A_PRECISION`, `d_variant`/`y_variant`/`yd_variant`, `block_timestamps`, `address` (error messages). |

### The `_dynamic_fee` local function

The `_dynamic_fee` helper is defined inside `get_dy()` as a closure. It's used only by `LIVE_ADMIN_DYNAMIC` and `LIVE_ADMIN_DYNAMIC_PRECISION` branches. It should move into the `LiveAdminDynamicCalculator`.

## Solution

### Phase A: Extract Invariant Solvers to Pure Functions

Extract `_get_d`, `_get_y`, `_get_y_d`, `_newton_y` as standalone pure functions in `calculations/stableswap.py`. These are direct ports of Vyper contract logic — numerical algorithms that should never have been pool methods.

#### Step A1: Extract `stableswap_get_d` to `calculations/stableswap.py`

The current `_get_d(self, _xp, _amp)` accesses:
- `self._strategies.d_variant` — variant dispatch → parameter
- `self._tokens` → `len(self._tokens)` → `n_coins` parameter
- `self.A_PRECISION` → constant parameter

Pure function signature:

```python
def stableswap_get_d(
    xp: Sequence[int],
    amp: int,
    n_coins: int,
    a_precision: int,
    d_variant: DVariant,
) -> int:
    """
    Solve for the Curve stableswap invariant D using modified Newton's method.
    Direct port of Vyper contract logic.
    """
```

The variant dispatch (`match self._strategies.d_variant`) moves inside the function. The `calc_d`, `calc_dp`, etc. local functions remain as nested definitions within `stableswap_get_d` — they are Vyper contract logic, not a separate module concern.

#### Step A2: Extract `stableswap_get_y` to `calculations/stableswap.py`

The current `_get_y(self, i, j, x, xp)` accesses:
- `self._tokens` → `n_coins` parameter
- `self.A_PRECISION` → parameter
- `self._strategies.y_variant` → parameter
- `self._block_timestamps[self.update_block]` → caller resolves `timestamp` and passes it
- `self._a(timestamp=...)` → caller resolves `amp` and passes it. This is critical: `_a()` is the I/O boundary (it calls `self._timestamp_fetcher`), so the calculator resolves amp *before* calling the pure function.
- `self._get_d(xp, amp)` → pure function call `stableswap_get_d(xp, amp, n_coins, a_precision, d_variant)`

Pure function signature:

```python
def stableswap_get_y(
    i: int,
    j: int,
    x: int,
    xp: Sequence[int],
    amp: int,
    n_coins: int,
    a_precision: int,
    y_variant: YVariant,
    d_variant: DVariant,
) -> int:
    """
    Calculate x[j] if one makes x[i] = x.
    Solves quadratic equation iteratively. Direct port of Vyper contract logic.
    """
    # ... calls stableswap_get_d internally ...
```

**Key design decision:** `stableswap_get_y` calls `stableswap_get_d` internally (it needs D as part of its iteration). This means it needs `d_variant` as well. The alternative is to pre-compute D and pass it in, but `_get_y` creates D as an intermediate value (`c = y = d = self._get_d(xp, amp)`) and doesn't return it. Keeping the D computation internal to `stableswap_get_y` matches the Vyper contract and avoids computing D twice when `_get_y` is called from a context that also needs D.

#### Step A3: Extract `stableswap_get_y_d` to `calculations/stableswap.py`

The current `_get_y_d(self, a, i, xp, d)` accesses:
- `self._tokens` → `n_coins` parameter
- `self._strategies.yd_variant` → parameter

D is already a parameter (unlike `_get_y` which computes D internally).

Pure function signature:

```python
def stableswap_get_y_d(
    a: int,
    i: int,
    xp: Sequence[int],
    d: int,
    n_coins: int,
    yd_variant: YDVariant,
) -> int:
    """
    Calculate y given A, xp, and D. Used by calc_token_amount and calc_withdraw_one_coin.
    Direct port of Vyper contract logic.
    """
```

#### Step A4: Extract `stableswap_newton_y` to `calculations/stableswap.py`

The current `_newton_y(self, ann, gamma, xp, d, token_index)` accesses:
- `self._tokens` → `n_coins` parameter
- `self.A_PRECISION` → `a_multiplier` parameter
- `self.address` → remove from error message (or pass as optional `pool_address` for diagnostics)

Pure function signature:

```python
def stableswap_newton_y(
    ann: int,
    gamma: int,
    xp: Sequence[int],
    d: int,
    token_index: int,
    n_coins: int,
    a_multiplier: int,
) -> int:
    """
    Calculate xp[i] given other balances and invariant D, using Newton's method.
    Used by crypto (volatile) Curve pools.
    """
```

#### Step A5: Pool methods become thin wrappers

Each pool method is replaced with a thin wrapper that resolves `self` state and delegates:

```python
def _get_y(self, i: int, j: int, x: int, xp: Sequence[int]) -> int:
    amp = self._a(
        timestamp=self._block_timestamps.get(self.update_block)
    ) // self.A_PRECISION
    return stableswap_get_y(
        i, j, x, xp, amp, len(self._tokens),
        self.A_PRECISION, self._strategies.y_variant,
        self._strategies.d_variant,
    )

def _get_d(self, _xp: Sequence[int], _amp: int) -> int:
    return stableswap_get_d(
        _xp, _amp, len(self._tokens),
        self.A_PRECISION, self._strategies.d_variant,
    )
```

Existing callers of `self._get_y(...)` and `self._get_d(...)` continue to work. The wrappers can be deprecated later once calculators are in place.

#### Step A6: Move `_reduction_coefficient` to `calculations/stableswap.py`

This is already a `@staticmethod` — it takes only `(x, fee_gamma, n_coins)`. Just relocate it from the pool class to the pure-function module (or leave it as a static import — no self access).

### Phase B: Extract DyCalculator Objects

#### Step B1: Define `DyCalculator` protocol

In `src/degenbot/curve/types.py`:

```python
from typing import Protocol

class DyCalculator(Protocol):
    """Calculates dy (output amount) for a Curve StableSwap swap.
    
    Calculators resolve data from the pool (amp, balances, rates) in a few lines,
    then call pure invariant-solver functions from calculations/stableswap.py.
    The pool parameter provides read-only access to caches and fetchers for
    data resolution only — the math is done by pure functions.
    """
    
    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        pool: CurveStableswapPool,
        block_number: int,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int: ...
```

The calculator receives the pool as a read-only reference for data resolution (balances, rate resolvers, caches, fetchers). The math — `stableswap_get_y`, `stableswap_get_d`, fee calculation, rate conversion — is done by pure functions. This keeps the calculator's effective interface narrow: it reads a few values from the pool, then does pure math.

**Why pass `pool` instead of a narrower "read-only state" interface?** Because each calculator needs a different subset of data, making a single narrow protocol useless. Instead, the calculator's effective interface is documented by what it actually accesses (verified in tests). The pool parameter is the pragmatic seam. If future friction warrants it, a narrower protocol can be extracted later — but only after calculators exist and we can see what they actually need.

#### Step B2: Define calculator dataclasses — one per SwapStyle

Each is a frozen dataclass implementing `DyCalculator`. The `calculate` method resolves data from the pool (5-10 lines), then calls pure functions for the math (5-10 lines).

```python
# src/degenbot/curve/calculators/standard.py
from degenbot.calculations.stableswap import stableswap_get_y

@dataclass(frozen=True, slots=True)
class StandardDyCalculator:
    """STANDARD: dy = xp[j] - y - 1, fee, then rate convert."""
    swap_style: SwapStyle = SwapStyle.STANDARD
    
    def calculate(self, i, j, dx, *, pool, block_number, override_state=None) -> int:
        pool_balances = override_state.balances if override_state else pool.balances
        rates = pool._resolve_rates(
            rates=pool.rate_multipliers,
            block_number=block_number,
            pool_balances=pool_balances,
        )
        xp = pool._xp(rates=rates, balances=pool_balances)
        x = xp[i] + (dx * rates[i] // pool.PRECISION)
        
        # Pure math — no pool access beyond this point
        amp = pool._a(timestamp=pool._block_timestamps.get(block_number))
        y = stableswap_get_y(
            i, j, x, xp, amp, len(pool.tokens),
            pool.A_PRECISION, pool._strategies.y_variant,
            pool._strategies.d_variant,
        )
        dy = xp[j] - y - 1
        fee = pool.fee * dy // pool.FEE_DENOMINATOR
        return (dy - fee) * pool.PRECISION // rates[j]
```

Notice the structure: **data resolution from pool (top half) → pure math (bottom half)**. The pool access is concentrated in the first few lines. The pure math section is identical regardless of whether the data came from a pool, a mock, or hardcoded integers.

All 11 calculators:

| Calculator | SwapStyle | Key difference |
|------------|-----------|----------------|
| `StandardDyCalculator` | `STANDARD` | dy - 1, fee, rate convert |
| `RateAdjustedDyCalculator` | `RATE_ADJUSTED` | rate convert before fee |
| `RateAdjustedNoOneDyCalculator` | `RATE_ADJUSTED_NO_ONE` | no -1 subtraction |
| `RawBalanceDyCalculator` | `RAW_BALANCE` | no rate conversion |
| `CryptoDyCalculator` | `CRYPTO` | Newton's method, dynamic fee, price_scale |
| `LiveAdminDyCalculator` | `LIVE_ADMIN` | live balances - admin balances |
| `LiveAdminDynamicDyCalculator` | `LIVE_ADMIN_DYNAMIC` | live balances - admin, offpeg dynamic fee |
| `LiveAdminDynamicPrecisionDyCalculator` | `LIVE_ADMIN_DYNAMIC_PRECISION` | precision multipliers for xp |
| `LiveAdminOracleDyCalculator` | `LIVE_ADMIN_ORACLE` | live balances - admin, oracle rates |
| `NoOneFeeRateDyCalculator` | `NO_ONE_FEE_RATE` | dy = xp[j] - y (no -1) |
| `CytokenDyCalculator` | `CYTOKEN` | fee inside rate conversion |

#### Step B3: Define metapool calculators

One calculator per `MetapoolRateStyle` value and one per `MetapoolUnderlyingStyle` value (matching the non-metapool pattern for consistency). Each eliminates the `match` inside the calculator — the strategy enum maps to exactly one calculator class.

| Calculator | Metapool Rate Style / Underlying Style |
|------------|----------------------------------------|
| `MetapoolPrecisionVpDyCalculator` | `PRECISION_VP` |
| `MetapoolRedemptionVpDyCalculator` | `REDEMPTION_VP` |
| `MetapoolStandardDyCalculator` | `STANDARD` |
| `MetapoolUnderlyingRedemptionDyCalculator` | `REDEMPTION` (underlying) |
| `MetapoolUnderlyingPrecisionVpDyCalculator` | `PRECISION_VP` (underlying) |
| `MetapoolUnderlyingStandardDyCalculator` | `STANDARD` (underlying) |

#### Step B4: Add calculator references to `PoolStrategies`

```python
@dataclasses.dataclass(slots=True, frozen=True)
class PoolStrategies:
    """Resolved calculation strategies for a Curve pool instance."""
    # ... existing enums for identity/introspection ...
    d_variant: DVariant = DVariant.STANDARD
    y_variant: YVariant = YVariant.STANDARD
    yd_variant: YDVariant = YDVariant.STANDARD
    swap_style: SwapStyle = SwapStyle.STANDARD
    metapool_rate_style: MetapoolRateStyle = MetapoolRateStyle.STANDARD
    metapool_underlying_style: MetapoolUnderlyingStyle = MetapoolUnderlyingStyle.STANDARD
    lending_rate_style: LendingRateStyle = LendingRateStyle.NONE
    
    # New: calculator instances
    dy_calculator: DyCalculator = field(default_factory=StandardDyCalculator)
    metapool_dy_calculator: MetapoolDyCalculator | None = None
    metapool_underlying_dy_calculator: MetapoolUnderlyingDyCalculator | None = None
```

The enum values remain for introspection (e.g., logging "this pool uses SwapStyle.RATE_ADJUSTED"). The calculator carries the actual implementation. Defaults ensure backwards compatibility — `PoolStrategies()` works for plain pools.

#### Step B5: Simplify `get_dy()` and `_get_dy_underlying()`

```python
def get_dy(self, i, j, dx, block_identifier=None, override_state=None) -> int:
    block_number = self._resolve_block_number(block_identifier)
    
    # Fetch and cache block timestamp for A ramping
    if block_number not in self._block_timestamps:
        if self._timestamp_fetcher is None:
            raise MissingCurveData(...)
        self._block_timestamps[block_number] = self._timestamp_fetcher(block_number)
    
    return self._strategies.dy_calculator.calculate(
        i, j, dx, pool=self, block_number=block_number, override_state=override_state
    )
```

#### Step B6: Update `_pool_strategies.py` to construct calculators

The address → `PoolStrategies` mapping constructs the appropriate calculator:

```python
def resolve_pool_strategies(pool_address: ChecksumAddress) -> PoolStrategies:
    base = _POOL_STRATEGIES.get(pool_address)
    if base is None:
        return PoolStrategies()  # defaults include StandardDyCalculator
    
    dy_calculator = _make_dy_calculator(base.swap_style)
    metapool_dy = _make_metapool_dy_calculator(base.metapool_rate_style) if base.metapool_rate_style != MetapoolRateStyle.STANDARD else None
    
    return dataclasses.replace(
        base,
        dy_calculator=dy_calculator,
        metapool_dy_calculator=metapool_dy,
    )

def _make_dy_calculator(swap_style: SwapStyle) -> DyCalculator:
    match swap_style:
        case SwapStyle.STANDARD:
            return StandardDyCalculator()
        case SwapStyle.RATE_ADJUSTED:
            return RateAdjustedDyCalculator()
        # ... etc
```

#### Step B7: Move `_dynamic_fee` into `LiveAdminDynamicDyCalculator`

The local `_dynamic_fee` closure currently defined inside `get_dy()` moves into the dynamic-fee calculators as a private method or standalone function in `calculations/stableswap.py`.

## Implementation Order

### Phase A: Pure function extraction (no behavior change)

1. **Create `calculations/stableswap.py`** with `stableswap_get_d` — copy body from `_get_d`, replace `self` access with parameters
2. **Add `stableswap_get_y`** — copy body from `_get_y`, replace `self` access, call `stableswap_get_d` internally
3. **Add `stableswap_get_y_d`** — copy body from `_get_y_d`, replace `self` access
4. **Add `stableswap_newton_y`** — copy body from `_newton_y`, replace `self` access
5. **Add `stableswap_reduction_coefficient`** — relocate from pool `@staticmethod`
6. **Rewire pool methods as thin wrappers** delegating to pure functions — green tests confirm identical behavior
7. **Add pure-function unit tests** in `tests/calculations/test_stableswap.py` — test each function with known-good inputs/outputs from existing integration tests

### Phase B: Calculator extraction (behavior change: dispatch via objects)

8. **Define `DyCalculator` protocol** in `curve/types.py`
9. **Create `calculators/` package** with calculator dataclasses — start with `StandardDyCalculator` (simplest, calls `stableswap_get_y`)
10. **Add `dy_calculator` field to `PoolStrategies`** (default = `StandardDyCalculator()`)
11. **Replace `STANDARD` branch** in `get_dy()` with `self._strategies.dy_calculator.calculate(...)` — green tests confirm the seam works
12. **Extract remaining 10 calculators**, one at a time, replacing each `case SwapStyle.XXX` branch — each extraction is a single focused commit with green tests
13. **Extract metapool calculators** from `_get_dy_underlying()` — 3+3 branches → 6 calculator dataclasses
14. **Update `_pool_strategies.py`** to construct calculator instances
15. **Update `CurvePoolBuilder.build()`** to pass calculator-armed `PoolStrategies`
16. **Create per-calculator unit tests** in `tests/curve/calculators/`
17. **Deprecate pool wrapper methods** (`_get_y`, `_get_d`, `_get_y_d`, `_newton_y`) if no remaining callers (or keep if other pool methods like `calc_token_amount` still use them)
18. **Update `CONTEXT.md`** with calculator terms

## Testing

### Pure-function unit tests (Phase A)

These need zero infrastructure — just integers:

```python
# tests/calculations/test_stableswap.py

def test_stableswap_get_d_convergence():
    xp = [1_000_000_000_000_000_000, 1_000_000_000_000_000_000]
    d = stableswap_get_d(xp, amp=1000, n_coins=2, a_precision=100, d_variant=DVariant.STANDARD)
    assert d == expected_from_contract

def test_stableswap_get_y_known_values():
    y = stableswap_get_y(
        0, 1, 2_000_000 * 10**18, xp,
        amp=1000, n_coins=2, a_precision=100,
        y_variant=YVariant.STANDARD, d_variant=DVariant.STANDARD,
    )
    assert y == expected_from_contract
```

Known-good inputs/outputs come from existing integration tests that call the pool methods with real RPC data.

### Calculator unit tests (Phase B)

Each calculator gets a dedicated test file. Tests construct the calculator directly and verify output:

```python
# tests/curve/calculators/test_standard_dy.py

def test_standard_dy_known_values():
    calc = StandardDyCalculator()
    result = calc.calculate(0, 1, dx=10**18, pool=fake_pool, block_number=18_000_000)
    assert result == expected_from_integration_test
```

The `fake_pool` is a real `CurveStableswapPool` constructed with `PoolStrategies(swap_style=SwapStyle.STANDARD, dy_calculator=StandardDyCalculator())` and `None` fetchers for paths the calculator won't exercise. The test documents what the calculator actually accesses on `pool`.

### Integration tests

All existing Curve tests pass unchanged — the pool's public interface is identical in both phases.

### Performance

No regression expected. The calculator `calculate()` method is called exactly where the old `match` branch was — one extra method call on the hot path, which is negligible compared to the invariant solve and the potential RPC fetch. The pure-function calls are identical to the current pool-method calls (same body, no overhead).

## Benefits

- **Two seams, not one:**
  - **DyCalculator seam** — which swap formula, testable in isolation
  - **Invariant-solver seam** — which numerical algorithm, testable with plain integers
- **Locality:** each swap formula is testable and debuggable in isolation — you don't need to construct a 30-argument pool just to trace the CYTOKEN path
- **Leverage:** `get_dy()` shrinks from ~600 lines of interleaved branches to ~15 lines of delegation. Future Curve variants add one class instead of one `case` branch
- **Pool class size:** `CurveStableswapPool` shrinks by ~50% (roughly 850 lines move: ~150 lines to `calculations/stableswap.py`, ~700 lines to calculator classes)
- **Testability at two levels:** pure functions with plain integers (fastest, zero infrastructure); calculators with fake pools (medium, documents pool access); integration with real RPC (existing tests, unchanged)
- **AI-navigability:** reading `get_dy()` now shows *where* to look (which calculator), not *what every calculator does*
- **Correctness guarantee:** the pure functions are Vyper contract ports. Standalone functions make it trivial to cross-reference against the original contract code

## Risks

- **`pool` parameter coupling:** calculators receive the whole pool object, which is broader than they need. Mitigated by: (1) the calculator structure forces pool access to the top (data resolution), then pure math with no pool access — accidental coupling is unlikely, (2) calculator tests document what they actually access, (3) calculators are internal implementation details, not public API. The pure-function seam is the guarantee: if a calculator calls `pool.some_method()` instead of a pure function, that's a code smell visible in review.
- **Pickle compatibility:** calculators are frozen dataclasses with no closures — they're picklable. The `PoolStrategies` dataclass needs `eq=False` on the calculator fields (or custom `__eq__`) since different calculator instances of the same type are functionally identical.
- **Performance (negligible):** one extra method call per `get_dy()` invocation. The invariant solve and potential RPC fetch dominate by orders of magnitude.
- **Partial migration risk:** both phases can be introduced incrementally. Phase A (pure function extraction) is mechanical — each function is a copy-paste-replace with a thin wrapper. Phase B (calculator extraction) proceeds one SwapStyle at a time; the `match` statement stays in `get_dy()` during migration. No "big bang" required.
- **`stableswap_get_y` calls `stableswap_get_d` internally:** this is correct per the Vyper contract. It means D may be computed twice when the caller also needs D (e.g., `calc_token_amount` calls `_get_d` and `_get_y` separately). This is the current behavior — `_get_y` already computes D internally. No regression.

## Relationship to Other Plans

- **Plan 026** (Strategy Objects): Complete. This plan deepens the architecture that Plan 026 created. Plan 026 replaced address dispatch with `SwapStyle` enum dispatch. This plan replaces enum dispatch with object dispatch — the next step in the deepening arc.
- **Plan 027** (Lending-Rate Fetcher Protocols): Complete. The calculators use the `LendingRateFetcher` that Plan 027 introduced, via `pool._resolve_rates()`.
- **Plan 029** (Variant Group Externalization): Complete. The `DVariant`/`YVariant`/`YDVariant` enums that drive `_get_d()`, `_get_y()`, `_get_y_d()` are now parameters to the pure functions instead of `self._strategies` accesses. The variant dispatch moves from pool methods to `calculations/stableswap.py`, which is its natural home.
- **Plan 041** (Elevate Curve State Mixin): Orthogonal and complementary. This plan extracts calculation; Plan 041 extracts state. They can proceed in either order.
- **Plan 040** (Consolidate Curve Fetcher Callbacks): Complementary. If fetchers are consolidated into a `CurveDataProvider`, the calculator's `pool` parameter can be narrowed to a data provider + state view. But this plan doesn't depend on 040 — the `pool` reference works with either 13 individual fetchers or one data provider.

## Scope Decision

This plan extracts `get_dy()` and `_get_dy_underlying()` calculators AND the invariant-solver functions they call. The remaining pool methods that call `_get_y_d` (`calc_token_amount`, `calc_withdraw_one_coin`) continue to use the pool wrapper methods during Phase B. Deprecating those wrappers is a cleanup step after all callers migrate to pure functions.

## Status: Complete

### Phase A: Complete ✅
- Extracted 5 pure functions to `calculations/stableswap.py`: `stableswap_get_d`, `stableswap_get_y`, `stableswap_get_y_d`, `stableswap_newton_y`, `stableswap_reduction_coefficient`
- Pool methods rewired as thin wrappers
- 24 new pure-function tests in `tests/calculations/test_stableswap.py`

### Phase B: Complete ✅
- **11 SwapStyle calculators** across 3 files:
  - `calculators/standard.py` — Standard, RateAdjusted, RateAdjustedNoOne, RawBalance, NoOneFeeRate, Cytoken
  - `calculators/crypto.py` — Crypto
  - `calculators/live_admin.py` — LiveAdmin, LiveAdminDynamic, LiveAdminDynamicPrecision, LiveAdminOracle (+ extracted `_dynamic_fee` helper)
- **3 MetapoolRateStyle calculators** in `calculators/metapool.py` — PrecisionVp, RedemptionVp, Standard
- **3 MetapoolUnderlyingStyle calculators** in `calculators/metapool.py` — Redemption, PrecisionVp, Standard
- **DyCalculator protocol** in `curve/types.py` with `calculate(i, j, dx, *, pool, block_number, override_state) -> int`
- **PoolStrategies** extended with `dy_calculator`, `metapool_dy_calculator`, `metapool_underlying_dy_calculator` fields
- **`get_dy()`** reduced to ~20-line dispatcher (metapool → calculator, non-metapool → calculator, lazy fallback)
- **`_get_dy_underlying()`** reduced to ~15-line dispatcher
- **`_get_dy_metapool()`** removed (replaced by metapool calculators)
- **`_dynamic_fee`** closure extracted from `get_dy()` to `live_admin.py`
- Lazy fallback for pools constructed without calculators (direct instantiation)
- All 153 Curve + calculations tests pass
- Pool class: 1698 → 999 lines (**-41%**)
- New code: 983 lines across `calculations/stableswap.py` (446) + `calculators/` (537)
