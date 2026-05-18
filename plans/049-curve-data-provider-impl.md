# Plan 049: Replace CurveFetcherFactory Closures with Structured CurveDataProvider Implementation

## Overview

Replace the 850-line `CurveFetcherFactory` — a bag of near-identical closure factories — with a
structured `_CurveDataProviderImpl` class where each `CurveDataProvider` protocol method is a
real method. Shared I/O patterns (call → decode → cast, block-identifier routing, error handling
for reverts) become private helpers, collapsing ~15 near-identical closure patterns into shared
infrastructure.

## Files Involved

**Primary:**
- `src/degenbot/curve/fetcher_factory.py` (851 lines) — convert from closure factory to class-based `_CurveDataProviderImpl`
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — update to use `_CurveDataProviderImpl` directly instead of via factory
- `src/degenbot/builders/curve_pool_builder.py` — update to construct `_CurveDataProviderImpl` instead of calling factory methods

**Secondary:**
- `src/degenbot/curve/types.py` — potentially add `_CurveDataProviderImpl` alongside the existing `CurveDataProvider` protocol
- `src/degenbot/curve/__init__.py` — update exports

**Tests:**
- `tests/curve/` — update any tests that construct fetcher factories; verify `FakeCurveDataProvider` still satisfies protocol
- `tests/fakes/` — no change needed (fakes satisfy the protocol independently)

## Problem

`CurveFetcherFactory` has 15+ methods, each creating a closure with the same pattern:

```python
def some_value_fetcher(self, pool_address: ChecksumAddress) -> Any:
    chain_id = self._chain_id
    def fetcher(block_number: int) -> int:
        provider = self._connections.get_provider(chain_id)
        (result,) = eth_abi.abi.decode(
            types=["uint256"],
            data=provider.call_raw(
                {"to": pool_address, "data": Web3.keccak(text="some_value()")[:4]},
                block=block_number,
            ),
        )
        return cast("int", result)
    return fetcher
```

