# Plan 005: Move Curve Fetcher Factories into Curve Module ✅

> **Note**: References to `Bot.build_curve_pool()` and `connections.get_web3()` are historical — these methods were removed by Plan 059. Use `bot.build_pool(address)` and `connections.get_provider()` instead.

## Problem

Bot contains 12 `_make_curve_*` methods totaling ~250 lines (lines 1997–2246). These are closures that capture `chain_id` and `pool_address`, then call `self.connections.get_web3(chain_id)` or `self.connections.get_provider(chain_id)` to perform I/O. They have no callers outside `build_curve_pool()` — they are pure implementation detail of the Curve pool construction path.

But they live in Bot, inflating the session class and forcing anyone reading Curve code to bounce between `curve/` and `bot.py`.

This was already anticipated in ADR-001: the fetcher closures are the I/O side of the I/O-free architecture, and they're supposed to be "injected" by Bot, not to bloat Bot's own class body.

The 12 methods are:

| Method | Lines | Purpose |
|--------|-------|---------|
| `_make_curve_virtual_price_fetcher` | 1997–2027 | Fetches `get_virtual_price()` from base pool or pool itself |
| `_make_curve_base_virtual_price_fetcher` | 2029–2055 | Fetches `base_virtual_price()` from metapool contract |
| `_make_curve_timestamp_fetcher` | 2057–2065 | Fetches block timestamp |
| `_make_curve_redemption_price_fetcher` | 2067–2105 | Fetches redemption price via snap contract |
| `_make_curve_admin_balances_fetcher` | 2107–2136 | Fetches admin balances for all token indices |
| `_make_curve_block_number_fetcher` | 2138–2145 | Fetches current block number |
| `_make_curve_total_supply_fetcher` | 2147–2153 | Fetches token total supply (delegates to Bot) |
| `_make_curve_token_balance_fetcher` | 2155–2163 | Fetches token balance (delegates to Bot) |
| `_make_curve_provider_call` | 2165–2179 | Raw `eth_call` via provider |
| `_make_curve_D_fetcher` | 2181–2200 | Fetches `D()` from crypto pool |
| `_make_curve_gamma_fetcher` | 2202–2221 | Fetches `gamma()` from crypto pool |
| `_make_curve_price_scale_fetcher` | 2223–2246 | Fetches `price_scale(i)` from crypto pool |

Three of these (`_make_curve_total_supply_fetcher`, `_make_curve_token_balance_fetcher`, `_make_curve_block_number_fetcher`) delegate directly back to Bot's own `get_token_total_supply()`, `get_token_balance()`, and `connections.get_provider().get_block_number()`. These indirection layers exist only because the closures are on Bot — a separate factory could call these more directly.

## Solution

Create a `CurveFetcherFactory` class in `src/degenbot/curve/` that takes a `ConnectionManager` (or provider callables) at construction. Bot (or the Curve builder from Plan 001) creates a factory instance and calls its methods.

If Plan 001 is implemented: the factory is used by `CurvePoolBuilder`, not by Bot directly.
If not: the factory is used by `Bot.build_curve_pool()`.

### New module

```
src/degenbot/curve/
├── ...existing files...
└── fetcher_factory.py   # CurveFetcherFactory
```

### Interface

