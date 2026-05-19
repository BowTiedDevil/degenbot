# I/O-Free Pool Architecture

## Overview

The I/O-free architecture is a design pattern for pool implementations that decouples on-chain data fetching from pool logic. Instead of calling provider methods directly, pools receive a **data provider** (or fetcher callbacks, in earlier versions) at construction time and call them on-demand when data is needed.

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
    def __init__(self, address, data_provider: CurveDataProvider | None = None):
        self.address = address
        self._data_provider = data_provider  # Injected seam
    
    def get_rate(self, block_number):
        # Pure delegation: pool doesn't know about providers
        return self._data_provider.lending_rate(block_number, token_address)
```

**Benefits:**
1. **Testability**: Pass `FakeCurveDataProvider` with fixed return values; no mocking needed
2. **Clean separation**: Pool logic is pure; I/O lives in data provider implementations
3. **Flexibility**: Data providers can be sync or async; caller decides
4. **Serialization**: Data provider is dropped on pickle, reconstructed on unpickle via builder

## Architecture

### Layers

```
┌───────────────────────────────────────┐
│  Client Code (e.g., Arbitrage Cycle)  │
│  - Calls pool methods                 │
│  - Doesn't see data_provider calls     │
├───────────────────────────────────────┤
│  Pool Class (e.g., CurveStableswapPool)│
│  - Pure swap calculation logic         │
│  - Calls data_provider on-demand       │
│  - No provider/connection imports      │
├───────────────────────────────────────┤
│  CurveDataProvider Protocol            │
│  - 13 methods: D, gamma, virtual_price,│
│    base_virtual_price, price_scale,    │
│    admin_balances, lending_rate, etc.  │
│  - Single seam replaces 13 fetchers    │
├───────────────────────────────────────┤
│  CurveDataProviderImpl (data_provider_impl)│
│  - Structured class with real methods     │
│  - Handles I/O, error handling, caching   │
│  - Takes ProviderAdapter directly         │
└───────────────────────────────────────┘
```

### Protocol Definitions

Curve uses a single `CurveDataProvider` protocol (structural subtyping), not ABCs:

```python
@runtime_checkable
class CurveDataProvider(Protocol):
    """Single I/O seam for Curve pools."""

    def D(self, block_number: int) -> int: ...
    def gamma(self, block_number: int) -> int: ...
    def virtual_price(self, block_number: int) -> int: ...
    def base_virtual_price(self, block_number: int) -> int: ...
    def price_scale(self, block_number: int) -> tuple[int, ...]: ...
    def admin_balances(self, block_number: int) -> tuple[int, ...]: ...
    def lending_rate(self, block_number: int, token_address: str) -> int: ...
    def redemption_price(self, block_number: int) -> int: ...
    def block_timestamp(self, block_number: int) -> int: ...
    def block_number(self) -> int: ...
    def token_balance(self, block_number: int, token_address: str) -> int: ...
    def token_total_supply(self, block_number: int, token_address: str) -> int: ...
    def is_crypto(self) -> bool: ...
```

**Why Protocols?**
- No inheritance required
- Any object with matching methods works
- Natural fit for `_CurveDataProviderImpl` wrapping existing fetcher closures
- Tests use `FakeCurveDataProvider` with fixed return values

### Data Provider Factory Pattern

The builder creates a `_CurveDataProviderImpl` via the fetcher factory:

```python
def build(self, pool_address):
    # Create data provider (structured class, not closure factory)
    data_provider = CurveDataProviderImpl(
        provider=ProviderAdapter(...),
        pool_address=pool_address,
        ...,
    )
    
    # Inject single data_provider into pool
    return CurveStableswapPool(
        address=pool_address,
        data_provider=data_provider,  # Single parameter replaces 13 fetcher callbacks
        ...,
    )
