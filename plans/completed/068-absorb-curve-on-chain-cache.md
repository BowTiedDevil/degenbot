# Plan 068: Absorb CurveOnChainCache into CurveStableswapPool as a Private Implementation Detail

## Overview

Merge `CurveOnChainCache` (279 lines) into `CurveStableswapPool` as private methods, eliminating the shallow abstraction that doesn't earn its own interface. Each cache accessor inlines the 3-line try-cache→call-provider→store→return pattern as a private pool method — no generic `_get_or_fetch` / `getattr` dispatch. The `CurveDataProvider` seam (the real adapter seam with two adapters: `CurveDataProviderImpl` and `FakeCurveDataProvider`) stays unchanged.

## Problem

### Deletion test

If you delete `CurveOnChainCache` and move its 9 active `BoundedCache` fields and accessor methods back into `CurveStableswapPool`, the cache logic reappears where it lived before Plan 054. The pool class grows, but the separation provided no independent testability — the cache is never tested except through the pool, and it's never used except by the pool.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|-------------|
| Cache is tightly coupled to pool | `on_chain_cache.py` — constructor takes `data_provider`, `pool_address` | Every cache method needs the pool's data provider and address; the cache is a pool-lifetime object |
| `base_cache_updated` / `base_virtual_price` instance attrs leak | `on_chain_cache.py:58–59` — public attributes | These are instance-level mirrors used only by `virtual_price()` expiry logic; they break encapsulation |
| Cache doesn't have two adapters | `on_chain_cache.py` — no test fakes, no alternative implementations | The `_get_or_fetch` pattern is internal; the real seam is `CurveDataProvider` which has `FakeCurveDataProvider` and `CurveDataProviderImpl` |
| Pool delegates to cache for every I/O accessor | `curve_stableswap_liquidity_pool.py` — `self._on_chain_cache.block_timestamp()`, `self._on_chain_cache.contract_D()`, etc. (13 call sites) | Every I/O step is `pool → cache → provider`; the cache adds a forwarding hop with no behavioral transformation |
| Pickle support duplicated | Both pool and cache have `__getstate__`/`__setstate__` that null out `_data_provider` | Two places to maintain pickle policy; the cache's own pickle method is an implementation detail that could silently drift from the pool's |
| Duplicate `_data_provider` reference | Pool holds `self._data_provider`; cache also holds `self._data_provider` | Two references to the same object; the cache's copy is only needed because the cache is a separate object |

## Solution

### Step 1: Move `BoundedCache` fields from `CurveOnChainCache` into the pool

Each of the 9 active `BoundedCache` instances becomes a private field on the pool class. The `_block_timestamps` dict becomes a `BoundedCache` (was an unbounded `dict` — see design decision below). Note: `CurveOnChainCache._rates` (BoundedCache index 0) is dead code — it is declared but never accessed by the pool or any calculator. Lending rates are cached in `CurveDataProviderImpl._lending_rate_cache`, not here. This field is dropped during absorption rather than migrated.

```python
# Before (on_chain_cache.py)
class CurveOnChainCache:
    def __init__(self, *, data_provider, pool_address, max_items):
        self._data_provider = data_provider
        self._rates = BoundedCache(max_items=max_items)  # DEAD — never accessed
        self._scaled_redemption_price = BoundedCache(max_items=max_items)
        self._virtual_price = BoundedCache(max_items=max_items)
        # ... 7 more caches
        self._block_timestamps: dict[BlockNumber, int] = {}

# After (curve_stableswap_liquidity_pool.py)
class CurveStableswapPool:
    def __init__(self, ...):
        # ... existing fields ...
        # Per-block on-chain caches (formerly CurveOnChainCache)
        # Note: _rates from CurveOnChainCache is dead code (never accessed) — not migrated
        self._cache_scaled_redemption_price: BoundedCache[BlockNumber, int] = BoundedCache(max_items=state_cache_depth)
        self._cache_virtual_price: BoundedCache[BlockNumber, int] = BoundedCache(max_items=state_cache_depth)
        self._cache_admin_balances: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(max_items=state_cache_depth)
        self._cache_base_cache_updated: BoundedCache[BlockNumber, int] = BoundedCache(max_items=state_cache_depth)
        self._cache_base_virtual_price: BoundedCache[BlockNumber, int] = BoundedCache(max_items=state_cache_depth)
        self._cache_price_scale: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(max_items=state_cache_depth)
        self._cache_contract_D: BoundedCache[BlockNumber, int] = BoundedCache(max_items=state_cache_depth)
        self._cache_gamma: BoundedCache[BlockNumber, int] = BoundedCache(max_items=state_cache_depth)
        self._cache_block_timestamps: BoundedCache[BlockNumber, int] = BoundedCache(max_items=state_cache_depth)
        # Latest-value mirrors for virtual_price expiry logic
        self._base_cache_updated_value: int | None = None
        self._base_virtual_price_value: int = 0
```

