# Plan 077: Extract PerBlockCache from CurveStableswapPool

## Overview

Extract the per-block on-chain cache subsystem (9 `BoundedCache` fields and 9 `_get_cached_*` accessor methods) from `CurveStableswapPool` into a `PerBlockCache` class that owns the cache fields and the accessor logic. The pool holds one `_cache: PerBlockCache` instead of nine fields and nine methods. The 2 side-effect mirrors are eliminated in favor of self-contained dependency resolution in `get_cached_virtual_price()`. The cache-expiry logic concentrates in one place.

## Problem

### Deletion test

If you deleted the 9 `_cache_*` fields, 9 `_get_cached_*` methods, and 2 `_base_*_value` mirrors from `CurveStableswapPool`, the pool would lose its ability to fetch per-block on-chain data. The complexity would need to resurface either in `get_dy()` (making it even longer) or in the callers (requiring them to manage cache invalidation). The cache earns its keep, but it's in the wrong module — it leaks across the pool's seam.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| 9 cache fields on the pool | `__init__`, lines ~218–256 | Constructor is dominated by cache setup; hard to find pool config among the cache fields |
| 9 accessor methods, each 10–15 lines | Lines ~350–530 | Each follows the same try-cache→call-provider→store→return pattern; no variation justifies 9 separate methods on the pool body |
| 2 side-effect mirrors | `_base_cache_updated_value`, `_base_virtual_price_value` | `_get_cached_base_cache_updated` and `_get_cached_base_virtual_price` update these mirrors as side effects; `_get_cached_virtual_price` reads them. The coupling is implicit — a caller must know to call the first two before the third. This already caused one bug (commit `a403a37`: missing mirror update on cache-hit path). |
| 1369-line pool class | Entire file | The pool is the largest class in the codebase; the cache subsystem accounts for ~200 lines that are a self-contained concern |
| Pickle drops/reconstructs | `_pickle_drops` includes `_data_provider` | Cache fields with `BoundedCache` can be pickled, but the `_data_provider` closure cannot — the pickle concern is split across cache fields and provider |

## Solution

### Step 1: Define `PerBlockCache`

