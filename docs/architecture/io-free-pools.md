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

**Bot-side changes:**
```python
# Before: Just pass address
def build_pool(self, address):
    return PoolWithIo(address)

# After: Create and inject fetchers
def build_pool(self, address):
    provider = self.get_provider_for_chain(chain_id)  # Bot handles I/O
    
    def make_rate_fetcher():
        def fetcher(block_number):
            return provider.call(...)  # I/O in fetcher, not pool
        return fetcher
    
    return IoFreePool(address, rate_fetcher=make_rate_fetcher())
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

## References

- `src/degenbot/curve/CONTEXT.md` — Curve domain terminology
- `src/degenbot/curve/types.py` — Fetcher protocol definitions
- `plans/20-curve-io-free-architecture.md` — Migration status and decisions
- `src/degenbot/types/CONTEXT.md` — I/O-free architecture pattern

---

*Last updated: 2026-05-08*
