# ADR-001: I/O-Free Pool Architecture

## Status

**Implemented** — All pool types are I/O-free at construction time. V2/V3/V4/Aerodrome/Camelot pools are also fully I/O-free at calculation time. Curve pools with non-plain swap styles may call `CurveDataProvider` at calculation time for per-block on-chain data (see I/O Status table below).

## Context

Previously, pool classes mixed two concerns:

1. **Pool logic** — swap calculations, state management, tick traversal
2. **I/O** — fetching on-chain data via Web3 providers

This created problems:

- **Impossible to unit test** without a live RPC endpoint
- **Tight coupling** to `ConnectionManager` singleton made parallel tests fragile
- **Slow tests** — every test incurred network round-trips
- **Hidden dependencies** — internal `_get_provider_for_chain()` calls made data flow implicit
- **Hard to simulate** edge cases requiring specific chain states

The singleton pattern (`ConnectionManager.get_instance()`) was particularly problematic:

```python
# OLD: Hidden dependency, impossible to mock cleanly
class CurveStableswapPool:
    def __init__(self, address):
        self.provider = get_connection_manager().get_provider(self.chain_id)
        # ... later, deep in the code ...
        rates = self.provider.w3.eth.call(...)
```

## Decision

Separate I/O from pool logic using **fetcher callbacks** injected at construction.

### Architecture

```
┌─────────────┐     ┌─────────────────┐     ┌─────────────────┐
│   Client    │────▶│  Bot (Session)  │────▶│  Pool Classes   │
│  (User/API) │     │                 │     │  (I/O-free)     │
└─────────────┘     │  • Manages RPC  │     └─────────────────┘
                    │  • Builds pools  │              │
                    │  • Creates       │              │
                    │  data_providers  │              ▼
                    └─────────────────┘     ┌─────────────────┐
                              │              │ CurveDataProvider│
                              │              │  (I/O here)     │
                              ▼              └─────────────────┘
                    ┌─────────────────┐
                    │  Registries     │
                    │  • pools        │
                    │  • tokens       │
                    │  • managed_pools│
                    └─────────────────┘
```

### Fetcher Protocols

A **Data Provider** is an object implementing the `CurveDataProvider` protocol, injected into the pool at construction. The pool calls its methods on-demand; the provider implementation handles the I/O:

```python
from typing import Protocol


class CurveDataProvider(Protocol):
    def D(self, block_number: int) -> int: ...
    def virtual_price(self, block_number: int) -> int: ...
    def lending_rate(self, block_number: int, token_address: str) -> int: ...

    # ... 13 methods total


# Bot creates the _CurveDataProviderImpl (handles I/O)
# Pool just calls data_provider methods (pure logic)
```

### Why Data Providers Instead of Dependency Injection?

| Approach | Pros | Cons | Decision |
|----------|------|------|----------|
| **Data provider protocol** (chosen) | Single seam, easy to mock with fakes, coarser-grained than individual callbacks | Slightly larger interface | ✅ Chosen — single point of injection, cleaner pickle, simpler builder code |
| Fetcher callbacks (earlier version) | Simple, no inheritance needed, easy to mock with lambdas | 13 constructor parameters, 13 pickle drops+reconstructs, complex builder | ❌ Replaced by `CurveDataProvider` (Plan 040) |
| Constructor-injected provider | Explicit dependency | Forces provider abstraction into pool signature | ❌ Rejected — pool shouldn't know about providers |
| Abstract base class | Standard OOP | Requires subclassing, overkill for simple fetch | ❌ Rejected — not Pythonic for this use case |
| Event stream | Decoupled, reactive | Complex state synchronization | ❌ Rejected — overkill for current needs |

## Consequences

### Positive

- **Testable**: Tests pass `lambda: [10**18, 10**18]` as a rate fetcher — no network calls
- **Fast**: Unit tests run in milliseconds instead of seconds
- **Explicit**: I/O locations are visible at construction site (`Bot.build_pool()`)
- **Composable**: Fetchers can chain (cache → provider → retry)
- **Parallel-safe**: No singleton state, tests can run in parallel

### Negative

- **More boilerplate**: `Bot.build_pool()` is longer than direct instantiation
- **Learning curve**: Users must understand the Bot session pattern
- **Migration effort**: All existing pool creation code needs updating
- **Constructor bloat**: Resolved — Curve pool now has a single `data_provider` parameter instead of 13 fetcher callbacks (Plan 040)

## Migration Path

### Phase 1: Fetcher Protocols (Curve) ✅
- Define protocols in `types.py`
- Add fetcher parameters to constructor
- Keep old I/O paths as fallback for backwards compatibility

### Phase 2: Bot Integration ✅
- The Curve Pool Builder creates a data provider and injects it
- Remove provider from pool constructor
- Builder extraction complete (`CurvePoolBuilder`)

### Phase 3: Other Pool Types ✅
- V2/V3/V4/Aerodrome construction is I/O-free (builders fetch data, pass to constructors)
- All `ProviderAdapter`-taking methods removed from pool classes (`get_reserves()`, `get_immutable_pool_values()`, `from_chain` classmethods, etc.)
- Plan 017 complete

