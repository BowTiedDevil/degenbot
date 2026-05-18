# Plan 048: Unify Bot and AsyncBot via Builder-Backed IO Seam

## Overview

AsyncBot (965 lines) duplicates nearly all pool construction, type resolution, and
factory-fetching logic from Bot (840 lines). The two differ only in whether they call
`provider.call()` or `await async_provider.call()`. This plan introduces a `PoolIO`
seam — a protocol for the I/O primitives that builders perform — so builders can be
reused by both Bot and AsyncBot. AsyncBot collapses from an independent construction
monolith to a thin async wrapper over the same builders Bot uses.

## Files Involved

**Primary:**
- `src/degenbot/async_bot.py` — decompose into builder-delegating methods; remove duplicated construction logic
- `src/degenbot/bot.py` — extract `PoolIO` from provider calls used in builders
- `src/degenbot/builders/v2_pool_builder.py` — accept `PoolIO` instead of `ConnectionManager`
- `src/degenbot/builders/v2_builder_base.py` — same
- `src/degenbot/builders/v3_pool_builder.py` — same
- `src/degenbot/builders/v4_pool_builder.py` — same
- `src/degenbot/builders/aerodrome_v2_builder.py` — same
- `src/degenbot/builders/camelot_builder.py` — same
- `src/degenbot/builders/curve_pool_builder.py` — same
- `src/degenbot/builders/erc20_builder.py` — same

**Secondary:**
- `src/degenbot/builders/protocol.py` — add `AsyncPoolBuilder` protocol or extend `PoolBuilder` with async `build`/`update`
- `src/degenbot/curve/fetcher_factory.py` — accept `PoolIO` instead of `ConnectionManager`
- `src/degenbot/connection/connection_manager.py` — potentially expose `PoolIO` factory methods

**Tests:**
- `tests/test_async_bot.py` — update to use `build_pool()` instead of `build_v2_pool()` etc.
- `tests/builders/` — add async builder tests reusing same builder classes with `AsyncPoolIO`
- `tests/test_bot.py` — verify no regression

## Problem

AsyncBot does not use builders. It inlines the full I/O choreography for every pool type:

```
Bot → V2PoolBuilder.build() → _fetch_v2_common_data() → pool constructor
AsyncBot → build_v2_pool() → inline DB lookup → async RPC → inline decode → pool constructor
```

The builder extraction (Plan 001) only benefited the sync path. The async path still has
the old monolithic construction. This means:

- **Zero locality.** A bug in type resolution is fixed in `Bot._resolve_pool_type()` but
  silently persists in `AsyncBot.build_v2_pool()` (which has its own inline resolution).
- **Feature gap.** `AsyncBot` has no `build_pool()` (universal with type resolution), no
  builder registry, no V4 fast-path, no Curve support. It only has `build_v2_pool()` and
  `build_v3_pool()` with `DeprecationWarning`.
- **Maintenance burden.** Every change to the pool construction pipeline must be applied
  twice — once in builders (sync), once in AsyncBot methods (async).

The deletion test confirms depth is missing: deleting `AsyncBot`'s construction methods
doesn't lose unique behavior — the same logic exists in the builders.

## Solution

### Step 1: Define `PoolIO` protocol for I/O primitives

```python
# src/degenbot/builders/pool_io.py

from __future__ import annotations
from typing import Protocol, runtime_checkable

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress
    from hexbytes import HexBytes
    from web3.types import BlockData, LogReceipt, TxParams, BlockIdentifier


@runtime_checkable
class PoolIO(Protocol):
    """I/O primitives needed by pool builders.

    Encapsulates the RPC surface builders use so they are agnostic
    to sync vs async execution. Two adapters satisfy this protocol:
    SyncPoolIO (wrapping ProviderAdapter) and AsyncPoolIO (wrapping
    AsyncProviderAdapter).
    """

    @property
    def chain_id(self) -> int: ...

    def get_block_number(self) -> int: ...

    def get_block(self, block_identifier: int | str) -> BlockData | None: ...

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes: ...

    def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes: ...


@runtime_checkable
class AsyncPoolIO(Protocol):
    """Async counterpart of PoolIO.

    All methods are async. Builders that use AsyncPoolIO must be
    async builders.
    """

    @property
    def chain_id(self) -> int: ...

    async def get_block_number(self) -> int: ...

    async def get_block(self, block_identifier: int | str) -> BlockData | None: ...

    async def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes: ...

    async def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes: ...
```

Key design decisions:
- **`PoolIO` is independent from `ProviderAdapter`/`AsyncProviderAdapter`.** Not a
  subclass, not a wrapper. Builders type-annotate against `PoolIO`. Production code
  creates `SyncPoolIO(provider_adapter)` or `AsyncPoolIO(async_provider_adapter)`.
- **Not every `ProviderAdapter` method is in `PoolIO`.** Only the methods builders
  actually call: `call`, `call_raw`, `get_block_number`, `get_block`. Subscription
  methods, log fetching, and Web3 helpers stay on the adapter.
- **`PoolIO` is a protocol, not an ABC.** Same design as `ProviderBackend` (ADR via
  Plan 042). Runtime checkable; satisfied by any object with the right methods.

