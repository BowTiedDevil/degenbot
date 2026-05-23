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
                    │  • Builds pools │              │
                    │  • Creates      │              │
                    │  data_providers │              ▼
                    └─────────────────┘     ┌───────────────────┐
                              │             │ CurveDataProvider │
                              │             │  (I/O here)       │
                              ▼             └───────────────────┘
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

### I/O Boundary

| Pool Family | Construction I/O-Free | Calculation I/O-Free | Notes |
|-------------|----------------------|----------------------|-------|
| V2/V3/V4/Aerodrome/Camelot | ✅ | ✅ | Builders fetch all data; pools are pure logic |
| Curve (STANDARD, RAW_BALANCE) | ✅ | ✅ | Rate multipliers are static |
| Curve (lending/crypto/live-admin/metapool) | ✅ | ❌ | `get_dy()` may call `CurveDataProvider` for per-block data |
| Curve (A ramping) | ✅ | ❌ | `_a()` needs `block_timestamp` via data provider |

The `CurveStableswapPool.requires_io_at_calculation_time` property exposes this distinction at runtime. The `_resolve_calculation_inputs_via_io` method name signals that I/O may occur during calculation input resolution.

### Why Data Providers Instead of Dependency Injection?

| Approach | Pros | Cons | Decision |
|----------|------|------|----------|
| **Data provider protocol** (chosen) | Single seam, easy to mock with fakes, coarser-grained than individual callbacks | Slightly larger interface | ✅ Chosen — single point of injection, cleaner pickle, simpler builder code |
| Fetcher callbacks (earlier version) | Simple, no inheritance needed, easy to mock with lambdas | 13 constructor parameters, 13 pickle drops+reconstructs, complex builder | ❌ Replaced by `CurveDataProvider` (Plan 040) |
| Constructor-injected provider | Explicit dependency | Forces provider abstraction into pool signature | ❌ Rejected — pool shouldn't know about providers |
| Abstract base class | Standard OOP | Requires subclassing, overkill for simple fetch | ❌ Rejected — not Pythonic for this use case |
| Event stream | Decoupled, reactive | Complex state synchronization | ❌ Rejected — overkill for current needs |

### Why Builders?

Pool construction requires multiple RPC calls and DB lookups that vary by pool family. A builder class encapsulates this I/O so the pool constructor receives only pre-resolved values. Alternatives considered:

| Approach | Why not |
|----------|---------|
| Class methods (`Pool.from_chain()`) | Class methods can't be swapped or composed; testing requires monkeypatching |
| Factory functions | Work for a single pool type, but don't scale to the builder registry pattern (see ADR-002) |
| Raw constructor calls | Circles back to the original problem — constructors doing I/O |

The `BuilderContext` frozen dataclass (one object per Bot session, passed to all builders) means adding a new pool family requires only a builder class + `register_builder()` — zero wiring changes in Bot.

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
- **Constructor bloat**: Resolved — Curve pool now has a single `data_provider` parameter instead of 13 fetcher callbacks

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
- V2/V3/V4/Aerodrome/Camelot construction is I/O-free (builders fetch data, pass to constructors)
- All `ProviderAdapter`-taking methods removed from pool classes (`get_reserves()`, `get_immutable_pool_values()`, `from_chain` classmethods, etc.)

### Phase 4: Cleanup ✅

Subsequent plans refined the architecture in ways this ADR should record:

- **Fetchers → Data Provider** (Plan 040): Individual fetcher callbacks collapsed
  into a single `CurveDataProvider` seam — see Alternatives table.
- **Typed builders → `build_pool()`** (Plans 044, 059): Per-type builder methods
  replaced by a single `build_pool()` dispatching through the builder registry.
- **DyCalculator gets `DyCalculationInputs`** (Plans 045, 069): The calculator
  protocol changed from accepting a `pool` reference to a frozen dataclass of
  pre-resolved values — eliminating all private-member access from calculators.
- **Per-block cache gets mirror-free design** (Plans 054, 077): Getter methods
  resolve their own dependencies inline instead of requiring a mirrored update call.
- **Provider interface split** (Plans 042, 058, 061): `EthereumProvider` replaced by
  `ProviderBackend` protocol; subscription stubs consolidated into
  `SyncSubscriptionSupport`/`AsyncSubscriptionSupport` mixins.
- **Builder base classes** (Plans 043, 060): V2/Aerodrome/Camelot share
  `V2BuilderBase`; V3/V4 get separate base classes with frozen dataclasses for
  immutable/slot0/DB data. Async builders call the same static methods without
  inheriting.
- **Calculation-time I/O boundary** (Plan 057): `requires_io_at_calculation_time`
  property and `_resolve_calculation_inputs_via_io` method name make the I/O
  boundary explicit (see I/O boundary table below).

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

Integration tests create a `Bot` with a live RPC provider and verify that builder-constructed pools produce results consistent with on-chain state:

```python
def test_curve_pool_live(bot):
    pool = bot.build_pool("0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7")
    assert pool.virtual_price > 0
    # Further assertions against known on-chain values
```

## Amendment: Calculation-Time I/O Boundary

The original decision stated pools are "I/O-free" without qualification. This was precise for construction time but not for calculation time. The I/O boundary table was added to the Decision section to make this distinction structural rather than appended.

## Related Decisions

- **ADR-002**: Pool Type Registry as Module-Level Singleton — the `pool_type_registry` singleton allows DEX modules to self-register at import time, independent of any Bot instance that might use those registrations.
- The `Bot`-as-entry-point pattern and `ConnectionManager` removal are consequences of this ADR, not separate ones.

## References

- [I/O-Free Architecture Doc](../architecture/io-free-pools.md)
- [Curve CONTEXT.md](../../src/degenbot/curve/CONTEXT.md)
- [Uniswap CONTEXT.md](../../src/degenbot/uniswap/CONTEXT.md)