### Phase 4: Cleanup ✅
- Typed pool builders (`build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool`) emit `DeprecationWarning` — use `build_pool()` instead (Plan 044). Removed by Plan 059.
- `PoolFamily`/`PoolInvariant` enum naming resolved (Plan 020)
- Curve fetcher callbacks collapsed into single `CurveDataProvider` seam (Plan 040)
- V2 variant builders extracted from `V2PoolBuilder` into per-variant builders (Plan 043); V3/V4 builder base classes with shared pure-logic helpers and frozen dataclasses (Plan 060)
- `ProviderBackend` protocol replaces `EthereumProvider` + `_SyncProviderBackend` mirror (Plan 042); `EthereumProvider` backward-compatibility alias removed (Plan 061); subscription stubs consolidated into `SyncSubscriptionSupport`/`AsyncSubscriptionSupport` mixins (Plan 058)
- DyCalculator `pool` parameter replaced with `DyCalculationInputs` frozen dataclass; 77 SLF001 errors → 0; calculators are pure consumers of pre-resolved data (Plan 045). `DyCalculationInputs` is a pure value object — all fields are ints, tuples, enums, or None (zero callables); calculators call `stableswap_get_y()` / `stableswap_newton_y()` directly with `EVMRevertError` wrapping (Plan 069).
- Old optimizer hierarchy deleted: `ArbitrageOptimizer` ABC, `OptimizerResult`/`OptimizerType`, and 7 concrete classes removed (zero production callers); pure Möbius math extracted to `_mobius_math.py` (Plan 053)
- Curve on-chain caches consolidated into `CurveOnChainCache` (Plan 054), then absorbed back into `CurveStableswapPool` as `_cache_*` fields with `_get_cached_*` accessors (Plan 068)
- Deprecated `*Fetcher` protocol classes deleted from `curve/types.py`; superseded by `CurveDataProvider` (Plan 055)
- Strategy enum factory methods: `make_calculator()` on `SwapStyle`/`MetapoolRateStyle`/`MetapoolUnderlyingStyle`; `PoolStrategies` auto-constructs calculators from enum values (Plan 056)
- Calculation-time I/O boundary documented: `_build_calculation_inputs` → `_resolve_calculation_inputs_via_io`, `requires_io_at_calculation_time` property (Plan 057)
- AsyncBot inline I/O methods collapsed: 4 token/ether balance methods routed through `AsyncErc20Builder` instead of duplicating logic inline; AsyncBot 462→401 lines (Plan 065)
- Type resolution sync/async duplication collapsed: 4 mirror functions → 2 thin wrappers + 2 shared pure functions (`_build_descriptor_from_db_result`, `_descriptor_from_probing_result`); ~56 lines of duplication removed (Plan 066)

## Testing Patterns

### Unit Tests (No I/O)

```python
def test_stableswap_swap():
    pool = CurveStableswapPool(
        address="0x1234...",
        data_provider=FakeCurveDataProvider(),  # Fixed-return test double
        tokens=[FAKE_DAI, FAKE_USDC],
        A=1000,
    )
    result = pool.calculate_tokens_out_from_tokens_in(token_in=FAKE_DAI, token_in_quantity=1000000)
    assert result == expected_amount
```

### Integration Tests (Bot + Live RPC)

```python
def test_curve_pool_live(bot):
    # Bot creates pool with real fetchers
    pool = bot.build_pool("0xbEbc4...")
    # Pool calls fetchers internally on-demand
    assert pool.virtual_price & gt
    0
```

## Amendment: Calculation-Time I/O Boundary

The original ADR stated all pools are "I/O-free." This is precise for construction time (all immutable parameters are provided by builders), but not for calculation time.

### I/O-Free Status by Pool Family

| Pool Family | Construction I/O-Free | Calculation I/O-Free | Notes |
|-------------|----------------------|----------------------|-------|
| V2/V3/V4/Aerodrome/Camelot | ✅ | ✅ | Builders fetch all data; pools are pure logic |
| Curve (STANDARD, RAW_BALANCE) | ✅ | ✅ | Rate multipliers are static |
| Curve (lending/crypto/live-admin/metapool) | ✅ | ❌ | `get_dy()` may call `CurveDataProvider` for per-block data |
| Curve (A ramping) | ✅ | ❌ | `_a()` needs `block_timestamp` via data provider |

The `CurveStableswapPool.requires_io_at_calculation_time` property exposes this distinction at runtime. The `_resolve_calculation_inputs_via_io` method name signals that I/O may occur during calculation input resolution.

## Related Decisions

- **ADR-002** (planned): Removal of `ConnectionManager` singleton
- **ADR-003** (planned): `Bot` as the sole entry point for pool creation

## References

- [I/O-Free Architecture Doc](../architecture/io-free-pools.md)
- [Curve CONTEXT.md](../../src/degenbot/curve/CONTEXT.md)
- [Uniswap CONTEXT.md](../../src/degenbot/uniswap/CONTEXT.md)
