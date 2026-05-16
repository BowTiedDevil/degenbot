# Plan 040: Consolidate Curve Fetcher Callbacks into a CurveDataProvider

## Overview

Replace the 13 individual fetcher callback parameters in `CurveStableswapPool.__init__()` with a single `CurveDataProvider` seam. The provider bundles all on-chain data access behind one interface: virtual price, base virtual price, block timestamp, redemption price, admin balances, D, gamma, price scale, lending rates, and token balance/total supply queries.

This directly addresses the "constructor bloat" negative consequence documented in ADR-001 and reduces the pool's pickle complexity from 13 individual fetcher drops + 13 reconstruct lambdas to 1 drop + 1 reconstruction.

## Files Involved

**Primary:**
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — replace 13 `*_fetcher` constructor parameters with one `data_provider: CurveDataProvider | None`; replace 13 `self._*_fetcher` attributes with `self._data_provider`; update all ~110 fetcher references in the class body
- `src/degenbot/curve/types.py` — replace 8 fetcher protocols (`VirtualPriceFetcher`, `TimestampFetcher`, `RedemptionPriceFetcher`, `AdminBalancesFetcher`, `DFetcher`, `GammaFetcher`, `PriceScaleFetcher`, `LendingRateFetcher` + 3 untyped `Any` fetchers) with one `CurveDataProvider` protocol
- `src/degenbot/curve/fetcher_factory.py` — the `CurveFetcherFactory` becomes a class implementing `CurveDataProvider`, dispatching internally to its existing methods; the 13 factory methods become the implementation of the provider's interface

