# Plan 048: Unify Bot and AsyncBot via Builder-Backed IO Seam

## Overview

AsyncBot (965 lines) duplicates nearly all pool construction, type resolution, and
factory-fetching logic from Bot (812 lines). The two differ only in whether they call
`provider.call()` or `await async_provider.call()`. This plan introduces a `PoolIO`
seam — a protocol for the I/O primitives that builders perform — so builders can be
reused by both Bot and AsyncBot. AsyncBot's inline construction logic moves to async
builders; AsyncBot itself collapses from 965 to ~160 lines. Total codebase lines
increase slightly (new PoolIO adapters + async builder classes) but the duplication
is eliminated and the maintenance surface shrinks.

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

### Deletion test

If you delete `AsyncBot`'s three `build_*_pool()` methods (546 lines) and
`build_erc20token()` (116 lines), no unique behavior is lost — the same construction
logic exists in `V2PoolBuilder`, `V3PoolBuilder`, `V4PoolBuilder`, and `Erc20Builder`.
The only difference is the sync vs async call syntax. This confirms the duplication is
real: AsyncBot is a monolithic reimplementation of what builders already do.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|-------------|
| V2 construction duplicated verbatim | `async_bot.py:247–382` (136 lines) vs `v2_builder_base.py` + `v2_pool_builder.py` | A bug fix in `_fetch_v2_common_data()` must be applied twice; the async path silently diverges |
| V3 construction duplicated verbatim | `async_bot.py:383–586` (204 lines) vs `v3_pool_builder.py` | Same — tick fetcher, bitmap, liquidity init all re-implemented inline |
| V4 construction duplicated verbatim | `async_bot.py:587–792` (206 lines) vs `v4_pool_builder.py` | Same — PoolManager probing, state view fetch, hook address all re-implemented |
| ERC20 construction duplicated | `async_bot.py:131–246` (116 lines) vs `erc20_builder.py` | Token metadata, decimal detection, DB upsert all re-implemented |
| No `build_pool()` with type resolution | `async_bot.py` — method does not exist | Async users must know the pool type upfront; they can't use the universal entry point that Bot provides |
| No V4 fast-path or Curve support | `async_bot.py` — only `build_v2_pool`, `build_v3_pool`, `build_v4_pool` (deprecated) | Feature gap: async users can't build Curve pools or use `build_pool(address)` auto-detection |
| Type resolution is private to Bot | `bot.py:347–530` — `_resolve_pool_type()`, `_pool_class_for_descriptor()`, `_fetch_factory_from_chain()` | AsyncBot can't reuse this logic; any improvement to type resolution is sync-only |

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

    async def get_block_number(self) -> int: ...

    async def get_block(self, block_identifier: int | str) -> BlockData | None: ...

    async def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes: ...

    async def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes: ...
