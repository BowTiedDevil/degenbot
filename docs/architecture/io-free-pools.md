# I/O-Free Pool Architecture

## Overview

The I/O-free architecture is a design pattern for pool implementations that decouples on-chain data fetching from pool logic. Instead of calling provider methods directly, pools receive **fetcher callbacks** at construction time and call them on-demand when data is needed.

## Motivation

### The Problem with Direct I/O

Traditional pool implementations embed I/O calls:

```python
class OldStylePool:
    def __init__(self, address):
        self.address = address
        self.provider = get_connection_manager().get_provider(self.chain_id)
    
    def get_rate(self, block_number):
        # Direct I/O: pool knows about connections, providers, async handling
        return self.provider.call(self.rate_contract, "exchangeRateStored", block_number)
```

**Problems:**
1. **Testing is hard**: Requires mocking providers, connection managers
2. **Async complexity**: Pool must handle sync/async boundaries
3. **Coupling**: Pool knows about infrastructure details (providers, chains, connections)
4. **State management**: Provider references complicate pickling/serialization

### The Fetcher Pattern Solution

```python
class IoFreePool:
    def __init__(self, address, rate_fetcher: RateFetcher):
        self.address = address
        self._rate_fetcher = rate_fetcher  # Injected callback
    
    def get_rate(self, block_number):
        # Pure call: no I/O knowledge, just delegates to callback
        return self._rate_fetcher(block_number)
```

**Benefits:**
1. **Testability**: Pass a lambda/fake function; no mocking needed
2. **Clean separation**: Pool logic is pure; I/O lives in callbacks
3. **Flexibility**: Fetchers can be sync or async; caller decides
4. **Serialization**: Fetchers are just callables (or None); easy to pickle

## Architecture

### Layers

```
┌───────────────────────────────────────┐
│  Client Code (e.g., Arbitrage Cycle)  │
│  - Calls pool methods                 │
│  - Doesn't see fetcher calls          │
├───────────────────────────────────────┤
│  Pool Class (e.g., CurveStableswapPool)│
│  - Pure swap calculation logic         │
│  - Calls fetcher callbacks on-demand   │
│  - No provider/connection imports      │
├───────────────────────────────────────┤
│  Fetcher Protocols (types.py)          │
│  - RateFetcher, VirtualPriceFetcher, etc│
│  - Pure type signatures                │
├───────────────────────────────────────┤
│  Fetcher Factories (e.g., Bot.build_pool)│
│  - Create closures capturing provider  │
│  - Handle I/O, error handling, caching  │
│  - Return callables matching protocols  │
└───────────────────────────────────────┘
```

### Protocol Definitions

Fetchers are `Protocol` types (structural subtyping), not ABCs:

```python
class RateFetcher(Protocol):
    """Fetch rates for lending tokens at a given block."""
    def __call__(self, block_number: int) -> tuple[int, ...]: ...

class VirtualPriceFetcher(Protocol):
    """Fetch virtual price from a base pool."""
    def __call__(self, block_number: int) -> int: ...
```

**Why Protocols?**
- No inheritance required
- Any callable with matching signature works
- Natural fit for closure-based implementations

### Fetcher Factory Pattern

`Bot.build_curve_pool()` creates fetcher closures:

```python
def build_curve_pool(self, pool_address):
    provider = self.get_provider_for_chain(chain_id)
    
    # Create fetcher closures
    def rate_fetcher(block_number: int) -> tuple[int, ...]:
        # Provider captured in closure
        results = []
        for i, token in enumerate(lending_tokens):
            rate = provider.call(token.address, "exchangeRateStored", block_number)
            results.append(rate)
        return tuple(results)
    
    def virtual_price_fetcher(block_number: int) -> int:
        return provider.call(base_pool_address, "get_virtual_price", block_number)
    
    # Inject fetchers into pool
    return CurveStableswapPool(
        address=pool_address,
        rate_fetcher=rate_fetcher,
        virtual_price_fetcher=virtual_price_fetcher,
        ...
    )
```

**Key insight**: The pool never touches `provider` or `connection_manager`. I/O lives in the injected closures.

## Migration Status

### Completed

- **Curve StableSwap Pools** — fully I/O-free with fetcher callbacks (ADR-001 Phase 1–2)
- **All pool construction** — `Bot.build_*_pool()` methods fetch data from DB/RPC and pass values to pool constructors; no provider references on pool objects after construction
- **Builder extraction** — pool construction I/O has been extracted from `Bot` into typed builder classes (`V2PoolBuilder`, `V3PoolBuilder`, `V4PoolBuilder`, `CurvePoolBuilder`, `Erc20Builder`)
- **V2/V3/V4/Aerodrome pool classes** — all `ProviderAdapter`-taking methods removed; I/O for construction and updates lives entirely in builders (ADR-001 Phase 3 complete, Plan 017)

## Migration Guide

### Converting from Direct I/O to Fetchers

**Before (Direct I/O):**
```python
class PoolWithIo:
    def __init__(self, address):
        self.address = address
        self._provider = _get_provider_for_chain(self.chain_id)  # ❌ Direct I/O
    
    def _get_stored_rates(self, block_number) -> tuple[int, ...]:
        rates = []
        for i, token in enumerate(self.tokens):
            if token.is_lending:
                # ❌ Pool doing I/O directly
                rate = self._provider.call(token.address, "exchangeRateStored", block_number)
                rates.append(rate)
        return tuple(rates)
```

