# ADR-001: I/O-Free Pool Architecture

## Status

**Implemented** — Active for Curve StableSwap pools (v2025.05). Planned for Uniswap V2/V3/V4 and other pool types.

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
                    │    fetchers      │              ▼
                    └─────────────────┘     ┌─────────────────┐
                              │              │ Fetcher Closures│
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

A **Fetcher** is a callable injected into the pool at construction. The pool calls it on-demand; the closure handles the I/O:

```python
from collections.abc import Callable
from typing import Protocol

class RateFetcher(Protocol):
    def __call__(self) -&gt; list[int] | list[float]: ...

class VirtualPriceFetcher(Protocol):
    def __call__(self) -&gt; int: ...

# Bot creates the closure (handles I/O)
# Pool just calls it (pure logic)
```

### Why Callbacks Instead of Dependency Injection?

| Approach | Pros | Cons | Decision |
|----------|------|------|----------|
| **Fetcher callbacks** (chosen) | Simple, no inheritance needed, easy to mock with lambdas | Closures capture state less explicitly | ✅ Chosen — matches Python idioms, works with `Protocol` |
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
- **Constructor bloat**: Pool `__init__` gains many optional fetcher parameters

## Migration Path

### Phase 1: Fetcher Protocols (Curve) ✅
- Define protocols in `types.py`
- Add fetcher parameters to constructor
- Keep old I/O paths as fallback for backwards compatibility

### Phase 2: Bot Integration ✅
- `Bot.build_curve_pool()` creates closures and injects them
- Remove provider from pool constructor
- Builder extraction complete (`CurvePoolBuilder`)

### Phase 3: Other Pool Types 🔄
- V2/V3/V4/Aerodrome construction is I/O-free (builders fetch data, pass to constructors)
- Residual `ProviderAdapter`-taking methods still on pool classes (`get_reserves()`, `get_immutable_pool_values()`, `from_chain` classmethods)
- Plan 017 tracks the removal of these methods
- Repeat for Uniswap V2/V3/V4, Aerodrome, etc.

### Phase 4: Cleanup 🔄
- Remove residual provider-dependent methods from pool classes
- Deprecate direct pool instantiation
- Update all tests to use `Fake*` fetchers
- `PoolFamily`/`PoolInvariant` enum naming resolved (Plan 020)

## Testing Patterns

### Unit Tests (No I/O)

```python
def test_stableswap_swap():
    pool = CurveStableswapPool(
        address="0x1234...",
        tokens=[FAKE_DAI, FAKE_USDC],
        A=1000,
        # Inject fetchers directly
        rate_fetcher=lambda: [10**18] * 2,
        virtual_price_fetcher=lambda: 10**18,
    )
    result = pool.calculate_tokens_out_from_tokens_in(
        token_in=FAKE_DAI, token_in_quantity=1000000
    )
    assert result == expected_amount
```

### Integration Tests (Bot + Live RPC)

```python
def test_curve_pool_live(bot):
    # Bot creates pool with real fetchers
    pool = bot.build_curve_pool("0xbEbc4...")
    # Pool calls fetchers internally on-demand
    assert pool.virtual_price &gt; 0
```

## Related Decisions

- **ADR-002** (planned): Removal of `ConnectionManager` singleton
- **ADR-003** (planned): `Bot` as the sole entry point for pool creation

## References

- [I/O-Free Architecture Doc](../architecture/io-free-pools.md)
- [Curve CONTEXT.md](../../src/degenbot/curve/CONTEXT.md)
- [Uniswap CONTEXT.md](../../src/degenbot/uniswap/CONTEXT.md)