```

Key design decisions:
- **`PoolIO` is independent from `ProviderAdapter`/`AsyncProviderAdapter`.** Not a
  subclass, not a wrapper. Builders type-annotate against `PoolIO`. Production code
  creates `SyncPoolIO(provider_adapter)` or `AsyncPoolIO(async_provider_adapter)`.
- **`chain_id` is NOT on `PoolIO`.** It's a configuration value, not an I/O operation.
  Builders already receive `chain_id` as a `build()` kwarg from the caller
  (Bot/AsyncBot). Adding it to `PoolIO` would create a redundant source of truth and
  complicate construction (adapters would need `chain_id` injected). Keep it on
  `build()` only.
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

    def __init__(self, provider: ProviderAdapter) -> None:
        self._provider = provider

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

    def __init__(self, provider: AsyncProviderAdapter) -> None:
        self._provider = provider

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

Builders keep their current constructor signature (accepting `ConnectionManager` for
backward compat and internal fallback), but `build()` and `update()` accept an optional
`io: PoolIO` kwarg. When provided, the builder uses `io` instead of
`self._connections`.

This is a two-phase migration:

- **Phase A**: Builders accept `ConnectionManager` at construction AND `PoolIO` per-call.
  When `io` is provided to `build()`, use it. Otherwise fall back to
  `self._connections.get_provider(chain_id)`. Bot migrates incrementally.
- **Phase B**: Once both Bot and AsyncBot pass `io`, remove `ConnectionManager` from
  builder constructors and `BuilderContext`.

### Step 4: Add `AsyncPoolBuilder` protocol and parallel async builders

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

Each sync builder gets a parallel async builder class (e.g., `AsyncV2PoolBuilder`
alongside `V2PoolBuilder`). The async builder calls `await io.call()` instead of
`io.call()`, but all pure logic (decode, resolve, construct) is extracted into
shared helper methods on the base class. See Design Decisions below.

### Design decisions

- **Separate sync/async builder classes, not a single async-everything builder.**
  Making `build()` async on all builders would force async on sync users (Bot, CLI,
  tests). The cost is maintaining two builder classes per pool family. Mitigated by
  extracting pure logic into shared helpers so the async builder's `build()` contains
  only the I/O call sequence (~20 lines), not the decode/construct logic (~60 lines).

- **Shared pure logic, duplicated I/O call sequence.** The V2 builder choreography
  breaks down as follows:

  | Step | Shared? | Why |
  |------|----------|-----|
  | DB lookup (`self._db() as session`) | Yes | No I/O, pure SQLAlchemy |
  | Chain fetch (factory, token0, token1) | No | `io.call()` vs `await io.call()` |
  | Decode raw bytes → typed addresses | Yes | `eth_abi.abi.decode()` is pure |
  | Fetch reserves | No | `raw_call(io, ...)` vs `await io.call()` + decode |
  | Deployer/init_hash resolution | Yes | Registry lookup, no I/O |
  | Token construction | No | `erc20_builder.build()` vs `await async_builder.build()` |
  | Pool construction + registration | Yes | `UniswapV2Pool(...)` + `self._pools.add(...)` |

  The shared steps are extracted into static/class methods on `V2BuilderBase`:

  ```python
  # v2_builder_base.py — shared pure logic

  @staticmethod
  def _extract_db_values(pool_from_db) -> tuple[str, str, str, Fraction, Fraction]:
      """Extract factory, token addresses, and fees from a DB row."""
      factory = get_checksum_address(pool_from_db.exchange.factory)
      token0_address = pool_from_db.token0.address
      token1_address = pool_from_db.token1.address
      if isinstance(pool_from_db, UniswapFeeMixin):
          fee_token0 = Fraction(pool_from_db.fee_token0, pool_from_db.fee_denominator)
          fee_token1 = Fraction(pool_from_db.fee_token1, pool_from_db.fee_denominator)
      else:
          fee_token0 = Fraction(3, 1000)
          fee_token1 = Fraction(3, 1000)
      return factory, token0_address, token1_address, fee_token0, fee_token1

  @staticmethod
  def _decode_immutable_data(
      factory_result: HexBytes,
      token0_result: HexBytes,
      token1_result: HexBytes,
  ) -> tuple[ChecksumAddress, ChecksumAddress, ChecksumAddress]:
      """Decode raw call results into typed addresses."""
      (factory_raw,) = eth_abi.abi.decode(types=["address"], data=factory_result)
      (token0_raw,) = eth_abi.abi.decode(types=["address"], data=token0_result)
      (token1_raw,) = eth_abi.abi.decode(types=["address"], data=token1_result)
      return (
          get_checksum_address(factory_raw),
          get_checksum_address(token0_raw),
          get_checksum_address(token1_raw),
      )

  @staticmethod
  def _resolve_deployment(
      *,
      chain_id: ChainId,
      factory: ChecksumAddress,
      default_init_hash: str,
      deployer_override: str | None,
      init_hash_override: str | None,
  ) -> _ResolvedDeployment:
      """Resolve deployer and init hash from registry + overrides."""
      # ...registry lookup + override logic...
  ```

  The async builder's `build()` then becomes:

  ```python
  # async_v2_pool_builder.py

  class AsyncV2PoolBuilder:
      def __init__(self, ctx: BuilderContext) -> None:
          self._db = ctx.db
          self._pools = ctx.pools
          self._tokens = ctx.tokens
          self._erc20_builder = ctx.erc20_builder  # must be async-capable

      async def build(
          self,
          address: str,
          *,
          chain_id: ChainId,
          state_block: int | None = None,
          silent: bool = False,
          io: AsyncPoolIO | None = None,
          **kwargs: Any,
      ) -> UniswapV2Pool:
          address = get_checksum_address(address)
          io = io or self._default_io(chain_id)  # fallback
          state_block = state_block or await io.get_block_number()

          # DB lookup (pure)
          pool_from_db = self._try_db_lookup(address, chain_id=chain_id)

          if pool_from_db is not None:
              factory, token0_addr, token1_addr, fee0, fee1 = (
                  V2BuilderBase._extract_db_values(pool_from_db)
              )
          else:
              # I/O — async-only (~8 lines)
              factory_result = await io.call(
                  to=address, data=encode_function_calldata("factory()", None)
              )
              token0_result = await io.call(
                  to=address, data=encode_function_calldata("token0()", None)
              )
              token1_result = await io.call(
                  to=address, data=encode_function_calldata("token1()", None)
              )
              factory, token0_addr, token1_addr = (
                  V2BuilderBase._decode_immutable_data(
                      factory_result, token0_result, token1_result
                  )
              )
              fee0 = fee1 = Fraction(3, 1000)

          # Token construction (async I/O)
          token0 = await self._erc20_builder.build(token0_addr, chain_id=chain_id)
          token1 = await self._erc20_builder.build(token1_addr, chain_id=chain_id)

          # Reserves (async I/O + shared decode)
          reserves_result = await io.call(
              to=address,
              data=encode_function_calldata("getReserves()", None),
              block=state_block,
          )
          reserves0, reserves1, _ = eth_abi.abi.decode(
              types=["uint256", "uint256", "uint256"], data=reserves_result
          )

          # Shared pure logic
          deployment = V2BuilderBase._resolve_deployment(
              chain_id=chain_id,
              factory=factory,
              default_init_hash=UniswapV2Pool.UNISWAP_V2_MAINNET_POOL_INIT_HASH,
              deployer_override=kwargs.get("deployer_address"),
              init_hash_override=kwargs.get("init_hash"),
          )

          # Pool construction (pure, shared)
          pool = UniswapV2Pool(
              address=address,
              chain_id=chain_id,
              token0=token0,
              token1=token1,
              factory=factory,
              fee_token0=fee0,
              fee_token1=fee1,
              reserves_token0=reserves0,
              reserves_token1=reserves1,
              deployer_address=deployment.deployer,
              init_hash=deployment.init_hash,
              state_block=state_block,
          )
          self._pools.add(pool_address=pool.address, chain_id=chain_id, pool=pool)
          return pool
  ```

  The async `build()` is ~50 lines, of which ~30 are shared pure logic called
  through base-class helpers and ~20 are the async I/O call sequence that must be
  written per-path. This is the minimum duplication Python's sync/async split
  requires.

- **`PoolIO` and `AsyncPoolIO` are separate protocols.** They share method names but
  have different sync/async signatures. A sync builder can't use an `AsyncPoolIO`,
  and vice versa. This is correct: the builder's type annotation pins which one it
  expects. They do not share a base type.

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
        io = AsyncPoolIO(self.connections.get_provider(chain_id))
        return await builder.build(address, chain_id=chain_id, io=io, ...)
```

