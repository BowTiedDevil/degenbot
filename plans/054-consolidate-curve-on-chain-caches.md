# Plan 054: Consolidate Curve Pool On-Chain Caches

## Overview

Replace the 10 individual `BoundedCache[BlockNumber, T]` fields in `CurveStableswapPool` with a single `CurveOnChainCache` object that owns all per-block on-chain data caches and provides accessor methods with the try-cache → call-provider → store pattern. Delete the dead code block after the unconditional `return inputs` in `_build_calculation_inputs`.

## Problem

### Deletion test

If you deleted the 10 individual `BoundedCache` fields and their accessor methods, the caching policy (bounded size, block-keyed, provider-fallback) would need to be re-implemented somewhere. The pool's `get_dy()` path genuinely needs per-block caching for on-chain data that changes every block (virtual price, admin balances, D, gamma, price_scale, lending rates, etc.). So the caches are earning their keep — but they should be concentrated in one module, not scattered as 10 independent fields in the pool class.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| 10 `BoundedCache` fields with identical patterns | `curve_stableswap_liquidity_pool.py` lines 192–228 | Each field follows the same pattern: `_cached_X: BoundedCache[BlockNumber, T] = BoundedCache(max_items=state_cache_depth)`, then accessor `_get_X(block_number)` with try-cache → provider → store → return |
| 8 identical accessor methods | `_get_scaled_redemption_price`, `_get_base_cache_updated`, `_get_base_virtual_price`, `_get_virtual_price`, `_get_admin_balances`, plus implicit caches in `_build_calculation_inputs` for `_cached_contract_D`, `_cached_gamma`, `_cached_price_scale` | Each is ~10 lines of identical boilerplate with different data-provider method calls |
| Dead code after `return inputs` | Lines 637–647: unreachable `_get_scaled_redemption_price` body duplication | After `_build_calculation_inputs` returns `inputs`, there's a dead block that duplicates `_get_scaled_redemption_price` logic. This appears to be a copy-paste artifact from an earlier refactor. |
| 1159-line pool class | `curve_stableswap_liquidity_pool.py` total | 10 cache fields + 8 accessor methods + `_build_calculation_inputs` (140 lines) account for ~300 lines. Extracting the cache shrinks the pool by ~25%. |
| `_build_calculation_inputs` mixes I/O orchestration with data assembly | Lines ~490–636 | The method does: resolve timestamp, resolve amp, resolve rates, compute XP, handle crypto I/O, handle live-admin I/O. Two of those (crypto, live-admin) involve cache-then-provider patterns that should live in the cache object. |

## Solution

### Step 1: Create `CurveOnChainCache` class

New file `curve/on_chain_cache.py`. The cache owns all `BoundedCache` instances and provides accessor methods that encapsulate the try-cache → call-provider → store → return pattern.

```python
# Before: scattered across CurveStableswapPool.__init__
self._cached_rates: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(max_items=depth)
self._cached_scaled_redemption_price: BoundedCache[BlockNumber, int] = BoundedCache(max_items=depth)
self._cached_virtual_price: BoundedCache[BlockNumber, int] = BoundedCache(max_items=depth)
self._cached_admin_balances: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(max_items=depth)
# ... 6 more

# After: single object
self._on_chain_cache = CurveOnChainCache(
    data_provider=self._data_provider,
    pool_address=self.address,
    max_items=state_cache_depth,
)
```

The `CurveOnChainCache` provides methods like:

```python
class CurveOnChainCache:
    def __init__(self, *, data_provider, pool_address, max_items): ...
    
    def virtual_price(self, block_number: int) -> int: ...
    def base_virtual_price(self, block_number: int) -> int: ...
    def base_cache_updated(self, block_number: int) -> int: ...
    def admin_balances(self, block_number: int) -> tuple[int, ...]: ...
    def contract_D(self, block_number: int) -> int: ...
    def gamma(self, block_number: int) -> int: ...
    def price_scale(self, block_number: int) -> tuple[int, ...]: ...
    def scaled_redemption_price(self, block_number: int) -> int: ...
    
    def get_or_fetch(self, cache_attr: str, block_number: int, fetcher: Callable[[], T]) -> T:
        """Generic try-cache → fetcher → store → return."""
        ...
```

### Step 2: Migrate pool accessor methods to use `CurveOnChainCache`

Replace each `_get_X(block_number)` method body with a call to `self._on_chain_cache.X(block_number)`. The pool methods become one-liners or are inlined.

For methods with additional logic (e.g., `_get_virtual_price` which checks `base_cache_updated` expiry), the logic moves into the cache method. The pool's `base_cache_updated` and `base_virtual_price` instance attributes become properties of the cache.

### Step 3: Delete dead code after `return inputs`

Lines 637–647 after `_build_calculation_inputs`'s unconditional `return inputs` are unreachable dead code that duplicates `_get_scaled_redemption_price`. Delete.

### Step 4: Simplify `_build_calculation_inputs`

The crypto-specific and live-admin-specific I/O blocks in `_build_calculation_inputs` use inline `try/except KeyError` against individual caches. After Step 2, these become:

```python
# Before
try:
    d_val = self._cached_contract_D[block_number]
except KeyError:
    d_val = self._data_provider.D(block_number)
    self._cached_contract_D[block_number] = d_val

# After
d_val = self._on_chain_cache.contract_D(block_number)
```

### Design decisions