Create a new plain class (not a dataclass — it's a stateful service with mutating accessors) that owns all `BoundedCache` instances and their accessor methods. The class receives a `CurveDataProvider` (or None) at construction, an `address`, `base_pool_is_set` (bool snapshot), and `state_cache_depth`.

Key difference from the pool's current code: **the side-effect mirrors are eliminated.** Each accessor follows the pure try/cache/store/return pattern with no shared mutable state. `get_cached_virtual_price()` internally resolves its own dependencies by calling `get_cached_base_cache_updated()` and `get_cached_base_virtual_price()` instead of reading mirrors set by earlier calls. This makes the class correct by construction — no call-ordering contract.

```python
# curve/per_block_cache.py

class PerBlockCache:
    """Owns all per-block on-chain caches for a CurveStableswapPool.

    Each accessor implements the try-cache → call-provider → store → return
    pattern. No accessor mutates state read by another accessor.
    get_cached_virtual_price() resolves its own dependencies internally,
    eliminating the need for side-effect mirrors or call-ordering contracts.

    The pool holds one instance of this class instead of nine
    separate BoundedCache fields.
    """

    # Moved from CurveStableswapPool — only used by get_cached_virtual_price
    BASE_CACHE_EXPIRES: int = 10 * 60  # 10 minutes in seconds

    def __init__(
        self,
        data_provider: CurveDataProvider | None,
        address: ChecksumAddress,
        base_pool_is_set: bool,
        state_cache_depth: int,
    ) -> None:
        self._data_provider = data_provider
        self._address = address
        self._base_pool_is_set = base_pool_is_set

        # Cache fields
        self._cache_block_timestamps: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_contract_D: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_gamma: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_price_scale: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_admin_balances: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_scaled_redemption_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_virtual_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_base_cache_updated: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )
        self._cache_base_virtual_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth
        )

    # ── Accessors (all follow the same try/cache/store/return pattern) ──

    def get_cached_block_timestamp(self, block_number: BlockNumber) -> int: ...
    def get_cached_contract_d(self, block_number: BlockNumber) -> int: ...
    def get_cached_gamma(self, block_number: BlockNumber) -> int: ...
    def get_cached_price_scale(self, block_number: BlockNumber) -> tuple[int, ...]: ...
    def get_cached_admin_balances(self, block_number: BlockNumber) -> tuple[int, ...]: ...
    def get_cached_scaled_redemption_price(self, block_number: BlockNumber) -> int: ...

    def get_cached_base_cache_updated(self, block_number: BlockNumber) -> int:
        """Pure try/cache/store/return. No mirror side effect."""
        ...

    def get_cached_base_virtual_price(self, block_number: BlockNumber) -> int:
        """Pure try/cache/store/return. No mirror side effect."""
        ...

    def get_cached_virtual_price(self, block_number: BlockNumber) -> int:
        """Resolve virtual price with self-contained expiry logic.

        For metapools, internally resolves base_cache_updated and
        base_virtual_price (calling their accessors) to determine
        whether to use the cached base_virtual_price or fetch live.
        No side-effect mirrors — all dependency resolution is inline.
        """
        with contextlib.suppress(KeyError):
            return self._cache_virtual_price[block_number]

        base_virtual_price: int
        if not self._base_pool_is_set:
            # Non-metapool: fetch directly
            if self._data_provider is None:
                raise MissingCurveData(...)
            base_virtual_price = self._data_provider.virtual_price(block_number)
        else:
            # Metapool expiry logic (matches contract's _vp_rate_ro())
            base_cache_updated = self.get_cached_base_cache_updated(block_number)
            block_timestamp = self.get_cached_block_timestamp(block_number)
            if block_timestamp > base_cache_updated + self.BASE_CACHE_EXPIRES:
                # Cache expired — fetch live virtual_price
                if self._data_provider is None:
                    raise MissingCurveData(...)
                base_virtual_price = self._data_provider.virtual_price(block_number)
            else:
                # Cache valid — resolve base_virtual_price from its own cache
                base_virtual_price = self.get_cached_base_virtual_price(block_number)

        self._cache_virtual_price[block_number] = base_virtual_price
        return base_virtual_price

    # ── Pickle support ──

    def __getstate__(self) -> dict[str, Any]:
        state = self.__dict__.copy()
        state["_data_provider"] = None  # can't pickle closures
        return state

    def __setstate__(self, state: dict[str, Any]) -> None:
        self.__dict__.update(state)
        # _data_provider already set to None by __getstate__
```

### Step 2: Replace pool fields with single `_cache: PerBlockCache`

In `CurveStableswapPool.__init__`, replace the 9 `_cache_*` field assignments and 2 mirror fields with:

```python
self._cache = PerBlockCache(
    data_provider=self._data_provider,
    address=self.address,
    base_pool_is_set=self.base_pool is not None,
    state_cache_depth=state_cache_depth,
)
```

Construction pre-population simplifies from two calls to one:

```python
# Before:
with contextlib.suppress(Exception):
    self._get_cached_base_cache_updated(block_number=state_block)
with contextlib.suppress(Exception):
    self._get_cached_base_virtual_price(block_number=state_block)

# After:
with contextlib.suppress(Exception):
    self._cache.get_cached_virtual_price(block_number=state_block)
```

### Step 3: Replace pool accessor calls with cache delegation

Replace all `self._get_cached_X(block_number)` calls with `self._cache.get_cached_X(block_number)` throughout the pool. This affects:

- `_resolve_calculation_inputs_via_io()` — 4–5 calls
- `_resolve_metapool_inputs_via_io()` — 2 calls
- `_a()` — 1 call (block_timestamp)
- `calc_token_amount()` — 1 call
- `calc_withdraw_one_coin()` — 1 call
- `_get_y()` — 1 call
- Constructor pre-population — simplified to 1 call (see Step 2)

### Step 4: Update pickle support

- `PerBlockCache` implements `__getstate__`/`__setstate__`: drops `_data_provider`, reconstructs as `None`. All 9 `BoundedCache` instances survive pickle — calculations on the other side of the pipe hit cache, no provider calls needed.
- Pool keeps `_data_provider` in its own `_pickle_drops`/`_pickle_reconstructs` for the 6 uncached I/O call sites that remain on the pool.

### What stays on the pool

The following I/O call sites remain on `CurveStableswapPool` — they are uncached and take pool-specific parameters. They are not cache-concern and do not move to `PerBlockCache`:

| Method | Provider call | Reason it stays |
|--------|--------------|-----------------|
| `_resolve_block_number` | `block_number()` | Block identifier coercion, not caching |
| `_resolve_calculation_inputs_via_io` (lending) | `lending_rates()` | Uncached I/O, not try/cache/store/return |
| `_resolve_calculation_inputs_via_io` (live-admin) | `token_balance()` | Uncached I/O, pool-specific params |
| `_resolve_rates` | `lending_rates()` | Uncached I/O, same as lending path |
| `_fetch_token_balance` | `token_balance()` | Uncached, takes token/address params the cache doesn't own |
| `_fetch_token_total_supply` | `token_total_supply()` | Uncached, takes token param the cache doesn't own |

These are territory for Plan 078 (InputResolver).

### Design decisions

- **Mirror-free design**: The 2 side-effect mirrors (`_base_cache_updated_value`, `_base_virtual_price_value`) are eliminated. `get_cached_virtual_price()` internally calls `get_cached_base_cache_updated()` and `get_cached_base_virtual_price()` to resolve its dependencies. This removes the implicit call-ordering contract and the shared mutable state across accessors. It also matches the Solidity contract's `_vp_rate_ro()` more faithfully — that function resolves base_cache_updated and base_virtual_price inline, it doesn't store mirrors.
- **Plain class, not dataclass**: `PerBlockCache` is a stateful service with mutating accessors. `@dataclasses.dataclass` would imply structural equality, hashing, and immutability semantics that don't apply.
- **Accessor naming**: `get_cached_X()` on `PerBlockCache`, consistent with the pool's existing `_get_cached_X()` pattern. The "cached" prefix is self-documenting — it tells you the method may hit a cache.
- **`base_pool_is_set` as bool snapshot**: Passed at construction time, not a reference to the base pool. Base pool is immutable after construction, so the snapshot can't go stale. Passing only the narrow dependency avoids coupling PerBlockCache to the pool object.
- **`BASE_CACHE_EXPIRES` moved to `PerBlockCache`**: Only used by `get_cached_virtual_price()`, so it naturally belongs on the cache class. A `# BASE_CACHE_EXPIRES moved to PerBlockCache` comment is left on the pool for discoverability.
- **PerBlockCache is not a Protocol**: It's an internal implementation detail of `CurveStableswapPool`. Tests use `FakeCurveDataProvider` at the `CurveDataProvider` seam, which is already established.
- **PerBlockCache holds `_data_provider` reference**: The cache needs the provider to call on miss. The pool also holds `_data_provider` for the 6 uncached I/O call sites listed above. Both reference the same provider, which is fine — it's a stateless RPC delegate, not mutable state.
- **No interaction with `external_update()`**: `external_update()` only touches `_state_cache` (balances, block number). Per-block caches key on block number and are never invalidated by external updates. `PerBlockCache` has no `invalidate()` method.
- **Why not pass `CurveStableswapPool` to PerBlockCache**: The prior `CurveOnChainCache` was a shallow wrapper that held a back-reference to the pool. This created a circular dependency and didn't reduce the interface surface. This design passes only the narrow dependencies (`CurveDataProvider`, `address`, `base_pool_is_set`).
- **Direct replacement, no transitional wrappers**: All `self._get_cached_X()` calls are replaced with `self._cache.get_cached_X()` in one step. No thin delegation wrappers on the pool.

## Files Involved

**Primary:**
- `src/degenbot/curve/per_block_cache.py` — **new**: `PerBlockCache` class with all cache fields, accessor methods, `BASE_CACHE_EXPIRES`, and pickle support
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — remove 9 cache fields, 9 accessors, 2 mirrors; add `self._cache: PerBlockCache`; replace all `_get_cached_X` calls; add `# BASE_CACHE_EXPIRES moved to PerBlockCache` comment

**Secondary:**
- `src/degenbot/curve/stableswap_pool_state.py` — no change needed (state mixin doesn't reference cache fields)
- `src/degenbot/curve/strategies.py` — no change needed
- `src/degenbot/curve/types.py` — no change needed (`CurveDataProvider` protocol unchanged)
- `tests/curve/test_per_block_cache.py` — **new**: unit tests for `PerBlockCache`
- `tests/curve/test_curve_stableswap_pool.py` — update `test_metapool_with_valid_base_cache` to drop mirror assertions

**No change needed:**
- `src/degenbot/curve/data_provider_impl.py` — implements `CurveDataProvider`, which is unchanged
- `src/degenbot/curve/calculators/*.py` — receive `DyCalculationInputs`, don't reference cache
- Builder code — builds pools with `data_provider=`, which is unchanged

## Implementation Order

### Slice 1: Create PerBlockCache class

1. Create `src/degenbot/curve/per_block_cache.py` with the `PerBlockCache` class (plain class, not dataclass)
2. Move all 9 cache fields and 9 accessor methods from `CurveStableswapPool`, adapted for mirror-free design
3. `get_cached_virtual_price()` resolves its own dependencies inline (no mirrors)
4. Move `BASE_CACHE_EXPIRES` from the pool to `PerBlockCache`
5. Add `__getstate__`/`__setstate__` for pickle support (drop `_data_provider`, reconstruct as `None`)
6. Run: `just test-python` — expect same results (new file, nothing uses it yet)

### Slice 2: Wire PerBlockCache into pool and replace all call sites

1. Add `self._cache = PerBlockCache(...)` in `CurveStableswapPool.__init__`
2. Replace all `self._get_cached_X(block_number)` calls with `self._cache.get_cached_X(block_number)` in:
   - `_resolve_calculation_inputs_via_io()`
   - `_resolve_metapool_inputs_via_io()`
   - `_a()`
   - `calc_token_amount()`
   - `calc_withdraw_one_coin()`
   - `_get_y()`
   - Constructor base-pool pre-population (simplify from 2 calls to 1: `self._cache.get_cached_virtual_price(state_block)`)
3. Delete the 9 `_cache_*` field assignments from `__init__`
4. Delete the 9 `_get_cached_*` method definitions
5. Delete the 2 mirror fields (`_base_cache_updated_value`, `_base_virtual_price_value`)
6. Remove `BASE_CACHE_EXPIRES` from the pool class, add `# BASE_CACHE_EXPIRES moved to PerBlockCache` comment
7. Run: `just test-python` — all Curve pool tests pass

### Slice 3: Validate and clean up

1. Add unit tests in `tests/curve/test_per_block_cache.py`:
   - `test_cache_miss_calls_provider` — on miss, calls provider and stores result; second call hits cache
   - `test_cache_missing_provider_raises` — on miss with no provider, raises `MissingCurveData`
   - `test_virtual_price_expiry_logic` — when base cache expired, fetches live; when valid, uses cached base_virtual_price
   - `test_virtual_price_non_metapool` — non-metapool path fetches virtual_price directly
2. Update `test_metapool_with_valid_base_cache`:
   - Drop `assert lp._base_cache_updated_value is not None`
   - Drop `assert lp._base_virtual_price_value != 0`
   - Keep `_test_calculations(lp=lp, w3=fork.w3)` end-to-end check (already proves correctness)
3. Run `just lint` + `just test-all`
4. Update `src/degenbot/curve/CONTEXT.md` — update `Per-block caches` entry to reference `PerBlockCache` and its mirror-free design
5. Verify the pool's line count dropped by ~200 lines

## Testing

### Per-slice test runs

- Slice 1: additive (new file), `just test-python`
- Slice 2: behavior change (delegation + mirror elimination), covered by existing Curve pool tests, `just test-python`
- Slice 3: new tests + cleanup, `just lint` + `just test-all`

### New unit tests

```python
# tests/curve/test_per_block_cache.py

def test_cache_miss_calls_provider():
    """On cache miss, PerBlockCache calls the data provider and stores the result."""
    fake = FakeCurveDataProvider(block_timestamp_return=12345)
    cache = PerBlockCache(
        data_provider=fake, address="0x...", base_pool_is_set=False, state_cache_depth=8
    )
    result = cache.get_cached_block_timestamp(100)
    assert result == 12345
    # Second call should hit cache, not provider
    result2 = cache.get_cached_block_timestamp(100)
    assert result2 == 12345

def test_cache_missing_provider_raises():
    """On cache miss with no provider, PerBlockCache raises MissingCurveData."""
    cache = PerBlockCache(
        data_provider=None, address="0x...", base_pool_is_set=False, state_cache_depth=8
    )
    with pytest.raises(MissingCurveData):
        cache.get_cached_contract_d(100)

def test_virtual_price_non_metapool():
    """Non-metapool path fetches virtual_price directly from provider."""
    fake = FakeCurveDataProvider(virtual_price_return=10**18)
    cache = PerBlockCache(
        data_provider=fake, address="0x...", base_pool_is_set=False, state_cache_depth=8
    )
    result = cache.get_cached_virtual_price(100)
    assert result == 10**18

def test_virtual_price_expiry_logic_base_cache_valid():
    """When base cache has not expired, get_cached_virtual_price uses cached base_virtual_price."""
    # Set up provider with base_cache_updated within the expiry window
    # and base_virtual_price returning a known value
    # Assert the returned virtual_price matches base_virtual_price, not a fresh provider call
    ...

def test_virtual_price_expiry_logic_base_cache_expired():
    """When base cache has expired, get_cached_virtual_price fetches live virtual_price."""
    # Set up provider with base_cache_updated outside the expiry window
    # Assert the returned virtual_price comes from virtual_price(), not base_virtual_price()
    ...
```

### Integration tests

Existing `tests/curve/test_curve_stableswap_pool.py` and `tests/curve/test_curve_data_provider.py` exercise the full pool → cache → provider path. If they pass after Slice 2, the extraction is correct. The `test_metapool_with_valid_base_cache` test is updated to drop mirror assertions but retains its end-to-end `_test_calculations` check.

## Benefits

- **Locality**: Cache expiry logic concentrates in `PerBlockCache`; the pool's `get_dy()` flow becomes: resolve inputs → delegate to calculator
- **Correctness**: Mirror-free design eliminates the implicit call-ordering contract and the shared mutable state that caused a prior bug
- **Depth**: Pool becomes deeper — the cache is hidden behind `self._cache`, not leaked as 11 private fields/mirrors on the pool body
- **Testability**: `PerBlockCache` can be tested independently with `FakeCurveDataProvider`, without constructing a full pool
- **Pickle simplicity**: `PerBlockCache` handles its own pickle boundary; cached data survives serialization for process-pool workers

## Risks

- **Behavioral change in cross-block mirror reads**: The current mirrors are unkeyed single values (`_base_cache_updated_value`, `_base_virtual_price_value`). A call to `_get_cached_base_virtual_price(block=N)` sets the mirror, which is then read by `_get_cached_virtual_price(block=M)` — cross-block contamination via unkeyed state. The mirror-free version resolves each block's data independently via `BoundedCache` (keyed on block number), so this contamination is impossible. The `BASE_CACHE_EXPIRES` check makes the current behavior identical within the 10-minute window, but the mirror-free version is structurally safer.
- **Exception surface change**: `get_cached_virtual_price()` can now propagate `MissingCurveData` with `field="base_cache_updated"` or `field="base_virtual_price"` — exceptions that previously could only come from calling those accessors directly. The caller (`_resolve_metapool_inputs_via_io`) already handles `MissingCurveData`, so this is a documentation concern, not a runtime concern.
- **Pickle**: Both `PerBlockCache` and the pool hold `_data_provider` references. Both must drop theirs independently on pickle. `PerBlockCache.__getstate__` drops its reference; the pool's `_pickle_drops` drops its own. This is redundant but correct — each level handles its own un-pickleable reference.

## Relationship to Other Plans

- **Plan 076** (provider split): Orthogonal — different module, different concern.
- **Plan 078** (Curve InputResolver): Complementary. After this plan extracts the cache, Plan 078 extracts the uncached I/O call sites (`lending_rates`, `token_balance`, `_resolve_block_number`, etc.) that remain on the pool. Together they reduce `get_dy()` from ~300 lines to ~20 lines. The boundary is clean: PerBlockCache owns all stateful per-block caching; the pool retains all uncached I/O, which Plan 078 will absorb into the InputResolver.

## Status

[x] Slice 1: Create PerBlockCache class (mirror-free, with pickle support)
[x] Slice 2: Wire PerBlockCache into pool and replace all call sites
[x] Slice 3: Validate and clean up (unit tests, integration test update, CONTEXT.md)