AsyncBot becomes ~160 lines: constructor, `add_tracker()`, `build_pool()`, `build_erc20token()`,
async I/O methods (`get_token_balance`, etc.), and `start_listening()`. All
construction logic lives in builders.

## Implementation Order

### Slice 1: PoolIO protocol + SyncPoolIO adapter (no behavior change)

1. Create `src/degenbot/builders/pool_io.py` with `PoolIO`, `AsyncPoolIO`, `SyncPoolIO`,
   `AsyncPoolIO`
2. `SyncPoolIO` wraps `ProviderAdapter` — trivial delegation
3. `AsyncPoolIO` wraps `AsyncProviderAdapter` — trivial async delegation
4. Add `io: PoolIO | None = None` parameter to `V2BuilderBase._fetch_v2_common_data()`
   When `io` is provided, use it. Otherwise use the existing `provider` parameter.
5. Run: `just test-python` — expect zero regression

### Slice 2: Wire Bot to pass SyncPoolIO to builders (no behavior change)

6. `Bot.build_pool()` creates `SyncPoolIO(provider)` and passes `io=...` to
   `builder.build()`
7. Each builder uses `io` when provided, falls back to `self._connections` otherwise
8. Verify `io` path is used by adding a `SyncPoolIO.call_count` for testing
9. Run: `just test-python` — expect zero regression