### Step 2: Provide `SyncPoolIO` and `AsyncPoolIO` adapters

```python
# src/degenbot/builders/pool_io.py (continued)


class SyncPoolIO:
    """PoolIO adapter wrapping a sync ProviderAdapter."""

    def __init__(self, provider: ProviderAdapter, chain_id: int) -> None:
        self._provider = provider
        self._chain_id = chain_id

    @property
    def chain_id(self) -> int:
        return self._chain_id

    def get_block_number(self) -> int:
        return self._provider.get_block_number()

    def get_block(self, block_identifier: int | str) -> BlockData | None:
        return self._provider.get_block(block_identifier)

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        return self._provider.call(to=to, data=data, block=block)

    def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes:
        return self._provider.call_raw(tx, block=block)


class AsyncPoolIO:
    """PoolIO adapter wrapping an AsyncProviderAdapter.

    All methods are async. Used by AsyncBot's async builders.
    """

    def __init__(self, provider: AsyncProviderAdapter, chain_id: int) -> None:
        self._provider = provider
        self._chain_id = chain_id

    @property
    def chain_id(self) -> int:
        return self._chain_id

    async def get_block_number(self) -> int:
        return await self._provider.get_block_number()

    async def get_block(self, block_identifier: int | str) -> BlockData | None:
        return await self._provider.get_block(block_identifier)

    async def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        return await self._provider.call(to=to, data=data, block=block)

    async def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes:
        return await self._provider.call_raw(tx, block=block)
```

### Step 3: Parameterize builders on `PoolIO` instead of `ConnectionManager`

```python
# Before (V2BuilderBase):
class V2BuilderBase:
    def __init__(self, *, connections: ConnectionManager, db, pools, tokens, erc20_builder):
        self._connections = connections

# After:
class V2BuilderBase:
    def __init__(self, *, db, pools, tokens, erc20_builder):
        # connections removed — io is provided per-call

    def build(self, address, *, chain_id, io: PoolIO, ...):
        # io is provided by the caller (Bot or AsyncBot)
        # No more self._connections.get_provider(chain_id)
```

But this changes the builder protocol's `build()` signature, which Bot already calls.
To minimize disruption, we use a **two-phase approach**:

**Phase A**: Builders accept `ConnectionManager` at construction AND `PoolIO` per-call.
When `io` is provided to `build()`, use it. Otherwise fall back to
`self._connections.get_provider(chain_id)`. This lets Bot migrate incrementally.

**Phase B**: Once both Bot and AsyncBot pass `io`, remove `ConnectionManager` from
builder constructors.

Actually, a cleaner approach: builders keep their current constructor signature
(accepting `ConnectionManager` for backward compat and internal fallback), but
`build()` and `update()` accept an optional `io: PoolIO` kwarg. When provided,
the builder uses `io` instead of `self._connections`.

### Step 4: Add `AsyncPoolBuilder` protocol

```python
# src/degenbot/builders/protocol.py (extended)


class AsyncPoolBuilder(Protocol):
    """Async counterpart of PoolBuilder.

    Satisfies the same interface but with async build/update methods.
    Used by AsyncBot.
    """

    async def build(
        self,
        address: str,
        *,
        chain_id: int | None = None,
        state_block: int | None = None,
        silent: bool = False,
        io: AsyncPoolIO | None = None,
        **kwargs: Any,
    ) -> AbstractLiquidityPool: ...

    async def update(
        self,
        pool: AbstractLiquidityPool,
        *,
        block_number: int | None = None,
        io: AsyncPoolIO | None = None,
    ) -> bool: ...
```

Each builder gains an async `build_async()` / `update_async()` method (or the builder
class itself implements both sync and async paths). The simpler approach: **create
parallel async builder classes** that share the same construction logic but call
`await io.call()` instead of `io.call()`.

But this reintroduces duplication. Better approach: **builders are logic-only** (pure
I/O choreography), parameterized on `PoolIO` (sync) or `AsyncPoolIO` (async). The
choreography is identical — only the call syntax differs. Use a decorator or helper
to abstract over sync/async:

**Simplest viable approach**: Each builder keeps its existing sync `build()`/`update()`.
A new `Async*Builder` wrapper delegates to the same construction logic using `AsyncPoolIO`.
The construction logic (decode steps, registry lookups, token construction) is extracted
into shared helper methods that receive the decoded results rather than performing the
calls themselves.

### Step 5: Refactor AsyncBot to delegate to async builders

```python
# Before: AsyncBot.build_v2_pool() is 120 lines of inline I/O
# After:

class AsyncBot:
    async def build_pool(self, address, *, chain_id=None, ...):
        # Same dispatch logic as Bot.build_pool()
        # Type resolution is shared (static method or module function)
        pool_type = self._resolve_pool_type(address, chain_id=chain_id)
        builder = self._async_builders[type_for_descriptor(pool_type)]
        io = AsyncPoolIO(self.connections.get_provider(chain_id), chain_id)
        return await builder.build(address, chain_id=chain_id, io=io, ...)
```