### Step 2: Convert cache accessor methods to private pool methods

Each accessor inlines the 3-line try-cache→call-provider→store→return pattern. No generic `_cache_get_or_fetch` helper — each method is self-contained with an explicit provider call, eliminating `getattr` dynamic dispatch on the pool. Additionally, the `_a()` method's direct `self._data_provider.block_timestamp(0)` call (line 357) is rerouted through `_get_cached_block_timestamp(0)` for consistency and to avoid redundant RPC calls — this was an oversight where the pool already used `_on_chain_cache.block_timestamp()` in 5 other places but bypassed it in `_a()`.

#### Simple cached accessors

```python
def _get_cached_contract_D(self, block_number: BlockNumber) -> int:  # noqa: N802
    with contextlib.suppress(KeyError):
        return self._cache_contract_D[block_number]
    if self._data_provider is None:
        msg = "contract_D requires a data_provider. Provide one via Bot.build_pool()."
        raise MissingCurveData(self.address, "D", msg)
    result = self._data_provider.D(block_number)
    self._cache_contract_D[block_number] = result
    return result

def _get_cached_gamma(self, block_number: BlockNumber) -> int:
    with contextlib.suppress(KeyError):
        return self._cache_gamma[block_number]
    if self._data_provider is None:
        msg = "gamma requires a data_provider. Provide one via Bot.build_pool()."
        raise MissingCurveData(self.address, "gamma", msg)
    result = self._data_provider.gamma(block_number)
    self._cache_gamma[block_number] = result
    return result
```

`_get_cached_block_timestamp` follows the same pattern but calls `self._data_provider.block_timestamp(block_number)` instead of the pool-specific methods. Note: the original `CurveOnChainCache.block_timestamp()` used `if block_number in self._block_timestamps` instead of `contextlib.suppress(KeyError)`. After converting to `BoundedCache`, both `in` and `__getitem__` work identically; the `contextlib.suppress(KeyError)` pattern is used for consistency with the other accessors.

#### Base cache updated (with side-effect on `_base_cache_updated_value`)

```python
def _get_cached_base_cache_updated(self, block_number: BlockNumber) -> int:
    with contextlib.suppress(KeyError):
        return self._cache_base_cache_updated[block_number]
    if self._data_provider is None:
        msg = "base_cache_updated requires a data_provider. Provide one via Bot.build_pool()."
        raise MissingCurveData(self.address, "base_cache_updated", msg)
    result = self._data_provider.base_cache_updated(block_number)
    self._cache_base_cache_updated[block_number] = result
    # Side effect: mirror for virtual_price expiry logic
    self._base_cache_updated_value = result
    return result
```

Note: `_get_cached_base_cache_updated` MUST update `_base_cache_updated_value` as a side effect. This is a temporal dependency — `_get_cached_virtual_price` reads `_base_cache_updated_value` and expects it to reflect the latest base-cache-updated value seen by the pool. This coupling previously existed between `CurveOnChainCache.get_base_cache_updated()` and `CurveOnChainCache.virtual_price()` — it is now intra-object rather than inter-object, which is an improvement in traceability.