### Slice 3: Create async builder wrappers

10. Create `src/degenbot/builders/async_v2_pool_builder.py` — same construction logic
    as `V2PoolBuilder` but using `await io.call()` instead of `io.call()`
11. Create analogous async builders for V3, V4, ERC20, Curve, Aerodrome, Camelot
12. Extract shared construction logic (decode steps, registry lookups) into helper
    functions on the base classes so sync and async builders share the decode/construct
    path
13. Add `AsyncPoolBuilder` protocol
14. Test async builders in isolation with `FakeAsyncPoolIO`
15. Run: `just test-python` — expect all builder tests pass (sync + async)

### Slice 4: Refactor AsyncBot

16. Add `build_pool()` to AsyncBot — same dispatch as Bot but with async builders
17. Add `build_erc20token()` delegation to async ERC20 builder
18. Remove `build_v2_pool()`, `build_v3_pool()` from AsyncBot (they were already
    deprecated)
19. Run: `just test-python` — expect async bot tests pass via `build_pool()`

### Slice 5: Shared type resolution

20. Extract `_pool_class_for_descriptor()` into `src/degenbot/builders/type_resolution.py`
    as a standalone function (it is already `@staticmethod` and pure — no I/O).
21. Extract `_resolve_pool_type()`, `_resolve_pool_type_by_probing()`, and
    `_fetch_factory_from_chain()` as functions that accept `io: PoolIO` (sync) or
    `io: AsyncPoolIO` (async). These are the I/O-dependent functions that both
    Bot and AsyncBot need. Signatures become:

    ```python
    # src/degenbot/builders/type_resolution.py

    def resolve_pool_type(
        address: ChecksumAddress,
        *,
        chain_id: ChainId,
        io: PoolIO,
        db: DatabaseSessionManager,
    ) -> PoolTypeDescriptor:
        # DB lookup (pure), then factory fetch (io.call), then probing (io.call)

    async def resolve_pool_type_async(
        address: ChecksumAddress,
        *,
        chain_id: ChainId,
        io: AsyncPoolIO,
        db: DatabaseSessionManager,
    ) -> PoolTypeDescriptor:
        # Same logic, but await io.call() for factory fetch and probing

    def fetch_factory_from_chain(
        address: ChecksumAddress,
        *,
        chain_id: ChainId,
        io: PoolIO,
    ) -> ChecksumAddress | None:
        ...

    async def fetch_factory_from_chain_async(
        address: ChecksumAddress,
        *,
        chain_id: ChainId,
        io: AsyncPoolIO,
    ) -> ChecksumAddress | None:
        ...

    def resolve_pool_type_by_probing(
        address: ChecksumAddress,
        *,
        chain_id: ChainId,
        factory: ChecksumAddress,
        io: PoolIO,
    ) -> PoolTypeDescriptor:
        ...

    async def resolve_pool_type_by_probing_async(
        address: ChecksumAddress,
        *,
        chain_id: ChainId,
        factory: ChecksumAddress,
        io: AsyncPoolIO,
    ) -> PoolTypeDescriptor:
        ...

    def pool_class_for_descriptor(
        pool_type: PoolTypeDescriptor,
        *,
        chain_id: ChainId,
    ) -> type[AbstractLiquidityPool]:
        # Pure — no io parameter needed
        ...
    ```

    Each async variant has the same logic as its sync counterpart, differing
    only in `io.call()` vs `await io.call()`. The pure `pool_class_for_descriptor()`
    has no async counterpart — both paths call the same function.

