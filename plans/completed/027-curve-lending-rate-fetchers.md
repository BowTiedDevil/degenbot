# Plan 027: Convert Curve Lending-Rate Methods to Typed Fetcher Protocols

Committed as `0fe5d9ed` (alongside Plan 026). See also bug fix `a03b3295` (sUSD pool LendingRateStyle).

## Overview

Replace the six `_stored_rates_from_*()` methods on `CurveStableswapPool` with typed fetcher closures injected at construction time. This eliminates the `provider_call` backdoor from the pool class entirely, completing the I/O-free architecture for Curve lending pools.

## Files Involved

**Primary:**
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — removed 6 `_stored_rates_from_*()` methods (~250 lines), removed `provider_call` fetcher, removed 6 `BoundedCache` attributes for rate results
- `src/degenbot/curve/types.py` — added `LendingRateFetcher` protocol
- `src/degenbot/curve/fetcher_factory.py` — added 7 factory methods for each lending rate fetcher variant

**Secondary:**
- `src/degenbot/builders/curve_pool_builder.py` — creates correct fetcher from `PoolStrategies.lending_rate_style`
- `src/degenbot/curve/CONTEXT.md` — documented `LendingRateFetcher` protocol
- `tests/curve/test_curve_io_free_example.py` — updated to use `lending_rate_fetcher` instead of `provider_call`

## Problem

### Deletion test

If you delete the `_stored_rates_from_*()` methods and `provider_call` from the pool class, complexity does NOT vanish — it reappears as "how does the pool get rates for lending tokens?" Currently, the pool contains all six rate-fetching implementations internally and selects one via `LendingRateStyle` dispatch. The implementations are genuinely different (cToken accrual formula vs yToken PPS vs aETH ratio inversion), so the complexity is real. But it doesn't belong *inside* the pool.

### Former state (before implementation)

| Method | Lines | ABI calls | Pattern |
|--------|-------|-----------|---------|
| `_stored_rates_from_ctokens()` | ~58 | `exchangeRateStored()`, `supplyRatePerBlock()`, `accrualBlockNumber()` | Cache check → provider_call → decode per-token → cache store |
| `_stored_rates_from_ytokens()` | ~40 | `getPricePerFullShare()` | Same pattern |
| `_stored_rates_from_cytokens()` | ~52 | `exchangeRateStored()`, `supplyRatePerBlock()`, `accrualBlockNumber()` | Same pattern |
| `_stored_rates_from_reth()` | ~25 | `getExchangeRate()` | Same pattern |
| `_stored_rates_from_aeth()` | ~30 | `ratio()` | Same pattern with inversion |
| `_stored_rates_from_oracle()` | ~45 | `oracle_method()`, then dynamic call via bitmask | Most complex — oracle method detection + fallback |

Each method followed the same pattern:
1. Check `BoundedCache` for a cached result at this block
2. Check that `self._provider_call` is not None (raise `MissingCurveData` if it is)
3. Perform ABI-encoded calls via `self._provider_call(to=, data=, block=)`
4. Decode results, compute rates
5. Store in cache, return

The `provider_call` closure was a raw `(to, data, block) -> bytes` escape hatch that let the pool do arbitrary on-chain reads. It was the one seam in the I/O-free architecture that hadn't been deepened into a typed fetcher.

## Solution

### Step 1: Define `LendingRateFetcher` protocol ✅

Added to `src/degenbot/curve/types.py`:

```python
class LendingRateFetcher(Protocol):
    """Fetch lending rates for all tokens in a Curve pool at a given block."""

    def __call__(self, block_number: int) -> tuple[int, ...]:
        """Return per-token rates scaled to PRECISION (10^18).

        Non-lending tokens return PRECISION. Lending tokens return
        their rate (e.g., cToken exchange rate, yToken PPS).
        """
```

### Step 2: Create fetcher implementations ✅

Each former `_stored_rates_from_*()` method became a standalone fetcher closure factory method in `CurveFetcherFactory` (in `src/degenbot/curve/fetcher_factory.py`):

- `ctoken_rate_fetcher()` — cToken exchange rate with supply rate accrual
- `ytoken_rate_fetcher()` — yToken price per full share
- `cytoken_rate_fetcher()` — combined cToken + yToken accrual
- `reth_rate_fetcher()` — Rocket Pool exchange rate
- `aeth_rate_fetcher()` — Lido aETH ratio inversion
- `oracle_rate_fetcher()` — on-chain oracle bitmask (with lazy `oracle_method` caching via Option A)
- `lending_rate_fetcher()` — dispatcher that selects the correct fetcher based on `LendingRateStyle`

Each factory creates a closure that captures `chain_id`, `tokens`, `use_lending`, `precision_multipliers`,
and uses `self._connections.get_provider(chain_id)` for I/O when called. Each closure has its own
`BoundedCache(max_items=8)` for per-block rate caching.

### Step 3: Remove `_stored_rates_from_*()` and `provider_call` from pool class ✅

All of the following were removed from `CurveStableswapPool`:

- 6 `_stored_rates_from_*()` methods (~250 lines total)
- `_set_oracle_method()` method
- `provider_call` constructor parameter and `_provider_call` attribute
- `oracle_method` attribute
- `LENDING_PRECISION` class variable
- 6 `_cached_rates_from_*` BoundedCache attributes

The pool's `_resolve_rates()` method now dispatches to `self._lending_rate_fetcher(block_number)`
based on `self._strategies.lending_rate_style`. Modified imports: removed `eth_abi.abi`, `HexBytes`,
`Web3` from the pool file (no longer needed).