#### Virtual price (with base-cache-expiry logic)

This is the most complex method. It has three paths:
1. Cache hit on `_cache_virtual_price` → return immediately
2. Base cache expired or unset → fetch live `virtual_price` from provider
3. Base cache still valid → use `_base_virtual_price_value`

```python
def _get_cached_virtual_price(self, block_number: BlockNumber) -> int:
    with contextlib.suppress(KeyError):
        return self._cache_virtual_price[block_number]

    # Determine virtual price from base pool cache expiry
    # (metapool logic matching the contract's _vp_rate_ro() cache behavior)
    # Note: uses self.BASE_CACHE_EXPIRES directly instead of taking
    # a parameter — the only caller always passed this constant.
    base_virtual_price: int
    if (
        self._base_cache_updated_value is None
        or self._cache_block_timestamps.get(block_number, 0)
        > self._base_cache_updated_value + self.BASE_CACHE_EXPIRES
    ):
        # Cache is not set or has expired — fetch live virtual_price
        if self._data_provider is None:
            msg = "virtual_price requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self.address, "virtual_price", msg)
        base_virtual_price = self._data_provider.virtual_price(block_number)
    else:
        # Cache is still valid — use the cached base_virtual_price
        base_virtual_price = self._base_virtual_price_value

    self._cache_virtual_price[block_number] = base_virtual_price
    self._base_virtual_price_value = base_virtual_price
    return base_virtual_price
```

Note: `_get_cached_virtual_price` reads `_base_cache_updated_value` (set by `_get_cached_base_cache_updated`) and `_base_virtual_price_value`. It also reads `_cache_block_timestamps` directly (not through `_get_cached_block_timestamp`) to check cache expiry without triggering an I/O call — this mirrors the original cache's `self._block_timestamps.get(block_number, 0)` pattern.

#### Call-site updates

```python
# Before
block_timestamp = self._on_chain_cache.block_timestamp(block_number)
d_val = self._on_chain_cache.contract_D(block_number)

# After
block_timestamp = self._get_cached_block_timestamp(block_number)
d_val = self._get_cached_contract_D(block_number)
```

### Step 3: Delete `on_chain_cache.py` and update imports

After all fields and methods are moved, delete the module. The only import is in `curve_stableswap_liquidity_pool.py` — remove it.

### Step 4: Merge pickle policy

The pool already drops `_data_provider` in `_pickle_drops`. The cache's separate `__getstate__`/`__setstate__` that also nulls `_data_provider` is eliminated. The 9 `BoundedCache` fields (plus 1 converted from `dict`) now pickle as direct attributes of the pool via `PoolPickleMixin.__getstate__`, which copies everything not in `_pickle_drops`. `BoundedCache` extends `OrderedDict` and is picklable; the cached values (ints, tuples of ints) are also picklable. No new `_pickle_drops` entries needed — the `_data_provider` drop already prevents the provider from being serialized, and the cache entries themselves are safe to pickle (they're just block→value mappings).

Note: currently `_on_chain_cache` is NOT in `_pickle_drops`. This means the pool pickles the entire cache object, and the cache's own `__getstate__` drops its internal `_data_provider`. After absorption, the individual `_cache_*` fields are pickled directly by `PoolPickleMixin`. The behavior is equivalent — the block→value mappings are preserved, the provider is dropped — but the mechanism changes from "cache object manages its own pickle" to "pool mixin manages it." This must be verified by the existing `test_pickle_tripool` test.

### Design decisions