```

**Key insight**: The pool never touches `provider` or `connection_manager`. I/O lives in the `_CurveDataProviderImpl` which wraps fetcher closures that capture the provider.

## Migration Status

### Completed

- **Curve StableSwap Pools** — fully I/O-free with `CurveDataProvider` seam (ADR-001 Phase 1–2, Plan 040 collapsed 13 fetchers → 1 data provider)
- **All pool construction** — `Bot.build_*_pool()` methods fetch data from DB/RPC and pass values to pool constructors; no provider references on pool objects after construction
- **Builder extraction** — pool construction I/O has been extracted from `Bot` into typed builder classes (`V2PoolBuilder`, `V3PoolBuilder`, `V4PoolBuilder`, `CurvePoolBuilder`, `Erc20Builder`); V2 variant builders extracted into `V2BuilderBase`, `AerodromeV2Builder`, `CamelotBuilder` (Plan 043)
- **V2/V3/V4/Aerodrome pool classes** — all `ProviderAdapter`-taking methods removed; I/O for construction and updates lives entirely in builders (ADR-001 Phase 3 complete, Plan 017)
- **Curve DyCalculator seam** — 14 `match`/`if` dispatch branches in `get_dy()` replaced by injectable calculator objects; pure math functions in `calculations/stableswap.py` (Plan 039)
- **DyCalculationInputs** — `pool: CurveStableswapPool` parameter in `DyCalculator.calculate()` replaced with `inputs: DyCalculationInputs` frozen dataclass carrying pre-resolved data; 77 SLF001 errors → 0; calculators are pure consumers of pre-resolved data with no private member access (Plan 045)
- **Curve state mixin** — 25 attributes + 22 properties with `_xxx` private pattern; `StableswapPoolState` (Plan 041)
- **ProviderBackend** — merged `EthereumProvider` + `_SyncProviderBackend` → `ProviderBackend` protocol; `__getattr__` dispatch replaces 15× delegation methods (Plan 042); `EthereumProvider` backward-compatibility alias removed (Plan 061); subscription stubs consolidated into `SyncSubscriptionSupport`/`AsyncSubscriptionSupport` mixins (Plan 058)
- **Builder Protocol** — `PoolBuilder` protocol replaces the 4-way union type annotation; `_dispatch_build()` isinstance chain eliminated via `**kwargs` forwarding (Plan 035)
- **Pool → Hop conversion** — each pool's `to_hop_state()` is the single source of truth; `solver_hop_builders.py` deleted (Plan 033)
- **SwapAmounts consolidation** — `input_amount()`/`output_amount()` on `AbstractSwapAmounts`; `build_swap_amount()` on pool classes via `ArbitragePathPool` protocol; `_extract_amount_in/out` deleted (Plan 036)
- **Legacy arbitrage cycles** — moved to `_legacy/` with deprecation warnings; `AbstractArbitrage` and `get_arbitrage_helpers()` deleted (Plan 038)
- **Bot typed builders deprecated** — `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool` emit `DeprecationWarning`; use `build_pool()` (Plan 044). Removed by Plan 059.
- **Functions module** — `functions.py` split into domain-aligned modules: `provider/call_helpers.py`, `provider/log_fetching.py`, `contract/addresses.py`, `calculations/evm_math.py`, `provider/block_helpers.py`; `eip_191_hash` deleted as dead code (Plan 037)
- **CurveDataProviderImpl** — 850-line closure bag (`CurveFetcherFactory`) replaced by structured `CurveDataProviderImpl` (~350 lines) with real methods and shared I/O helpers (Plan 049)
- **Curve on-chain cache** — 10 individual `BoundedCache` fields consolidated into single `CurveOnChainCache` object with try-cache→call-provider→store→return pattern; pool class 1160→988 lines (Plan 054)
- **Deprecated fetcher protocols** — 8 deprecated `*Fetcher` protocol classes deleted from `curve/types.py`; superseded by `CurveDataProvider` (Plan 055)
- **Strategy enum factory methods** — `make_calculator()` on `SwapStyle`, `MetapoolRateStyle`, `MetapoolUnderlyingStyle`; `PoolStrategies` auto-constructs calculators from enum values (Plan 056)
- **Calculation-time I/O** — `_build_calculation_inputs` → `_resolve_calculation_inputs_via_io`, `requires_io_at_calculation_time` property, ADR-001 amended with construction-time vs calculation-time I/O table (Plan 057)
- **Old optimizer hierarchy** — `ArbitrageOptimizer` ABC, `OptimizerResult`/`OptimizerType`, and 7 concrete classes deleted (zero production callers); pure Möbius math extracted to `_mobius_math.py` (Plan 053)

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
    def __init__(self, address, data_provider: CurveDataProvider | None = None):
        self.address = address
        self._data_provider = data_provider  # ✅ Injected seam
    
    def _get_stored_rates(self, block_number) -> tuple[int, ...]:
        if self._data_provider is None:
            raise MissingCurveData("No data_provider provided")
        # ✅ Pure delegation: pool doesn't know about providers
        rates = []
        for token in self.lending_tokens:
            rate = self._data_provider.lending_rate(block_number, token.address)
            rates.append(rate)
        return tuple(rates)
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

# Curve construction: builder creates CurveDataProviderImpl, injects into pool
class CurvePoolBuilder:
    def build(self, address, *, chain_id, ...):
        data_provider = CurveDataProviderImpl(
            provider=ProviderAdapter(...),
            pool_address=address,
            ...,
        )
        pool = CurveStableswapPool(
            ...,
            data_provider=data_provider,  # Single parameter replaces 13 fetcher callbacks
        )
```

## When to Use

### Use I/O-Free Architecture When:

1. **Testing matters**: You need deterministic unit tests without network
2. **Multiple I/O sites**: Pool needs rates, virtual prices, timestamps, balances, etc. (Curve uses a single `CurveDataProvider` with 13 methods instead of 13 separate callbacks)
3. **Cross-cutting concerns**: A single pool type needs different fetch strategies (e.g., cached vs real-time)
4. **Separation of concerns**: Core logic should be pure, testable
5. **Async complexity**: Pool doesn't care if fetcher is sync or async (caller manages)

### Keep Direct I/O When:

1. **Single I/O call**: Constructor only needs one thing
2. **Simple fetch**: No complex error handling or caching needed
3. **Performance**: Avoids function call overhead (marginal in practice)

## Testing with Fake Data Providers