Pickle support: `_pickle_drops` updated to include `_lending_rate_fetcher`. After unpickle,
`_lending_rate_fetcher` defaults to `None` (consistent with how other fetchers degrade).

### Step 4: Update pool construction ✅

```python
class CurveStableswapPool:
    def __init__(
        self,
        # ... existing parameters ...
        lending_rate_fetcher: LendingRateFetcher | None = None,  # NEW
        # REMOVED: provider_call
    ):
        self._lending_rate_fetcher = lending_rate_fetcher
```

When `lending_rate_fetcher` is None and the pool has no lending tokens, `get_dy()` uses
`self.rate_multipliers` directly (no fetcher needed). When it's None but the pool *does* have
lending tokens, `MissingCurveData` is raised — consistent with the existing pattern.

### Step 5: Handle the `oracle_method` state ✅

**Implemented: Option A** — the oracle fetcher closure captures a mutable `list` as a cache
for the `oracle_method` value. The first call fetches `oracle_method()` from the contract,
caches it in the closure, and uses it for all subsequent calls. This matches the old behavior
where `_set_oracle_method()` was called as a side effect of `_stored_rates_from_oracle()`.

### Step 6: Update `CurvePoolBuilder.build()` ✅

The builder calls `CurveFetcherFactory.lending_rate_fetcher()` which dispatches to the correct
variant based on `PoolStrategies.lending_rate_style`:

```python
lending_rate_fetcher = fetchers.lending_rate_fetcher(
    pool_address=pool_address,
    lending_rate_style=strategies.lending_rate_style,
    tokens=coins,
    use_lending=lending.use_lending,
    precision_multipliers=lending.precision_multipliers,
)
```

If `LendingRateStyle.NONE`, no fetcher is created (returns `None`).

## Implementation Order

1. ✅ **Define `LendingRateFetcher` protocol** in `types.py`
2. ✅ **Add fetcher factory methods** to `CurveFetcherFactory` — 7 methods (6 variants + 1 dispatcher)
3. ✅ **Add `lending_rate_fetcher` parameter to pool constructor** with None default — backwards-compatible
4. ✅ **Remove `_stored_rates_from_*()` methods** from pool class — all 6 methods removed
5. ✅ **Remove `provider_call` from pool constructor** and all related attributes (`_provider_call`, `oracle_method`, `LENDING_PRECISION`, 6 `BoundedCache` attributes)
6. ✅ **Add `_resolve_rates()` method** to pool class — dispatches to `_lending_rate_fetcher` based on `LendingRateStyle`
7. ✅ **Update builder** to create the correct fetcher from `PoolStrategies.lending_rate_style`
8. ✅ **Update tests** — `test_curve_io_free_example.py` updated to use `lending_rate_fetcher`
9. ✅ **Update `CONTEXT.md`** — documented `LendingRateFetcher` protocol

## Testing

### Integration tests (existing)

All 129 Curve tests pass. The fetcher closures produce the same results as the methods they replaced.

### Regression test: no `provider_call` on pool

The pool class no longer has any `_provider_call` attribute, `oracle_method` attribute, or
`provider_call` constructor parameter. All on-chain I/O flows through typed fetcher protocols.

## Benefits

- **Locality:** Rate-fetching logic (cToken accrual formula, yToken PPS, aETH ratio inversion, oracle bitmask decoding) is concentrated in fetcher implementations, not spread across the pool class. A bug in the cToken accrual formula is found in one fetcher, not in a method on a 1708-line class.
- **Leverage:** Each fetcher can be tested independently — pass canned bytes to the factory, verify the decoded rate. No need to construct a full pool to test rate logic.
- **Simpler pool constructor:** The `provider_call` backdoor is gone — the pool is fully I/O-free with no escape hatch.
- **True I/O-free:** After this plan and Plan 026, `CurveStableswapPool` has zero references to `ProviderAdapter`, `ConnectionManager`, `Bot`, or any I/O mechanism. All on-chain data flows through typed fetcher protocols.
- **Pool class size:** 2122 → 1708 lines (~19% reduction combined with Plan 026)
- **Removed imports:** `eth_abi.abi`, `HexBytes`, `Web3` no longer imported by the pool file

## Risks

- **Caching migration:** ✅ Resolved. Each fetcher closure has its own `BoundedCache(max_items=8)`. This is an improvement over the previous pattern (no cross-contamination between rate types), with bounded memory overhead.
- **Oracle method state:** ✅ Resolved (Option A). The `oracle_method` value is cached inside the oracle fetcher closure as a lazy-once mutable. The pool no longer exposes `oracle_method` as a public attribute — it's now an implementation detail of the oracle fetcher.
- **`_stored_rates_from_oracle` complexity:** ✅ Resolved. The two-step pattern (detect oracle method → compute rate) is preserved inside the oracle fetcher closure. The mutable `oracle_method` cache in the closure handles the lazy initialization correctly.

## Relationship to Other Plans

- **Plan 026** (Strategy Objects): Plan 026 defines *which* lending rate style a pool uses (`LendingRateStyle` enum). This plan (027) defines *how* that style fetches its data (typed fetcher closures). Both were implemented together in `0fe5d9ed`.
- **Plan 013** (Curve I/O-Free Architecture): Complete. This plan deepens the I/O-free seam that Plan 013 established. Plan 013 created the fetcher pattern but left `provider_call` as a backdoor for lending rates. This plan closes that backdoor.
- **ADR-001** (I/O-Free Pools): This plan completes ADR-001 for Curve pools. After this, no Curve pool method contains I/O — all on-chain data flows through typed fetcher protocols.