22. Both Bot and AsyncBot import from this module. Bot passes `SyncPoolIO`,
    AsyncBot passes `AsyncPoolIO`.
23. Run: `just test-python` — expect zero regression

### Slice 6: Remove ConnectionManager from builder constructors

24. Remove `connections: ConnectionManager` from all builder `__init__` signatures
25. Remove `connections` from `BuilderContext` (Plan 051 intersection — see
    Relationship to Other Plans)
26. Builders receive `io` on every `build()`/`update()` call — no fallback
27. Update Bot and all tests
28. Run: `just test-python` — expect zero regression

### Slice 7: Validate and clean up

29. Run: `just lint` + `just test-all`
30. Update `CONTEXT.md` files if terminology changed
31. Remove any deprecated shims introduced during migration

## Testing

### Per-slice test runs

Each slice runs `just test-python`. If a migration requires a compatibility period,
both old and new paths must pass.

### New unit tests

**Slice 1 (PoolIO protocol + SyncPoolIO)**:

```python
# tests/builders/test_pool_io.py


class FakeProvider:
    """Minimal fake satisfying SyncPoolIO's delegate."""

    def __init__(self):
        self.call_count = 0

    def call(self, *, to, data, block=None):
        self.call_count += 1
        return HexBytes(b"\x00" * 32)

    def call_raw(self, tx, block=None):
        return HexBytes(b"\x00" * 32)

    def get_block_number(self):
        return 42

    def get_block(self, block_identifier):
        return {"number": 42}


def test_sync_pool_io_delegates_call():
    """SyncPoolIO.call() delegates to the wrapped provider."""
    provider = FakeProvider()
    io = SyncPoolIO(provider)
    result = io.call(to="0x01", data=b"\x00")
    assert result == HexBytes(b"\x00" * 32)
    assert provider.call_count == 1


def test_sync_pool_io_delegates_get_block_number():
    """SyncPoolIO.get_block_number() delegates to the wrapped provider."""
    io = SyncPoolIO(FakeProvider())
    assert io.get_block_number() == 42


def test_pool_io_protocol_satisfied():
    """SyncPoolIO satisfies the PoolIO protocol at runtime."""
    io = SyncPoolIO(FakeProvider())
    assert isinstance(io, PoolIO)
```

**Slice 2 (Bot passes SyncPoolIO to builders)**:

```python
# tests/builders/test_pool_io.py (extended)


def test_v2_builder_uses_io_when_provided():
    """When io= is passed, the builder uses it instead of ConnectionManager."""
    provider = FakeProvider()
    io = SyncPoolIO(provider)
    # Build via builder with io= kwarg, verify provider.call_count > 0
    # and ConnectionManager.get_provider() is never called
```

**Slice 3 (Async builder wrappers)**:

```python
# tests/builders/test_async_pool_io.py


class FakeAsyncProvider:
    """Minimal async fake satisfying AsyncPoolIO's delegate."""

    def __init__(self):
        self.call_count = 0

    async def call(self, *, to, data, block=None):
        self.call_count += 1
        return HexBytes(b"\x00" * 32)

    async def call_raw(self, tx, block=None):
        return HexBytes(b"\x00" * 32)

    async def get_block_number(self):
        return 42

    async def get_block(self, block_identifier):
        return {"number": 42}


async def test_async_pool_io_delegates_call():
    """AsyncPoolIO.call() awaits the wrapped provider."""
    provider = FakeAsyncProvider()
    io = AsyncPoolIO(provider)
    result = await io.call(to="0x01", data=b"\x00")
    assert result == HexBytes(b"\x00" * 32)
    assert provider.call_count == 1


async def test_async_pool_io_protocol_satisfied():
    """AsyncPoolIO satisfies the AsyncPoolIO protocol at runtime."""
    io = AsyncPoolIO(FakeAsyncProvider())
    assert isinstance(io, AsyncPoolIO)
```