Repeated 15 times with variations in:
- Target address (pool vs base pool vs token contract)
- Method signature and return types
- Error handling (some catch `ContractLogicError`, some don't)
- Block parameter passing
- Value extraction (`cast("int", result)` vs `int(result)` vs multi-value decode)

Problems with this pattern:
1. **Unreadable stack traces.** A failed fetcher shows `<lambda>` or `fetcher()` with no
   indication of which fetcher failed.
2. **Boilerplate.** Each closure repeats `provider = self._connections.get_provider(chain_id)`,
   the decode pattern, and the cast. ~400 of the 850 lines are structural repetition.
3. **Untestable in isolation.** Testing one fetcher requires constructing the whole factory.
   Individual closures can't be inspected or breakpointed.
4. **Inconsistent error handling.** Some fetchers catch `ContractLogicError`; others don't.
   There's no shared pattern for "call this method, revert returns None."

The deletion test: deleting `CurveFetcherFactory` would scatter the I/O across `_CurveDataProviderImpl`
or back into the pool. But since `_CurveDataProviderImpl` wraps the factory's closures, the factory
*is* the implementation. The proposal makes this explicit.

## Solution

### Step 1: Define `_CurveDataProviderImpl` as a class with real methods

```python
# src/degenbot/curve/data_provider_impl.py

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING, cast

import eth_abi.abi
from web3 import Web3
from web3.exceptions import ContractLogicError

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.types import CurveDataProvider
from degenbot.exceptions.pool import EVMRevertError

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.provider.interface import ProviderAdapter
    from degenbot.types.aliases import ChainId


class _CurveDataProviderImpl:
    """Production implementation of CurveDataProvider.

    Replaces 15+ closure factories from CurveFetcherFactory with real methods.
    Each protocol method is a public method; shared I/O patterns are private helpers.
    """

    def __init__(
        self,
        *,
        provider: ProviderAdapter,
        chain_id: ChainId,
        pool_address: ChecksumAddress,
        base_pool_address: ChecksumAddress | None = None,
    ) -> None:
        self._provider = provider
        self._chain_id = chain_id
        self._pool_address = pool_address
        self._base_pool_address = base_pool_address

    # ── Shared helpers ──

    def _call(
        self,
        to: ChecksumAddress,
        method_sig: str,
        return_types: list[str],
        block_number: int,
    ) -> tuple[Any, ...]:
        """Call a contract method and decode the result.

        Shared by all fetch methods. Handles provider lookup, ABI encoding,
        and decoding in one place.
        """
        data = self._provider.call_raw(
            {"to": to, "data": Web3.keccak(text=method_sig)[:4]},
            block=block_number,
        )
        return eth_abi.abi.decode(types=return_types, data=data)

    def _call_single(
        self,
        to: ChecksumAddress,
        method_sig: str,
        return_type: str,
        block_number: int,
    ) -> Any:
        """Call a method returning a single value. Returns the unwrapped value."""
        (result,) = self._call(to, method_sig, [return_type], block_number)
        return result

    def _call_with_revert_fallback(
        self,
        to: ChecksumAddress,
        method_sig: str,
        return_type: str,
        block_number: int,
        fallback: Any = None,
    ) -> Any:
        """Call a method, returning fallback on revert."""
        try:
            return self._call_single(to, method_sig, return_type, block_number)
        except ContractLogicError:
            return fallback

    # ── CurveDataProvider protocol methods ──

    def D(self, block_number: int) -> int:
        return cast("int", self._call_single(
            self._pool_address, "get_D()", "uint256", block_number
        ))

    def gamma(self, block_number: int) -> int:
        return cast("int", self._call_single(
            self._pool_address, "gamma()", "uint256", block_number
        ))

    def virtual_price(self, block_number: int) -> int:
        target = self._base_pool_address or self._pool_address
        return cast("int", self._call_single(
            target, "get_virtual_price()", "uint256", block_number
        ))

    def base_virtual_price(self, block_number: int) -> int:
        if self._base_pool_address is None:
            msg = "base_virtual_price requires a base pool"
            raise ValueError(msg)
        return cast("int", self._call_single(
            self._base_pool_address, "get_virtual_price()", "uint256", block_number
        ))

    def price_scale(self, block_number: int) -> int:
        return cast("int", self._call_single(
            self._pool_address, "price_scale()", "uint256", block_number
        ))

    def admin_balances(self, block_number: int, index: int) -> int:
        sig = f"admin_balances(uint256)"
        data = Web3.keccak(text=sig)[:4] + eth_abi.abi.encode(["uint256"], [index])
        result = self._provider.call_raw(
            {"to": self._pool_address, "data": data},
            block=block_number,
        )
        (value,) = eth_abi.abi.decode(types=["uint256"], data=result)
        return cast("int", value)

    def lending_rate(self, block_number: int, token_address: str) -> int:
        # Implementation depends on which rate method the pool supports
        # Previously spread across 6 separate fetcher closures
        ...

    def redemption_price(self, block_number: int) -> int:
        return cast("int", self._call_single(
            self._pool_address, "redemption_price()", "uint256", block_number
        ))

    def block_timestamp(self, block_number: int) -> int:
        return self._provider.get_block(block_number)["timestamp"]

    def block_number(self) -> int:
        return self._provider.get_block_number()

    def token_balance(self, block_number: int, token_address: str) -> int:
        checksummed = get_checksum_address(token_address)
        data = Web3.keccak(text="balanceOf(address)")[:4] + eth_abi.abi.encode(
            ["address"], [checksummed]
        )
        result = self._provider.call_raw(
            {"to": checksummed, "data": data},
            block=block_number,
        )
        (value,) = eth_abi.abi.decode(types=["uint256"], data=result)
        return cast("int", value)

    def token_total_supply(self, block_number: int, token_address: str) -> int:
        checksummed = get_checksum_address(token_address)
        data = Web3.keccak(text="totalSupply()")[:4]
        result = self._provider.call_raw(
            {"to": checksummed, "data": data},
            block=block_number,
        )
        (value,) = eth_abi.abi.decode(types=["uint256"], data=result)
        return cast("int", value)

    def is_crypto(self) -> bool:
        # Non-block-number method — probes contract
        try:
            self._call_single(self._pool_address, "price_oracle()", "uint256", 0)
            return True
        except (ContractLogicError, Exception):
            return False
```

### Step 2: Handle lending-rate complexity

The most complex fetcher is the lending-rate fetcher, which varies based on token type
(cToken, yToken, aToken, cyToken, etc.). Currently, the factory has 6 separate methods
for different rate fetcher styles. In the new design:

- `_CurveDataProviderImpl` receives a `LendingRateStyle` at construction
- The `lending_rate()` method dispatches on the style internally
- Each style maps to one of the private `_call_*` helpers with the appropriate method
  signature and target address

```python
def lending_rate(self, block_number: int, token_address: str) -> int:
    match self._lending_rate_style:
        case LendingRateStyle.NONE:
            return self.PRECISION
        case LendingRateStyle.CTOKEN:
            return self._fetch_ctoken_rate(block_number, token_address)
        case LendingRateStyle.YTOKEN:
            return self._fetch_ytoken_rate(block_number, token_address)
        case LendingRateStyle.ATOKEN:
            return self._fetch_atoken_rate(block_number, token_address)
        case LendingRateStyle.CYTOKEN:
            return self._fetch_cytoken_rate(block_number, token_address)
```

Each `_fetch_*_rate` is a 5-line method using `_call_single()` — no closures.

### Step 3: Update `CurvePoolBuilder` to construct `_CurveDataProviderImpl`

```python
# Before:
factory = CurveFetcherFactory(connections=self._connections, chain_id=chain_id)
data_provider = _CurveDataProviderImpl(pool_address, factory)  # wraps closure bag

# After:
provider = self._connections.get_provider(chain_id)
data_provider = _CurveDataProviderImpl(
    provider=provider,
    chain_id=chain_id,
    pool_address=pool_address,
    base_pool_address=base_pool_address,
    lending_rate_style=lending_rate_style,
)
```

The builder already determines the lending rate style, base pool address, etc. during
detection. It passes them to the `DataProviderImpl` constructor.

### Step 4: Delete `CurveFetcherFactory`

After all construction paths use `_CurveDataProviderImpl` directly, the factory is dead
code. Delete it and its module.

### Step 5: Handle multi-value fetchers

Some fetchers return multiple values (e.g., `get_dy()` returns a single int but
`price_oracle(uint256)` returns different values per index). These use `_call()` instead
of `_call_single()`:

```python
def crypto_price_oracle(self, block_number: int, index: int) -> int:
    data = Web3.keccak(text="price_oracle(uint256)")[:4] + eth_abi.abi.encode(["uint256"], [index])
    result = self._provider.call_raw(
        {"to": self._pool_address, "data": data}, block=block_number
    )
    (value,) = eth_abi.abi.decode(types=["uint256"], data=result)
    return cast("int", value)
```

Still fewer lines than the current closure pattern — and the helper methods are shared
across all indexed calls.

## Implementation Order

### Phase 1: Create `_CurveDataProviderImpl` alongside factory (additive)

1. Create `src/degenbot/curve/data_provider_impl.py` with the new class
2. Implement `_call`, `_call_single`, `_call_with_revert_fallback` helpers
3. Implement the 13 `CurveDataProvider` protocol methods
4. Verify `isinstance(_CurveDataProviderImpl(...), CurveDataProvider)` — protocol check
5. Write unit tests with `OfflineProvider` mocking the RPC calls
6. Run existing tests — zero regression (factory still in use)

### Phase 2: Wire `CurvePoolBuilder` to use `_CurveDataProviderImpl`

7. Add construction path in `CurvePoolBuilder.build()` that creates
   `_CurveDataProviderImpl` alongside the factory path
8. Feature flag or parameter to select new vs old path for testing
9. Run all Curve tests with new path — verify identical results
10. Switch default to new path

### Phase 3: Handle lending-rate dispatch

11. Add `LendingRateStyle`-based dispatch to `lending_rate()` method
12. Implement each `_fetch_*_rate` private method
13. Run all Curve lending-pool tests

### Phase 4: Delete factory

14. Remove all references to `CurveFetcherFactory` in builder code
15. Delete `src/degenbot/curve/fetcher_factory.py`
16. Run all tests — zero regression
17. Remove any unused imports

### Phase 5: Clean up

18. Verify `FakeCurveDataProvider` still satisfies `CurveDataProvider` protocol
19. Run `ruff`, `mypy`, full test suite
20. Update `src/degenbot/curve/CONTEXT.md` if terminology changed

## Benefits

- **~850 → ~300 lines.** Shared helpers replace 15 near-identical closure patterns.
  Estimated 400+ lines of boilerplate collapse.
- **Readable stack traces.** Failed calls show `virtual_price()`, `lending_rate()`, etc.
  — not `<lambda>` or `fetcher()`.
- **Individually testable methods.** Construct a `_CurveDataProviderImpl` with an
  `OfflineProvider`, call `D()`, verify the result. No need for a full factory.
- **Concentrated error handling.** `_call()` and `_call_with_revert_fallback()` handle
  provider errors once. A change to how reverts are handled is one edit.
- **Simpler pickle.** `_CurveDataProviderImpl` pickles as one object with 5 attributes
  (provider, chain_id, pool_address, base_pool_address, lending_rate_style) instead of
  13 closures, each capturing different state.

## Risks

- **Lending-rate complexity.** The 6 rate-fetcher styles have subtle differences (cToken
  vs yToken vs aToken method signatures, different fallback values). The match dispatch
  is simpler than 6 closures, but must be tested against real contracts for each style.
- **Base pool target address.** Some methods call the base pool, others call the pool
  itself. The current closures capture the target address at creation time. The new design
  selects the target per-method. This is actually more correct (the target doesn't change
  at runtime) but must be verified.
- **Performance.** Method calls on `self` have slightly different dispatch characteristics
  than closure calls. The overhead is negligible compared to the RPC round-trip.
- **`Any` return types in `_call`.** The helper methods return `Any` because the decoded
  types vary. Callers must cast. This is the same as the current closure pattern.

## Relationship to Other Plans

- **Plan 040** (Curve Data Provider): Collapsed 13 fetcher callbacks into a single
  `CurveDataProvider` seam. This plan deepens the production adapter from "closure bag"
  to structured implementation.
- **Plan 027** (Curve Lending-Rate Fetchers): Removed 6 `_stored_rates_from_*()` methods
  from the pool and added `LendingRateFetcher` protocol. This plan absorbs the rate
  fetcher factory's output into the data provider implementation.
- **Plan 048** (Async Builder Shared): If AsyncBot builds Curve pools, it will need an
  async `_CurveDataProviderImpl`. The class-based design makes adding an async variant
  straightforward (add `AsyncCurveDataProviderImpl` with `await` calls, identical logic).
- **ADR-001** (I/O-Free Pools): The `CurveDataProvider` protocol is the I/O seam. This
  plan improves the production adapter without changing the seam.