**Secondary:**
- `src/degenbot/builders/curve_pool_builder.py` — simplify `build()` from 13 individual fetcher factory calls + 13 keyword arguments to one `fetchers = CurveFetcherFactory(...)` + `data_provider=fetchers`
- `src/degenbot/curve/stableswap_pool_state.py` — no change (state attributes don't include fetchers)
- `src/degenbot/curve/_pool_strategies.py` — no change (strategies are pure configuration, not fetchers)
- `tests/curve/` — update pool construction: 13 separate lambdas → 1 fake provider or `None`

## Problem

### Deletion test

If you deleted the 13 fetcher protocols, each pool call site that checks `if self._*_fetcher is None: raise MissingCurveData(...)` would need an alternative. The fetches don't vanish — the complexity would reappear as direct `ConnectionManager` access inside the pool (violating ADR-001). The protocols are earning their keep. But 13 individual protocols, 13 constructor parameters, 13 pickle drops, and 13 None-guard patterns are pure boilerplate overhead that obscures the actual computation.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| **13 constructor parameters** | `CurveStableswapPool.__init__` | Most complex constructor in the codebase — 30+ keyword arguments, 13 of which are fetchers |
| **13 pickle drops** | `_pickle_drops` class var | 13 of the 20 pickle-dropped attributes are fetchers |
| **13 reconstruct lambdas** | `_pickle_reconstructs` class var | Each fetcher defaults to `lambda: None` after unpickle |
| **~110 occurrences of "fetcher"** | Pool class body | `if self._*_fetcher is None: raise MissingCurveData(...)` appears at every call site; finding which fetchers a method actually uses requires scanning all 13 None-checks |
| **Builder boilerplate** | `CurvePoolBuilder.build()` | 13 lines calling `fetchers.xxx_fetcher(...)` followed by 13 keyword arguments passing results to the constructor |
| **Test setup complexity** | `tests/curve/` | Testing any calculation that touches a fetcher requires supplying up to 13 callback arguments |

### Current fetcher inventory

| Fetcher | Protocol type | Called by methods | Notes |
|---------|--------------|-------------------|-------|
| `virtual_price_fetcher` | `VirtualPriceFetcher` | `_get_virtual_price`, `get_dy` (metapool), `calculate_tokens_out_from_tokens_in` | Accepts `block_number` arg |
| `base_virtual_price_fetcher` | `VirtualPriceFetcher` | `_get_base_virtual_price` | Metapool only |
| `base_cache_updated_fetcher` | `VirtualPriceFetcher` | `_get_base_cache_updated` | Metapool only |
| `timestamp_fetcher` | `TimestampFetcher` | `get_dy`, `_a`, `_resolve_block_number` | Accepts `block_number` arg |
| `redemption_price_fetcher` | `RedemptionPriceFetcher` | `_get_scaled_redemption_price` | LSD pools only |
| `admin_balances_fetcher` | `AdminBalancesFetcher` | `_get_admin_balances` | Live-admin pools |
| `block_number_fetcher` | `Any` | `_resolve_block_number` | Untyped; returns int |
| `total_supply_fetcher` | `Any` | `_fetch_token_total_supply` | Untyped; pool-agnostic helper |
| `token_balance_fetcher` | `Any` | `_fetch_token_balance` | Untyped; pool-agnostic helper |
| `D_fetcher` | `DFetcher` | `_get_d` (crypto path) | Crypto pools only |
| `gamma_fetcher` | `GammaFetcher` | `get_dy` (crypto path) | Crypto pools only |
| `price_scale_fetcher` | `PriceScaleFetcher` | `get_dy` (crypto path) | Crypto pools only |
| `lending_rate_fetcher` | `LendingRateFetcher` | `_resolve_rates` | Lending pools only |

These fall into three natural groups:

1. **Pool-state fetchers** — query the pool contract itself (virtual_price, base_virtual_price, base_cache_updated, admin_balances, D, gamma, price_scale)
2. **Chain-state fetchers** — query chain state unrelated to the pool (timestamp, block_number)
3. **Helper fetchers** — reusable I/O helpers (token_balance, total_supply, lending_rate)

## Solution

### Step 1: Define `CurveDataProvider` protocol

In `src/degenbot/curve/types.py`:

```python
class CurveDataProvider(Protocol):
    """On-chain data access for a Curve StableSwap pool.
    
    All methods are optional — the pool checks availability
    before calling. A provider that doesn't support a method
    should raise MissingCurveData if called.
    """
    
    # Pool-state fetchers
    def virtual_price(self, block_number: int) -> int: ...
    def base_virtual_price(self, block_number: int) -> int: ...
    def base_cache_updated(self, block_number: int) -> int: ...
    def admin_balances(self, block_number: int) -> tuple[int, ...]: ...
    def D(self, block_number: int) -> int: ...            # crypto only
    def gamma(self, block_number: int) -> int: ...        # crypto only
    def price_scale(self, block_number: int) -> tuple[int, ...]: ...  # crypto only
    
    # Chain-state fetchers
    def block_timestamp(self, block_number: int) -> int: ...
    def block_number(self) -> int: ...
    
    # Helper fetchers
    def token_balance(self, token: Erc20Token, holder: ChecksumAddress, block_number: int) -> int: ...
    def token_total_supply(self, token: Erc20Token, block_number: int) -> int: ...
    def lending_rates(
        self,
        block_number: int,
        pool_address: ChecksumAddress,
        tokens: list[Erc20Token],
        use_lending: list[bool],
        precision_multipliers: list[int],
        rate_multipliers: tuple[int, ...],
        lending_rate_style: LendingRateStyle,
    ) -> tuple[int, ...]: ...
```

**Design decision:** One protocol, not three. The pool already knows which methods are valid for its variant (via `PoolStrategies`). Splitting into `PoolStateProvider` / `ChainStateProvider` / `HelperProvider` would reduce the pool's constructor to 3 parameters instead of 13, but introduce a new coordination problem: "which provider(s) does this pool variant need?" A single provider with all methods is simpler, and the pool guards with `MissingCurveData` exactly as it does today.

### Step 2: `CurveFetcherFactory` implements `CurveDataProvider`

The existing `CurveFetcherFactory` gains a `CurveDataProvider` adapter layer. Two implementation options:

**Option A: Factory becomes the provider**

```python
class CurveFetcherFactory:
    """Creates fetcher closures AND implements CurveDataProvider."""
    
    def __init__(self, *, connections: ConnectionManager, chain_id: ChainId) -> None:
        self._connections = connections
        self._chain_id = chain_id
        self._pool_address: ChecksumAddress | None = None
    
    def set_pool_address(self, address: ChecksumAddress) -> None:
        self._pool_address = address
    
    # --- CurveDataProvider implementation ---
    
    def virtual_price(self, block_number: int) -> int:
        # Same logic as current virtual_price_fetcher() return value
        ...
    
    def block_timestamp(self, block_number: int) -> int:
        ...
    
    # etc.
```

**Option B: Factory creates a standalone provider**

```python
class CurveFetcherFactory:
    """Creates a CurveDataProvider for a given pool."""
    
    def create_provider(
        self,
        pool_address: ChecksumAddress,
        *,
        base_pool_address: ChecksumAddress | None = None,
        tokens: list[Erc20Token] | None = None,
        use_lending: list[bool] | None = None,
        precision_multipliers: list[int] | None = None,
        rate_multipliers: tuple[int, ...] | None = None,
        lending_rate_style: LendingRateStyle = LendingRateStyle.NONE,
        is_crypto: bool = False,
        n_coins: int = 2,
    ) -> CurveDataProvider:
        # Returns a _CurveDataProviderImpl instance
        ...
```

**Recommendation:** Option B. It keeps the factory's responsibility ("create a provider") separate from the provider's responsibility ("perform I/O"). The implementation class `_CurveDataProviderImpl` is private to the factory module.

### Step 3: Replace 13 constructor parameters with 1

```python
class CurveStableswapPool:
    def __init__(
        self,
        address: ChecksumAddress | str,
        *,
        tokens: Sequence[Erc20Token],
        a_coefficient: int,
        fee: int,
        admin_fee: int,
        balances: Sequence[int],
        chain_id: ChainId | None = None,
        state_block: BlockNumber | None = None,
        state_cache_depth: int = 8,
        # One provider replaces 13 fetchers
        data_provider: CurveDataProvider | None = None,
        # Pool configuration (unchanged)
        base_pool: "CurveStableswapPool | None" = None,
        tokens_underlying: Sequence[Erc20Token] | None = None,
        lp_token: Erc20Token | None = None,
        use_lending: Sequence[bool] | None = None,
        precision_multipliers: Sequence[int] | None = None,
        # A ramping configuration (unchanged)
        initial_a_coefficient: int | None = None,
        future_a_coefficient: int | None = None,
        initial_a_coefficient_time: int | None = None,
        future_a_coefficient_time: int | None = None,
        create_timestamp: int | None = None,
        # Crypto pool parameters (unchanged)
        fee_gamma: int | None = None,
        mid_fee: int | None = None,
        out_fee: int | None = None,
        gamma: int | None = None,
        offpeg_fee_multiplier: int | None = None,
        # Strategy enums (unchanged)
        strategies: PoolStrategies = PoolStrategies(),
    ) -> None:
```

The constructor parameter count drops from ~30 to ~18. The 13 fetcher keyword arguments are replaced by `data_provider`.

### Step 4: Replace fetcher attributes and call sites

```python
# Old:
if self._virtual_price_fetcher is None:
    raise MissingCurveData(self.address, "virtual_price", "...")
vp = self._virtual_price_fetcher(block_number)

# New:
if self._data_provider is None:
    raise MissingCurveData(self.address, "data_provider", "...")
vp = self._data_provider.virtual_price(block_number)
```

The None-check pattern changes from per-fetcher to per-provider. Since the pool typically uses the same provider for all fetches, a single check at the top of methods that need I/O is sufficient:

```python
def _get_virtual_price(self, block_number: BlockNumber) -> int:
    self._require_provider("virtual_price")
    ...
```

### Step 5: Simplify pickle handling

```python
_pickle_drops: ClassVar[frozenset[str]] = frozenset({
    "_state_lock",
    "_subscribers",
    "_data_provider",  # replaces 13 fetcher drops
})

_pickle_reconstructs: ClassVar[dict[str, Any]] = {
    "_state_lock": Lock,
    "_subscribers": WeakSet,
    "_data_provider": lambda: None,  # replaces 13 fetcher reconstructs
}
```

13 entries → 1 entry in each pickle map.

### Step 6: Simplify the builder

```python
# Old (13 fetcher calls + 13 keyword args):

fetchers = CurveFetcherFactory(connections=self._connections, chain_id=chain_id)
pool = CurveStableswapPool(
    ...
    virtual_price_fetcher=fetchers.virtual_price_fetcher(pool_address, ...),
    base_virtual_price_fetcher=fetchers.base_virtual_price_fetcher(pool_address),
    base_cache_updated_fetcher=fetchers.base_cache_updated_fetcher(pool_address),
    timestamp_fetcher=fetchers.timestamp_fetcher(),
    redemption_price_fetcher=fetchers.redemption_price_fetcher(pool_address),
    admin_balances_fetcher=fetchers.admin_balances_fetcher(pool_address),
    block_number_fetcher=fetchers.block_number_fetcher(),
    total_supply_fetcher=fetchers.total_supply_fetcher(),
    token_balance_fetcher=fetchers.token_balance_fetcher(),
    lending_rate_fetcher=fetchers.lending_rate_fetcher(...),
    D_fetcher=fetchers.D_fetcher(pool_address) if crypto.is_crypto else None,
    gamma_fetcher=fetchers.gamma_fetcher(pool_address) if crypto.is_crypto else None,
    price_scale_fetcher=fetchers.price_scale_fetcher(...) if crypto.is_crypto else None,
    ...
)

# New:

fetchers = CurveFetcherFactory(connections=self._connections, chain_id=chain_id)
provider = fetchers.create_provider(
    pool_address,
    base_pool_address=metapool.base_pool_address if metapool.is_meta else None,
    tokens=list(tokens),
    use_lending=...,
    precision_multipliers=...,
    rate_multipliers=...,
    lending_rate_style=strategies.lending_rate_style,
    is_crypto=crypto.is_crypto,
    n_coins=len(tokens),
)
pool = CurveStableswapPool(
    ...
    data_provider=provider,
    ...
)
```

13 fetcher calls + 13 keyword args → 1 provider creation + 1 keyword arg.

### Step 7: Remove old fetcher protocols from `curve/types.py`

The individual fetcher protocols (`VirtualPriceFetcher`, `TimestampFetcher`, etc.) are replaced by `CurveDataProvider`. They can be removed or deprecated. If external code references them (unlikely — they're implementation details), re-export as aliases for a deprecation period.

## Implementation Order

1. **Define `CurveDataProvider` protocol** in `curve/types.py`
2. **Add `_CurveDataProviderImpl`** as a private class in `fetcher_factory.py` implementing the protocol, wrapping the existing closure-creation logic
3. **Add `create_provider()` method to `CurveFetcherFactory`** — returns a `_CurveDataProviderImpl`
4. **Add `data_provider` parameter to `CurveStableswapPool.__init__`** alongside the existing 13 fetcher params (both accepted during migration)
5. **Replace fetcher call sites** one at a time — `self._virtual_price_fetcher(...)` → `self._data_provider.virtual_price(...)`, with fallback to old attribute for migration
6. **Update builder** to use `data_provider` path
7. **Remove the 13 old fetcher parameters** from the constructor — hard cutover
8. **Simplify pickle handling** — 13 drops + 13 reconstructs → 1 + 1
9. **Remove old fetcher protocols** from `curve/types.py`
10. **Update tests** — 13 lambdas → 1 fake provider or `None`
11. **Update `CONTEXT.md`** with provider terms

## Testing

### Per-step test runs

Each step runs `just test-python`. The migration supports incremental rollout: both old and new paths work during steps 4–6.

### New unit tests

- `tests/curve/test_curve_data_provider.py`:
  - Protocol satisfaction — `_CurveDataProviderImpl` satisfies `CurveDataProvider`
  - Each method forwards to the correct RPC call with correct encoding
  - `None` provider raises `MissingCurveData` on access
  - Pickle round-trip drops and reconstructs the provider

### Integration tests

All existing Curve tests pass — the pool's public interface is identical. Tests that construct pools directly update from 13 fetcher lambdas to 1 `FakeCurveDataProvider` (or `None` for tests that don't need I/O).

### Fake provider for tests

```python
class FakeCurveDataProvider:
    """Minimal provider for unit tests that don't need I/O."""
    
    def virtual_price(self, block_number: int) -> int:
        return 10**18
    
    def block_timestamp(self, block_number: int) -> int:
        return 1700000000
    
    # ... etc, returning safe defaults
```

## Benefits

- **Constructor shrinkage:** 30+ parameters → ~18. The 13 fetcher keyword arguments are replaced by 1.
- **Pickle simplification:** 13 drop entries + 13 reconstruct lambdas → 1 + 1.
- **Test simplification:** 13 separate lambdas → 1 fake provider or `None`.
- **Builder simplification:** 13 fetcher factory calls + 13 constructor keyword args → 1 `create_provider()` + 1 keyword arg.
- **~110 fetcher references → ~15 provider references:** the `if self._*_fetcher is None: raise MissingCurveData(...)` guard repeats 13 times across the class; a single `_require_provider()` helper replaces all of them.
- **ADR-001 negative consequence addressed:** the "constructor bloat" directly noted in ADR-001 is resolved.

## Risks

- **Provider becomes a god interface:** `CurveDataProvider` has ~13 methods, which is on the boundary. Mitigated by: (1) the methods are naturally grouped (pool-state, chain-state, helpers), (2) a single provider is still simpler than 13 separate closures, (3) the protocol is only implemented by `_CurveDataProviderImpl` — there's no polymorphism benefit to splitting it yet.
- **Lending-rate fetcher coupling:** the `lending_rates()` method takes many parameters (tokens, use_lending, precision_multipliers, rate_multipliers, lending_rate_style). This is because lending rate fetching needs pool-specific context. In the current design, these are captured by the closure at creation time. In the provider design, they're passed at call time. Mitigated by: the provider can capture them at construction (they're immutable pool state), so the method signature stays simple.
- **Migration path:** the incremental approach (accept both old and new during steps 4–6) means the constructor temporarily has 14 fetcher-like parameters. This is temporary and acceptable.

## Relationship to Other Plans

- **Plan 039** (DyCalculator Seam): Complementary. Plan 039 extracts calculation; this plan consolidates I/O. They can proceed in either order. If 039 is done first, the DyCalculator instances can receive a `CurveDataProvider` instead of the full pool.
- **Plan 041** (Elevate Curve State Mixin): Orthogonal. That plan reorganizes where state data attributes live; this plan reorganizes where I/O callbacks live. They don't depend on each other.
- **ADR-001** (I/O-Free Pools): This plan directly addresses the "constructor bloat" negative consequence documented in ADR-001.
- **Plan 013** (Curve I/O-Free Architecture): Complete. This plan builds on the I/O-free architecture by simplifying the seam through which I/O re-enters the pool.