```python
# src/degenbot/curve/fetcher_factory.py

from typing import TYPE_CHECKING, Any

from eth_typing import ChecksumAddress
from web3 import Web3

from degenbot.curve.types import (
    AdminBalancesFetcher,
    DFetcher,
    GammaFetcher,
    PriceScaleFetcher,
    RedemptionPriceFetcher,
    TimestampFetcher,
    VirtualPriceFetcher,
)

if TYPE_CHECKING:
    from degenbot.connection.connection_manager import ConnectionManager
    from degenbot.types.aliases import ChainId


class CurveFetcherFactory:
    """
    Creates fetcher closures for Curve StableSwap pools.

    Each fetcher captures chain_id and optionally pool_address at creation,
    then uses the ConnectionManager to perform I/O when called.

    This factory is created by Bot (or the CurvePoolBuilder) and its
    methods are called during pool construction. The resulting closures
    are injected into the I/O-free CurveStableswapPool.
    """

    def __init__(self, *, connections: ConnectionManager, chain_id: ChainId) -> None:
        self._connections = connections
        self._chain_id = chain_id

    def virtual_price_fetcher(
        self,
        pool_address: ChecksumAddress,
        base_pool_address: ChecksumAddress | None = None,
    ) -> VirtualPriceFetcher:
        """Create a virtual price fetcher closure."""
        target_address = base_pool_address if base_pool_address is not None else pool_address

        def fetcher(block_number: int) -> int:
            w3 = self._connections.get_web3(self._chain_id)
            ...
            return vp

        return fetcher

    def base_virtual_price_fetcher(self, pool_address: ChecksumAddress) -> VirtualPriceFetcher:
        """Create a base virtual price fetcher closure for metapools."""
        ...

    def timestamp_fetcher(self) -> TimestampFetcher:
        """Create a timestamp fetcher closure."""
        ...

    def redemption_price_fetcher(self, pool_address: ChecksumAddress) -> RedemptionPriceFetcher:
        """Create a redemption price fetcher closure."""
        ...

    def admin_balances_fetcher(self, pool_address: ChecksumAddress) -> AdminBalancesFetcher:
        """Create an admin balances fetcher closure."""
        ...

    def block_number_fetcher(self) -> Any:  # BlockNumberFetcher
        """Create a block number fetcher closure."""
        ...

    def total_supply_fetcher(self) -> Any:  # TotalSupplyFetcher
        """Create a total supply fetcher closure."""
        ...

    def token_balance_fetcher(self) -> Any:  # TokenBalanceFetcher
        """Create a token balance fetcher closure."""
        ...

    def provider_call(self) -> Any:  # ProviderCall
        """Create a raw provider.call() closure."""
        ...

    def D_fetcher(self, pool_address: ChecksumAddress) -> DFetcher:
        """Create a D() fetcher closure for crypto pools."""
        ...

    def gamma_fetcher(self, pool_address: ChecksumAddress) -> GammaFetcher:
        """Create a gamma() fetcher closure for crypto pools."""
        ...

    def price_scale_fetcher(self, pool_address: ChecksumAddress, n_coins: int) -> PriceScaleFetcher:
        """Create a price_scale() fetcher closure for crypto pools."""
        ...
```

### Usage in builder or Bot

```python
# In CurvePoolBuilder.build() or Bot.build_curve_pool():
fetchers = CurveFetcherFactory(connections=self._connections, chain_id=chain_id)

pool = CurveStableswapPool(
    address=pool_address,
    tokens=tokens,
    ...
    virtual_price_fetcher=fetchers.virtual_price_fetcher(
        pool_address, base_pool_address=base_pool_address
    ),
    base_virtual_price_fetcher=fetchers.base_virtual_price_fetcher(pool_address),
    timestamp_fetcher=fetchers.timestamp_fetcher(),
    redemption_price_fetcher=fetchers.redemption_price_fetcher(pool_address),
    admin_balances_fetcher=fetchers.admin_balances_fetcher(pool_address),
    block_number_fetcher=fetchers.block_number_fetcher(),
    total_supply_fetcher=fetchers.total_supply_fetcher(),
    token_balance_fetcher=fetchers.token_balance_fetcher(),
    provider_call=fetchers.provider_call(),
    D_fetcher=fetchers.D_fetcher(pool_address) if pool_fee_gamma else None,
    gamma_fetcher=fetchers.gamma_fetcher(pool_address) if pool_fee_gamma else None,
    price_scale_fetcher=fetchers.price_scale_fetcher(pool_address, len(tokens)) if pool_fee_gamma else None,
)
```

### Simplification of delegating fetchers

Three fetchers currently delegate back to Bot methods:

1. `_make_curve_total_supply_fetcher` → calls `self.get_token_total_supply()`
2. `_make_curve_token_balance_fetcher` → calls `self.get_token_balance()`
3. `_make_curve_block_number_fetcher` → calls `provider.get_block_number()`

In the factory, these can be simplified:

```python
def total_supply_fetcher(self) -> Any:
    """Create a total supply fetcher closure."""

    def fetcher(token: Any, *, block_identifier: int | None = None) -> int:
        provider = self._connections.get_provider(self._chain_id)
        (total_supply,) = eth_abi.abi.decode(
            types=["uint256"],
            data=provider.call(
                to=token.address,
                data=Web3.keccak(text="totalSupply()")[:4],
                block=block_identifier,
            ),
        )
        return total_supply

    return fetcher
```