```python
# tests/builders/test_async_v2_builder.py

async def test_async_v2_builder_builds_pool():
    """AsyncV2PoolBuilder.build() produces the same UniswapV2Pool as sync builder."""
    # Uses FakeAsyncProvider with same mock responses as test_from_chain.py
```

**Slice 4 (AsyncBot refactored)**: Existing `tests/test_async_bot.py` updated —
`build_v2_pool()` and `build_v3_pool()` calls replaced with `build_pool()`. Same test
coverage, new entry point.

**Slice 5 (Shared type resolution)**: Extracted functions tested in isolation:

```python
# tests/builders/test_type_resolution.py

def test_resolve_pool_type_v2():
    """resolve_pool_type returns CONSTANT_PRODUCT for a V2 factory."""


def test_resolve_pool_type_by_probing():
    """Probing identifies pool family from on-chain method availability."""


def test_pool_class_for_descriptor():
    """pool_class_for_descriptor returns the registered class or default."""


# tests/builders/test_type_resolution_async.py

async def test_resolve_pool_type_async_v2():
    """resolve_pool_type_async returns CONSTANT_PRODUCT for a V2 factory."""


async def test_resolve_pool_type_by_probing_async():
    """Async probing identifies pool family from on-chain method availability."""
```

**Slice 6 (Remove ConnectionManager from builders)**: Existing
`tests/builders/test_from_chain.py` updated — `FakeProvider` wrapped in `SyncPoolIO`
instead of injected via `ConnectionManager`. No new test logic, just wiring change.

### Integration tests

- `tests/test_async_bot.py` (440 lines, 9 tests) covers end-to-end async pool
  construction. Updated in Slice 4 to use `build_pool()`.
- `tests/builders/test_from_chain.py` covers Aerodrome/Camelot builder construction
  with `FakeProvider`. Updated in Slice 6.
- `tests/test_bot.py` covers sync `build_pool()`. No changes needed — Slices 1–2
  verify the `io=` path is functionally equivalent.

## Benefits

- **~965 → ~160 lines in AsyncBot.** Pool construction logic moves to builders. What
  remains: constructor, `add_tracker()`, `build_pool()` (thin dispatch to async
  builders), `build_erc20token()` (delegation), and the async I/O methods
  (`get_token_balance`, `get_token_approval`, `get_token_total_supply`,
  `get_ether_balance`) which are AsyncBot-specific and don't move to builders.
- **Locality in type resolution.** A bug in pool type detection is one fix, not two.
- **Builder architecture benefits async.** Async users get `build_pool()` with automatic
  type resolution, builder registry, V4 support, Curve support — currently all missing.
- **Testing.** Builders can be tested with `FakeAsyncPoolIO` for full async coverage
  without a live RPC endpoint. Tests no longer need `AsyncWeb3` instances.
- **Feature parity.** AsyncBot gains `build_pool()`, `update()`, and all pool types
  that Bot supports.

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
- **Plan 051** (BuilderContext): Phase 6 removes `connections: ConnectionManager` from
  `BuilderContext`, which Plan 051 just simplified. `BuilderContext` drops the
  `connections` field; builders receive `io` per-call instead. This is a breaking
  change to the builder wiring — all builder constructors and `BuilderContext`
  construction in Bot must be updated in the same slice.
- **Plan 046/047** (eth_subscribe / Event-Driven Listener): These plans added async
  subscription infrastructure to AsyncBot (`AsyncConnectionManager`, subscription
  wiring, `LogListener` dispatch). This plan's refactoring of AsyncBot's `build_*`
  methods does not touch the subscription/listener code, which remains on AsyncBot.
  No conflict expected.

## Status

[x] Slice 1: PoolIO protocol + SyncPoolIO adapter
[x] Slice 2: Bot passes SyncPoolIO to builders
[x] Slice 3: Async builder wrappers
[x] Slice 4: AsyncBot refactored to delegate to async builders
[x] Slice 5: Shared type resolution
[x] Slice 6: Remove ConnectionManager from builder constructors
[x] Slice 7: Validate and clean up