AsyncBot becomes ~200 lines: constructor, `build_pool()`, `build_erc20token()`,
`update()`, and `start_listening()`. All construction logic lives in builders.

## Implementation Order

### Phase 1: PoolIO protocol + SyncPoolIO adapter (no behavior change)

1. Create `src/degenbot/builders/pool_io.py` with `PoolIO`, `AsyncPoolIO`, `SyncPoolIO`,
   `AsyncPoolIO`
2. `SyncPoolIO` wraps `ProviderAdapter` — trivial delegation
3. `AsyncPoolIO` wraps `AsyncProviderAdapter` — trivial async delegation
4. Add `io: PoolIO | None = None` parameter to `V2BuilderBase._fetch_v2_common_data()`
   When `io` is provided, use it. Otherwise use the existing `provider` parameter.
5. Run tests — zero regression

### Phase 2: Wire Bot to pass SyncPoolIO to builders (no behavior change)

6. `Bot.build_pool()` creates `SyncPoolIO(provider, chain_id)` and passes `io=...` to
   `builder.build()`
7. Each builder uses `io` when provided, falls back to `self._connections` otherwise
8. Run tests — zero regression
9. Verify `io` path is used by adding a `SyncPoolIO.call_count` for testing

### Phase 3: Create async builder wrappers

10. Create `src/degenbot/builders/async_v2_pool_builder.py` — same construction logic
    as `V2PoolBuilder` but using `await io.call()` instead of `io.call()`
11. Create analogous async builders for V3, V4, ERC20, Curve, Aerodrome, Camelot
12. Extract shared construction logic (decode steps, registry lookups) into helper
    functions on the base classes so sync and async builders share the decode/construct
    path
13. Add `AsyncPoolBuilder` protocol
14. Test async builders in isolation with `FakeAsyncPoolIO`

### Phase 4: Refactor AsyncBot

15. Add `build_pool()` to AsyncBot — same dispatch as Bot but with async builders
16. Add `update()` to AsyncBot — async builder lookup
17. Add `build_erc20token()` delegation to async ERC20 builder
18. Remove `build_v2_pool()`, `build_v3_pool()` from AsyncBot (they were already
    deprecated)
19. Run all async tests

### Phase 5: Shared type resolution

20. Extract `_resolve_pool_type()`, `_resolve_pool_type_by_probing()`,
    `_pool_class_for_descriptor()`, `_fetch_factory_from_chain()` into a standalone
    module (e.g., `src/degenbot/builders/type_resolution.py`)
21. Both Bot and AsyncBot import from this module
22. Run all tests

### Phase 6: Remove ConnectionManager from builder constructors

23. Remove `connections: ConnectionManager` from all builder `__init__` signatures
24. Builders receive `io` on every `build()`/`update()` call — no fallback
25. Update Bot and all tests
26. Run all tests

## Benefits

- **~965 → ~200 lines in AsyncBot.** Pool construction logic moves to builders. AsyncBot
  becomes a thin async facade.
- **Locality in type resolution.** A bug in pool type detection is one fix, not two.
- **Builder architecture benefits async.** Async users get `build_pool()` with automatic
  type resolution, builder registry, V4 support, Curve support — currently all missing.
- **Testing.** Builders can be tested with `FakeAsyncPoolIO` for full async coverage
  without a live RPC endpoint. Tests no longer need `AsyncWeb3` instances.
- **Feature parity.** AsyncBot gains `build_pool()`, `update()`, and all pool types
  that Bot supports

## Risks

- **Sync/async builder divergence.** If sync and async builders are separate classes,
  they could diverge. Mitigated by extracting shared decode/construct helpers into base
  classes. The only difference between sync and async builders is `io.call()` vs
  `await io.call()`.
- **Scope.** This plan touches Bot, AsyncBot, every builder, and CurveFetcherFactory.
  The phased approach (additive first, then migrate) limits blast radius per phase.
- **Protocol vs ABC tradeoff.** `PoolIO` as a protocol means `mypy` can't verify that
  a builder only calls methods on the protocol. But `SyncPoolIO` and `AsyncPoolIO`
  satisfy the protocol by construction, so the risk is theoretical.
- **AsyncPoolIO is not a subtype of PoolIO.** They have the same method names but
  different sync/async signatures. They cannot share a base type. This is correct:
  a sync builder can't use an async IO, and vice versa. The builder's type annotation
  pins which one it expects.

## Relationship to Other Plans

- **Plan 001** (Extract Pool Builders): This plan extends builder reuse from sync-only
  to sync+async. The builder extraction is the foundation.
- **Plan 042** (Collapse Provider Adapter Mirror): The `ProviderBackend` protocol
  established the pattern of protocol-based IO abstraction. `PoolIO` follows the same
  approach but at a different seam (builder-facing instead of adapter-facing).
- **ADR-001** (I/O-Free Pools): Continued. Builders become I/O-parameterized — the
  pool objects they produce remain I/O-free regardless of which PoolIO adapter was used.
- **Plan 014** (Async REPL): AsyncBot is the foundation for the async REPL. This plan
  makes AsyncBot feature-complete, which Plan 014 depends on.