**After (Fetcher Pattern):**
```python
class IoFreePool:
    def __init__(self, address, rate_fetcher: RateFetcher | None = None):
        self.address = address
        self._rate_fetcher = rate_fetcher  # ✅ Injected callback
    
    def _get_stored_rates(self, block_number) -> tuple[int, ...]:
        if self._rate_fetcher is None:
            raise MissingCurveData("No rate_fetcher provided")
        # ✅ Pure delegation: pool doesn't know about providers
        return self._rate_fetcher(block_number)
```

**Builder-side (Bot delegates to builders, not pools):**
```python
# Builders own the I/O choreography
# Bot.create_builder() injects connections and db into builders
# Builders call providers, construct pools with pure data

# V2 construction: builder fetches from DB/RPC, pool receives pure values
class V2PoolBuilder:
    def build(self, pool_address, *, chain_id, ...):
        provider = self._connections.get_provider(chain_id)  # Builder handles I/O
        ...
        pool = UniswapV2Pool(
            address=pool_address,
            token0=token0, token1=token1,
            reserves_token0=reserves0, reserves_token1=reserves1,
            # No provider reference passed to pool
        )

# Curve construction: builder creates fetcher closures, injects into pool
class CurvePoolBuilder:
    def build(self, address, *, chain_id, ...):
        fetchers = CurveFetcherFactory(connections=self._connections, chain_id=chain_id)
        pool = CurveStableswapPool(
            ...,
            virtual_price_fetcher=fetchers.virtual_price_fetcher(pool_address),
            timestamp_fetcher=fetchers.timestamp_fetcher(),
            # Fetchers are closures — pool calls them, doesn't know about providers
        )
```

## When to Use

### Use I/O-Free Architecture When:

1. **Testing matters**: You need deterministic unit tests without network
2. **Multiple I/O sites**: Pool needs rates, virtual prices, timestamps, balances, etc.
3. **Cross-cutting concerns**: A single pool type needs different fetch strategies (e.g., cached vs real-time)
4. **Separation of concerns**: Core logic should be pure, testable
5. **Async complexity**: Pool doesn't care if fetcher is sync or async (caller manages)

### Keep Direct I/O When:

1. **Single I/O call**: Constructor only needs one thing
2. **Simple fetch**: No complex error handling or caching needed
3. **Performance**: Avoids function call overhead (marginal in practice)

## Testing with Fake Fetchers

```python
def test_pool_calculation():
    # Pure test: no mocking, no providers
    pool = IoFreePool(
        address="0x1234...",
        rate_fetcher=lambda block: (PRECISION, PRECISION * 2),  # Fake rates
        virtual_price_fetcher=lambda block: PRECISION + PRECISION // 100,  # Fake VP
        timestamp_fetcher=lambda block: 1234567890,
    )
    
    # Test calculation logic only
    result = pool.calculate_swap(amount_in=1000_000, token_in=0, token_out=1, block=100)
    assert result == expected_amount
```

## Error Handling

Fetchers should handle I/O errors and return appropriate values or raise domain exceptions:

```python
def rate_fetcher(block_number: int) -> tuple[int, ...]:
    try:
        return provider.call(...)
    except ContractLogicError as e:
        # Convert to domain exception
        raise MissingCurveData(f"Rate fetch failed at block {block_number}") from e
```

The pool catches `MissingCurveData` (not `ContractLogicError` or `ConnectionError`), keeping pool logic provider-agnostic.

## Curve-Specific Implementation

The Curve I/O-free refactor introduced these fetchers:

| Fetcher | Purpose | When Injected |
|---------|---------|---------------|
| `RateFetcher` | Lending token rates (cToken/yToken) | Pool has `is_lending=True` tokens |
| `VirtualPriceFetcher` | Base pool virtual price | Pool is a metapool |
| `TimestampFetcher` | Block timestamps for A ramping | Pool has ramping A coefficient |
| `RedemptionPriceFetcher` | LSD redemption price | Pool wraps stETH, frxETH, etc. |
| `AdminBalancesFetcher` | Admin fee balances | Pool tracks admin fees |

See `src/degenbot/curve/types.py` for protocol definitions.

## Related Pool Types

### V2/V3/V4/Aerodrome Pools

These pool types are fully I/O-free — builders fetch all construction data from DB/RPC and pass values to pool constructors, and the update path calls providers directly in the builder (not via pool methods). No pool class imports `ProviderAdapter` or carries provider-dependent methods.

### Solver Cache Integration

Pools participating in the Rust solver cache implement the `CacheablePool` protocol with `reserves_for_cache()` and `fee_for_cache()` methods. This replaces `getattr`-based introspection in the adapter with explicit protocol methods (Plan 019).

## References

- `src/degenbot/curve/CONTEXT.md` — Curve domain terminology
- `src/degenbot/curve/types.py` — Fetcher protocol definitions
- `src/degenbot/types/pool_protocols.py` — Pool simulation and cacheable protocols
- `plans/completed/017-v2-v3-io-free-migration.md` — Plan to complete ADR-001 Phase 3 (complete)
- `plans/019-pool-cache-adapter-protocol.md` — CacheablePool protocol plan
- `docs/adr/ADR-001-io-free-pools.md` — ADR-001 (I/O-free pools)

---

*Last updated: 2026-05-11*