This is slightly more code than the delegating version, but it removes the circular dependency on Bot's own `get_token_total_supply()` method. The factory is self-contained — it only needs `ConnectionManager`, not Bot.

Alternatively, the factory can accept optional helper callables for these:

```python
def __init__(
    self,
    *,
    connections: ConnectionManager,
    chain_id: ChainId,
    token_total_supply_fn: Callable | None = None,
    token_balance_fn: Callable | None = None,
) -> None:
    self._token_total_supply_fn = token_total_supply_fn
    self._token_balance_fn = token_balance_fn
```

This allows Bot to pass its own helper methods if it has caching logic, while keeping the factory self-contained for simple use cases. **Recommendation: start with the self-contained version (inline the RPC calls).** The caching on `Erc20Token` is separate and the fetcher doesn't need to go through Bot to access it.

## Implementation steps

### Phase 1: Create CurveFetcherFactory ✅

1. ~~Create `src/degenbot/curve/fetcher_factory.py`.~~
2. ~~Copy the body of each `_make_curve_*` method from `bot.py` into the corresponding factory method.~~
3. ~~Replace `self.connections` with `self._connections`, `self` (Bot) references with factory attributes.~~
4. ~~Replace `self.get_token_total_supply(token, ...)` with direct provider calls.~~
5. ~~Replace `self.get_token_balance(token, ...)` with direct provider calls.~~
6. ~~Type all return types using the existing fetcher protocols from `curve/types.py`.~~

### Phase 2: Wire the factory into Bot (or builder) ✅

7. ~~In `Bot.build_curve_pool()`, create a `CurveFetcherFactory` instance:~~
   ```python
   fetchers = CurveFetcherFactory(connections=self.connections, chain_id=chain_id)
   ```
8. ~~Replace each `self._make_curve_*(...)` call with `fetchers.xyz_fetcher(...)`.~~
9. ~~If Plan 001 is implemented, the factory is created in `CurvePoolBuilder.build()` instead.~~

### Phase 3: Remove methods from Bot ✅

10. ~~Remove all 12 `_make_curve_*` methods from `bot.py`.~~
11. ~~Remove the `from web3.types import TxParams` import — no other usage remains.~~
12. ~~Remove Curve-related imports from `bot.py` that are only used by the fetcher methods.~~

### Phase 4: Tests ✅

13. ~~Add `tests/curve/test_curve_fetcher_factory.py` — 14 tests validating each fetcher with faked ConnectionManager.~~
14. ~~Ensure existing Curve tests still pass — 40 passing.~~
15. ~~Full test suite: 2422 passing.~~

## Metrics

| Metric | Before | After |
|--------|--------|-------|
| Lines of Curve fetcher code in `bot.py` | ~250 | 0 |
| Places to look for Curve fetcher bugs | 2 (bot.py + curve/) | 1 (curve/) |
| Factory reusability | Tied to Bot instance | Usable standalone with any ConnectionManager |
| `bot.py` total lines | ~2437 | ~2183 |

## Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| Caching behavior lost when removing Bot delegation | The `Erc20Token` class has its own balance/supply caching. The factory's `total_supply_fetcher` and `token_balance_fetcher` call the RPC directly, bypassing Bot's `get_token_balance()` which checks `token.get_cached_balance()`. **Mitigation**: The fetchers are used within CurveStableswapPool's internal logic (computing D, virtual price, etc.), not for user-facing balance queries. Curve's own access patterns are time-sensitive (they need the balance at a specific block) and the caching on `Erc20Token` is per-address, not per-pool. Direct RPC is safer here. |
| `provider_call` uses `w3.eth.call()` — w3 vs provider inconsistency | Some fetchers use `provider.call()` (ProviderAdapter), others use `w3.eth.call()` (Web3). The factory receives a `ConnectionManager` and can use either via `connections.get_provider()` or `connections.get_web3()`. This matches the current code — no change in I/O behavior. |

## Dependencies on other plans

- **Plan 001** (Pool builders): This plan can be done independently. If Plan 001 is also implemented, the factory is created and used by `CurvePoolBuilder`, not by Bot directly. The migration is the same — the factory just moves from Bot to the builder.
- **Plan 004** (Update dispatch): The Curve builder from Plan 001 absorbs the `_update_curve_pool()` method. This plan (fetcher factory) is about the 12 fetcher closures used at construction time, not the update path. They're separate migrations that both reduce `bot.py`.