- **Merge, don't wrap**: The cache has no independent test surface. Wrapping it adds a forwarding hop. Merging absorbs the logic into the pool where it's used.
- **Keep `CurveDataProvider` as the I/O seam**: This is the real seam with two adapters (`CurveDataProviderImpl`, `FakeCurveDataProvider`). The cache was a false seam — it never had two adapters.
- **Inline the cache-miss pattern, no `_cache_get_or_fetch` helper**: The original `CurveOnChainCache._get_or_fetch` uses `getattr(self._data_provider, provider_method_name)` — dynamic dispatch by string. This is the only `getattr`-by-string in the pool class hierarchy. Each absorbed method will inline the 3-line try-cache→call-provider→store→return pattern with an explicit `self._data_provider.X(block_number)` call. This:
  - Eliminates `getattr` dynamic dispatch on the pool (all provider calls are statically resolvable)
  - Makes each method self-contained: the try/suppress, the explicit `if self._data_provider is None` check, the explicit provider method call, and the cache store are all visible at the call site
  - Adds ~3 lines per method instead of 1 delegating line, but each method is independently understandable without tracing through a generic helper
- **Prefix cache fields with `_cache_`**: Distinguishes the per-block cache fields from the pool's other private fields. Makes the origin from `CurveOnChainCache` traceable.
- **Keep `base_cache_updated` / `base_virtual_price` as instance attrs on the pool**: They're now `_base_cache_updated_value` and `_base_virtual_price_value`. They were public on the cache (`self.base_cache_updated`); they are private on the pool. The temporal coupling between `_get_cached_base_cache_updated` (which sets `_base_cache_updated_value` as a side effect) and `_get_cached_virtual_price` (which reads it for expiry logic) is now intra-object instead of inter-object — easier to follow and grep.
- **Make `_block_timestamps` a `BoundedCache` instead of `dict`**: The original `CurveOnChainCache._block_timestamps` is an unbounded `dict` that grows without limit. A long-running pool processing millions of blocks would accumulate unbounded entries. Making it a `BoundedCache[BlockNumber, int]` with the same `max_items` as the other caches bounds memory consistently. This was likely an oversight in Plan 054.

  **Benign behavior change**: `_get_cached_virtual_price` uses `self._cache_block_timestamps.get(block_number, 0)` to check base-cache expiry. If the timestamp for a given block has been evicted from the `BoundedCache` (due to the max_items bound), `.get()` returns 0 and the pool treats the cache as expired, fetching virtual_price live. This is a redundant I/O call but always produces correct results — when in doubt, fetch live. In practice this only affects the metapool virtual-price path, and only when the virtual-price cache misses AND the timestamp cache has evicted the relevant block. The original unbounded dict never evicted, so it never had this redundancy. The trade-off is bounded memory vs. an occasional redundant RPC call on stale cache hits.