- **Cache holds `data_provider` reference**: The cache needs the provider for misses. This is consistent with the existing pattern where the pool holds `_data_provider` and delegates to it. The cache doesn't import I/O — it calls the same `CurveDataProvider` protocol.
- **`base_cache_updated`/`base_virtual_price` as cache properties**: Currently these are instance attributes on the pool (set during `_get_base_cache_updated` and pre-populated in `__init__`). They are effectively cache-latest-value mirrors, identical to what a `BoundedCache` knows. The cache can expose `latest_base_virtual_price` as a property.
- **Generic `get_or_fetch` helper**: Most accessors follow the identical pattern. A generic method avoids duplicating it 8 times in the cache class itself.
- **Don't change `DyCalculationInputs`**: The cache refactor is internal to the pool. The `DyCalculationInputs` frozen dataclass is the calculator-facing interface and stays unchanged.

## Files Involved

**Primary:**
- `src/degenbot/curve/on_chain_cache.py` — new file
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — replace 10 BoundedCache fields with single `_on_chain_cache`, delete 8 accessor methods, delete dead code, simplify `_build_calculation_inputs`

**Secondary:**
- `src/degenbot/curve/__init__.py` — may need to export `CurveOnChainCache` if tests import it directly

**No change needed:**
- `src/degenbot/curve/types.py` — `DyCalculationInputs` and `CurveDataProvider` unchanged
- `src/degenbot/curve/calculators/` — calculators read `DyCalculationInputs`, not pool internals

## Implementation Order

### Slice 1: Create `CurveOnChainCache` with generic helper

1. Create `src/degenbot/curve/on_chain_cache.py` with `CurveOnChainCache` class
2. Implement `get_or_fetch` generic helper and 8 accessor methods
3. Handle the `virtual_price` method's `base_cache_updated` expiry logic
4. Write unit tests for the cache object using `FakeCurveDataProvider`
5. Run: `just test-python` — expect all tests green (cache not yet wired)

### Slice 2: Wire `CurveOnChainCache` into pool, delete old accessors

1. Replace 10 `BoundedCache` fields in `CurveStableswapPool.__init__` with `self._on_chain_cache = CurveOnChainCache(...)`
2. Replace each `_get_X(block_number)` call with `self._on_chain_cache.X(block_number)`
3. Delete the old `_get_X` method bodies from the pool class
4. Move `base_cache_updated` and `base_virtual_price` instance attribute maintenance into the cache
5. Run: `just test-python` — expect all tests green

### Slice 3: Simplify `_build_calculation_inputs` and delete dead code

1. Replace inline `try/except KeyError` cache-then-provider patterns with `self._on_chain_cache.X(block_number)` calls
2. Delete the dead code block at lines 637–647
3. Run: `just test-python` — expect all tests green

### Slice 4: Validate and clean up

1. Run `just lint` + `just test-all`
2. Update `src/degenbot/curve/CONTEXT.md` — add `CurveOnChainCache` term
3. Verify line count reduction: pool class should be ~900 lines (from 1159)

## Testing

### Per-slice test runs

Each slice runs `just test-python`. Slices 1–2 should be green at each step.

### New unit tests

```python
# tests/curve/test_on_chain_cache.py


def test_cache_miss_calls_provider():
    """On cache miss, the cache calls data_provider and stores the result."""
    ...


def test_cache_hit_skips_provider():
    """On cache hit, the cache returns stored value without calling provider."""
    ...


def test_virtual_price_expiry():
    """Virtual price uses base_cache_updated to determine cache validity."""
    ...


def test_max_items_bounded():
    """Cache respects max_items and evicts oldest entries."""
    ...
```

### Integration tests

Existing Curve pool tests (`tests/curve/`) exercise `get_dy()` which calls through the caches. These provide integration coverage without modification.

## Benefits

- **Locality**: All on-chain caching policy lives in one module (`on_chain_cache.py`). Changing cache depth, eviction strategy, or provider-fallback logic requires editing one file, not 8 methods.
- **Depth**: The `CurveOnChainCache` presents a deep interface — 8 accessor methods behind a single object — replacing 10 independent fields that callers had to understand individually.
- **Leverage**: The `get_or_fetch` generic helper means adding a new cached on-chain value is a 3-line method, not a 10-line try/except block.
- **Dead code removal**: The unreachable code after `return inputs` is eliminated by this refactor.

## Risks

- **Attribute access pattern change**: Currently, `self.base_cache_updated` and `self.base_virtual_price` are instance attributes set during cache population. Moving them into the cache object changes `pool.base_cache_updated` to `pool._on_chain_cache.latest_base_cache_updated` or similar. Any external access to these attributes must be preserved. Mitigation: add read-through properties on the pool that delegate to the cache.
- **Pickle compatibility**: `CurveStableswapPool._pickle_drops` includes `_data_provider` but not individual cache fields. After this refactor, the pickled object needs to drop `_on_chain_cache` (which holds the provider). Verify the pickle/reconstruct cycle works.

## Relationship to Other Plans

- **Plan 013** (Curve StableSwap I/O-Free Architecture): Completed. Established the `CurveDataProvider` seam. This plan is a follow-on — consolidating the caches that the data provider made possible.
- **Plan 040** (Curve Data Provider): Completed. Collapsed 13 fetcher callbacks into 1 `CurveDataProvider`. This plan further consolidates the provider-backed caches into one object.
- **Plan 054** (this plan): Independent of active plans 014, 048, and 053.

## Status

[ ] Slice 1: Create `CurveOnChainCache` with generic helper
[ ] Slice 2: Wire into pool, delete old accessors
[ ] Slice 3: Simplify `_build_calculation_inputs`, delete dead code
[ ] Slice 4: Validate and clean up