```python
def test_pool_calculation():
    # Pure test: no mocking, no providers
    from degenbot.curve.types import CurveDataProvider

    class FakeCurveDataProvider:
        """Fixed-return test double for CurveDataProvider."""

        def D(self, block_number):
            return 3 * 10**18

        def gamma(self, block_number):
            return 10**18

        def virtual_price(self, block_number):
            return 10**18 + 10**16

        def base_virtual_price(self, block_number):
            return 10**18

        def price_scale(self, block_number):
            return (10**18,)

        def admin_balances(self, block_number):
            return (0, 0, 0)

        def lending_rate(self, block_number, token_address):
            return 10**18

        def redemption_price(self, block_number):
            return 10**18

        def block_timestamp(self, block_number):
            return 1234567890

        def block_number(self):
            return 100

        def token_balance(self, block_number, token_address):
            return 10**18

        def token_total_supply(self, block_number, token_address):
            return 10**18

        def is_crypto(self):
            return False

    pool = CurveStableswapPool(
        address="0x1234...",
        data_provider=FakeCurveDataProvider(),
        A=1000,
        tokens=[FAKE_DAI, FAKE_USDC],
    )

    # Test calculation logic only
    result = pool.calculate_swap(amount_in=1000_000, token_in=0, token_out=1, block=100)
    assert result == expected_amount
```

## Error Handling

Fetchers (accessed via `CurveDataProvider` methods) should handle I/O errors and return appropriate values or raise domain exceptions:

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

Curve pools use a **CurveDataProvider** seam — a single `@runtime_checkable` protocol with 13 methods that replaces the former 13 individual fetcher callback parameters. The pool calls `self._data_provider.xxx()` on-demand; the builder creates a `_CurveDataProviderImpl` that wraps existing fetcher closures.

Calculators receive a **DyCalculationInputs** frozen dataclass instead of the pool object. The pool's `get_dy()` performs all I/O (rate resolution, cache lookups, block data, invariant solver closure construction) before constructing a `DyCalculationInputs` and passing it to the calculator. This eliminates all private member access from calculators — they are pure math consumers of pre-resolved data (Plan 045).

On-chain data caches are consolidated into a single **CurveOnChainCache** object that owns all per-block `BoundedCache` instances and provides accessor methods with the try-cache→call-provider→store→return pattern (Plan 054). The pool class no longer has 10 individual cache fields.

Each strategy enum (`SwapStyle`, `MetapoolRateStyle`, `MetapoolUnderlyingStyle`) has a **`make_calculator()`** factory method that returns the matching `DyCalculator` instance. `PoolStrategies` auto-constructs calculators from enum values in `__post_init__` via these factory methods; explicitly-passed calculator arguments are preserved (Plan 056).

| Method | Purpose | When Needed |
|--------|---------|-------------|
| `D()` | On-chain invariant D value | Crypto pools |
| `gamma()` | Gamma parameter | Crypto pools |
| `virtual_price()` | Base pool virtual price | Metapools |
| `base_virtual_price()` | Base pool virtual price (alternate) | Metapools |
| `price_scale()` | On-chain price_scale values | Crypto pools |
| `admin_balances()` | Admin fee balances | Pools tracking admin fees |
| `lending_rate()` | Per-token lending rates | Lending pools |
| `redemption_price()` | LSD redemption price | Pools wrapping stETH, frxETH |
| `block_timestamp()` | Block timestamps for A ramping | Pools with ramping A |
| `block_number()` | Current block number | All pools |
| `token_balance()` | Token balance at block | Metapools |
| `token_total_supply()` | Token total supply | Metapools |
| `is_crypto()` | Whether pool uses CryptoSwap | All pools (flag) |

See `src/degenbot/curve/types.py` for protocol definition and `src/degenbot/curve/data_provider_impl.py` for `CurveDataProviderImpl`.

## Related Pool Types

### V2/V3/V4/Aerodrome Pools

These pool types are fully I/O-free — builders fetch all construction data from DB/RPC and pass values to pool constructors, and the update path calls providers directly in the builder (not via pool methods). No pool class imports `ProviderAdapter` or carries provider-dependent methods.

### Solver Cache Integration

Pools participating in the Rust solver cache implement the `CacheablePool` protocol with `reserves_for_cache()` and `fee_for_cache()` methods. This replaces `getattr`-based introspection in the adapter with explicit protocol methods (Plan 019).

## References

- `src/degenbot/curve/CONTEXT.md` — Curve domain terminology
- `src/degenbot/curve/types.py` — CurveDataProvider protocol, DyCalculationInputs dataclass, DyCalculator protocol definitions
- `src/degenbot/types/pool_protocols.py` — Pool simulation and cacheable protocols
- `plans/completed/017-v2-v3-io-free-migration.md` — Plan to complete ADR-001 Phase 3 (complete)
- `plans/completed/019-pool-cache-adapter-protocol.md` — CacheablePool protocol plan
- `plans/completed/045-calculator-explicit-data.md` — DyCalculationInputs: replace pool parameter with explicit data
- `docs/adr/ADR-001-io-free-pools.md` — ADR-001 (I/O-free pools)

---

*Last updated: 2026-05-19*