- **Single `_data_provider` reference**: Currently the pool holds `self._data_provider` and the cache also holds `self._data_provider`. After absorption, there is only one `self._data_provider` on the pool — all cache methods reference it directly. This eliminates the possibility of the two references drifting out of sync.
- **Hardcode `BASE_CACHE_EXPIRES` in `_get_cached_virtual_price`**: The current `CurveOnChainCache.virtual_price()` takes `base_cache_expires` as a parameter, but the only caller always passes `self.BASE_CACHE_EXPIRES` — a class constant. After absorption, this parameter is removed and the method uses `self.BASE_CACHE_EXPIRES` directly. This simplifies the signature and eliminates a parameter that was never varied.
- **Route `_a()` through `_get_cached_block_timestamp`**: The `_a()` method currently calls `self._data_provider.block_timestamp(0)` directly on line 357, bypassing the cache. Every other block-timestamp call site (lines 405, 431, 466, 706, 710) goes through `_on_chain_cache.block_timestamp()`. After absorption, `_a()` is updated to call `self._get_cached_block_timestamp(0)` so it benefits from the same per-block caching. This is a minor but consistent improvement — the timestamp for block 0 is always constant (it's a provider-identity probe), so the cache hit will be immediate on subsequent calls.

## Files Involved

**Primary:**
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — absorb cache fields and methods; remove import of `CurveOnChainCache`
- `src/degenbot/curve/on_chain_cache.py` — delete

**Secondary:**
- `src/degenbot/curve/data_provider_impl.py` — no change (CurveDataProvider interface unchanged)
- `src/degenbot/curve/types.py` — no change (CurveDataProvider protocol unchanged)

**Documentation (Slice 4):**
- `src/degenbot/curve/CONTEXT.md` — remove `CurveOnChainCache` term, update relationships
- `CONTEXT-MAP.md` — remove `CurveOnChainCache` references (lines 22, 57)
- `docs/adr/ADR-001-io-free-pools.md` — update line 131
- `docs/architecture/io-free-pools.md` — update lines 149, 325
- `AGENTS.md` — update "CurveStableswapPool uses `BoundedCache`" section

**No change needed:**
- `src/degenbot/curve/trackers.py` — doesn't reference `CurveOnChainCache`
- `src/degenbot/builders/curve_pool_builder.py` — doesn't import `CurveOnChainCache`; it passes `data_provider=` to the pool constructor, which creates the cache internally. After absorption, the pool's `__init__` creates the `_cache_*` fields instead — no builder change needed.

## Implementation Order

### Slice 1: Move cache fields into pool class

1. Copy the 9 active `BoundedCache` fields from `CurveOnChainCache.__init__` into `CurveStableswapPool.__init__`, prefixed with `_cache_` (skip `_rates` — dead code, never accessed)
2. Convert `_block_timestamps` from `dict[BlockNumber, int]` to `BoundedCache[BlockNumber, int]` with `max_items=state_cache_depth`
3. Copy `_base_cache_updated_value` (`int | None = None`) and `_base_virtual_price_value` (`int = 0`) as `_base_cache_updated_value` and `_base_virtual_price_value`
4. Keep `CurveOnChainCache` as-is (not yet deleted); the new fields exist but aren't used yet
5. Keep `self._on_chain_cache = CurveOnChainCache(...)` in `__init__` (still the active path)
6. Run: `just test-python` — expect all green (fields exist but aren't used yet)

### Slice 2: Move accessor methods into pool and update all call sites

1. Create each `_get_cached_*` method on the pool, inlining the 3-line cache-miss pattern with explicit `self._data_provider.X(...)` calls (no `_get_or_fetch` / `getattr` helper)
2. Create `_get_cached_virtual_price` with the full base-cache-expiry logic documented above, reading `_base_cache_updated_value` and `_base_virtual_price_value` directly
3. Create `_get_cached_base_cache_updated` with the `_base_cache_updated_value = result` side-effect
4. Update all 13 `_on_chain_cache` call sites from `self._on_chain_cache.xxx(...)` to `self._get_cached_xxx(...)`:
   - `block_timestamp`: 5 call sites (lines 405, 431, 466, 706, 710)
   - `contract_D`, `gamma`, `price_scale`: 3 call sites (crypto I/O block)
   - `admin_balances`: 1 call site (live-admin I/O block)
   - `virtual_price`: 1 call site (metapool resolution; remove `base_cache_expires` kwarg — now hardcoded)
   - `scaled_redemption_price`: 1 call site (metapool resolution)
   - `get_base_cache_updated`: 1 call site (pool `__init__` pre-population)
   - `get_base_virtual_price`: 1 call site (pool `__init__` pre-population)
5. Route `_a()` through `_get_cached_block_timestamp`: change `self._data_provider.block_timestamp(0)` on line 357 to `self._get_cached_block_timestamp(0)`
6. Run: `just test-python` — expect all green

### Slice 3: Delete `on_chain_cache.py` and update imports

1. Delete `src/degenbot/curve/on_chain_cache.py`
2. Remove `from degenbot.curve.on_chain_cache import CurveOnChainCache` from `curve_stableswap_liquidity_pool.py`
3. Remove `self._on_chain_cache = CurveOnChainCache(...)` from `__init__`
4. Run: `just lint` + `just test-all`

### Slice 4: Update documentation

1. `src/degenbot/curve/CONTEXT.md` — remove `CurveOnChainCache` term, add note that per-block caches are private `_cache_*` fields on the pool
2. `CONTEXT-MAP.md` — update lines 22 and 57
3. `docs/adr/ADR-001-io-free-pools.md` — update line 131
4. `docs/architecture/io-free-pools.md` — update lines 149 and 325
5. `AGENTS.md` — update CurveStableswapPool architecture section

### Slice 5: Validate

1. Verify pool class line count (expected: ~+100 from cache absorption, ~-10 from removing `_on_chain_cache` construction and import, ~-2 from removing `base_cache_expires` kwarg at call site and `_a()` direct-provider reroute = net ~+88)
2. Verify all curve tests pass, including `test_pickle_tripool`
3. Verify no external code references `CurveOnChainCache` or `on_chain_cache` (grep the codebase)

## Testing

### Per-slice test runs

Each slice runs `just test-python`. Existing curve tests in `tests/curve/` are the primary validation.

### Pickle test

The existing `test_pickle_tripool` in `tests/curve/test_curve_stableswap_pool.py` validates the pickle round-trip. After absorption, `BoundedCache` fields pickle as direct attributes of the pool via `PoolPickleMixin.__getstate__`, which copies everything not in `_pickle_drops`. This is a change from the current behavior where the cache object manages its own pickle. The pickle test MUST pass without modification — if it doesn't, the `_pickle_drops` / `_pickle_reconstructs` need adjustment.

### New unit tests

No new tests needed — the cache methods are already tested through the pool. The pool's `get_dy()` tests exercise every cache accessor. The change is purely structural (moving methods from one class to another).

### Integration tests

`tests/curve/test_curve_io_free_example.py` and `tests/curve/test_pool_strategies.py` cover the full pool lifecycle including cache misses and provider calls.

## Benefits

- **Locality**: Cache miss logic concentrates inside the pool — one place to debug, one pickle policy, one `_data_provider` reference
- **Eliminates `getattr` dynamic dispatch**: Each accessor calls `self._data_provider.X()` explicitly — statically resolvable, grep-friendly, no string-based dispatch
- **Eliminates duplicate `_data_provider` reference**: The pool and cache each hold `self._data_provider`. After absorption, there's only one.
- **Bounded `_block_timestamps`**: Was an unbounded `dict`; now a `BoundedCache` matching the other 9 cache fields
- **Drops dead `_rates` cache**: `CurveOnChainCache._rates` was declared but never accessed (lending rates are cached in `CurveDataProviderImpl._lending_rate_cache`). Eliminated during absorption.
- **Depth**: `CurveOnChainCache` is shallow — its interface is nearly as wide as its implementation (each accessor method is a thin cache-miss wrapper). Absorbing it deepens the pool.
- **Deletion test**: The cache doesn't survive deletion — its logic reappears in the pool (where it was before Plan 054). This confirms it's an extraction that didn't yield independent testability.

## Risks

- **The pool retains 6 direct `self._data_provider.X()` call sites after absorption**: The 13 `_on_chain_cache` calls become `_get_cached_*` calls. The `_a()` method's direct `_data_provider.block_timestamp(0)` is rerouted to `_get_cached_block_timestamp(0)`. The remaining direct `_data_provider` calls are: `token_balance` (in `_fetch_token_balance` and live-admin I/O), `token_total_supply` (in `_fetch_token_total_supply`), `block_number` (in `_resolve_block_number`), and `lending_rates` (in `_resolve_calculation_inputs_via_io` and `_resolve_rates`). These are NOT absorbed because they are not per-block-cached values behind `BoundedCache` — `lending_rates` has its own cache in `CurveDataProviderImpl._lending_rate_cache`, and `token_balance`/`token_total_supply` are called with token-address parameters that don't fit a simple block-number-keyed cache. This is fine — the remaining direct calls are legitimate uses of the `CurveDataProvider` seam.

- **Pool class grows**: The pool is 1016 lines. Absorbing ~100 lines of cache methods takes it to ~1116. Mitigation: the methods are simple and repetitive; they don't add conceptual complexity. The pool was 1160 lines before Plan 054 extracted the cache — this partially returns to a known working size, but stays below the pre-054 line count because we inline the 3-line pattern instead of moving the `_get_or_fetch` helper.
- **Reverses Plan 054 partially**: Plan 054 extracted the cache to "shrink the pool class." However, the extraction didn't yield independent testability or a clean seam. The behavioral improvements from Plan 054 (consolidated `BoundedCache` fields, unified cache-miss pattern, dead code deletion) are preserved — only the structural separation is reversed.
- **Pickle mechanism changes**: Currently the cache object manages its own pickle via `__getstate__`/`__setstate__`. After absorption, `PoolPickleMixin` manages the pickle lifecycle. The 9 `BoundedCache` fields (plus 1 converted from `dict`) now pickle as direct attributes. This is functionally equivalent (same data preserved, same provider dropped) but the mechanism changes. Mitigation: the existing `test_pickle_tripool` test validates the round-trip without modification.
- **Temporal coupling between `_get_cached_base_cache_updated` and `_get_cached_virtual_price`**: This coupling existed before (between `CurveOnChainCache.get_base_cache_updated` and `CurveOnChainCache.virtual_price`) but was hidden across two methods in the same auxiliary class. Now it's visible on the same class — an improvement in transparency, but worth noting.
- **Pre-population uses `contextlib.suppress(Exception)`**: The pool's `__init__` wraps the pre-population calls (`_get_cached_base_cache_updated`, `_get_cached_base_virtual_price`) in `contextlib.suppress(Exception)` — not just `contextlib.suppress(MissingCurveData)`. This swallows all exceptions (including RPC errors) during pre-population, matching the contract's behavior where a failed cache warmup is non-fatal. After absorption, the same `contextlib.suppress(Exception)` wraps the pool methods. This is unchanged behavior but worth noting since the methods now raise `MissingCurveData` on provider absence.

## Relationship to Other Plans

- **Plan 069** (Remove closures from DyCalculationInputs): Complementary — after absorbing the cache, the pool's internal I/O resolution is all in one place, making it easier to trace how closures are constructed. Specifically, the `get_y` closure in `_resolve_calculation_inputs_via_io` calls `self._get_y()` which calls `self._get_cached_block_timestamp(self.update_block)` — after Plan 069, this closure is eliminated and the block timestamp is resolved directly in `_resolve_calculation_inputs_via_io`.
- **Plan 054** (Consolidate Curve caches): This plan reverses the structural aspect of Plan 054 (separate cache class) while keeping the behavioral aspect (consolidated `BoundedCache` fields and unified cache-miss pattern). The 9 individual fields that Plan 054 consolidated remain consolidated — they're just owned by the pool instead of a separate object. The dead `_rates` field is dropped. The `_block_timestamps` unbounded `dict` overlooked in Plan 054 is also fixed.

## Status

[x] Slice 1: Move cache fields into pool class (convert `_block_timestamps` to `BoundedCache`)
[x] Slice 2: Move accessor methods into pool and update all call sites
[x] Slice 3: Delete `on_chain_cache.py` and update imports
[x] Slice 4: Update documentation (CONTEXT.md, CONTEXT-MAP.md, ADR-001, io-free-pools.md, AGENTS.md)
[x] Slice 5: Validate (line count, all tests, no external references)

### Post-completion fix

`_get_cached_base_virtual_price` was missing the `_base_virtual_price_value = result` side effect on both the cache-hit and cache-miss paths. This mirror field is read by `_get_cached_virtual_price` when the base cache has not expired. Without the side effect, `_base_virtual_price_value` stays at its initial value of `0`, causing metapool `get_dy` / `get_dy_underlying` calculations to return `0` for virtual-price-dependent terms. The bug was latent in the original `CurveOnChainCache.get_base_virtual_price()` too (it also didn't update `self.base_virtual_price`). It was only exposed at blocks where the base cache hadn't expired — the existing multi-block test at blocks 18,850,000–18,850,500 always had an expired cache, masking the bug.
