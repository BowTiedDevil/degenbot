# Plan 052: Migrate V3/V4/Curve Builders to Full PoolIO

## Overview

Complete the PoolIO migration started in Plan 048 by removing all direct
`ProviderAdapter`/`AsyncProviderAdapter` dependencies from V3, V4, Curve, and
ERC20 builders. After this plan, no builder holds `self._connections` and
`connections` can be removed from `BuilderContext`/`AsyncBuilderContext`.

## Problem

### Deletion test

If you deleted `self._connections` from V3/V4/Curve/ERC20 builders, pool
construction, token construction, and updates would all fail because they
currently resolve providers through `ConnectionManager`. The I/O surface is
split: V2-family builders use `PoolIO` exclusively, while V3/V4/Curve/ERC20
still reach through `ConnectionManager` for chain calls. This inconsistency
means adding a new builder still requires wiring through `ConnectionManager`
rather than the unified `PoolIO` seam.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| V3/V4 builders hold `self._connections` for I/O | `v3_pool_builder.py:48`, `v4_pool_builder.py:48` | Same builder class uses two I/O surfaces; `io` param is accepted but `self._connections` is still the real path |
| Curve builder holds `self._connections` for I/O | `curve_pool_builder.py:47` | All 10 detection steps + `_fetch_pool_params` + `update()` bypass `io` entirely |
| ERC20 builder holds `self._connections` for I/O | `erc20_builder.py:50` | Uses `provider.get_code()`, `provider.get_balance()`, `provider.call()`, `Erc20Token.fetch_*()` — all bypass `io` |
| 6 Curve detection modules take `provider: ProviderAdapter` | `src/degenbot/curve/detection/*.py` | Can't be reused with async I/O; tight coupling to sync ProviderAdapter |
| `CurveDataProviderImpl` stores `self._provider: ProviderAdapter` | `data_provider_impl.py:47` | The long-lived data provider on the pool bypasses the PoolIO seam; can't be swapped for async or testing |
| Tick data fetcher takes `provider_lookup: Callable[[], ProviderAdapter]` | `tick_data_fetcher.py:34` | Closure over `ConnectionManager` prevents async reuse; the lazy lookup pattern is a workaround for not having a stable I/O reference |
| `PoolIO` protocol missing `get_block_timestamp`, `get_code`, `get_balance` | `pool_io.py` | Curve builder, `CurveDataProviderImpl`, and `Erc20Builder` need them but they're not on the protocol, forcing fallback to `provider.*()` |
| `connections` still on `BuilderContext` | `context.py` | V3/V4/Curve/ERC20 builders are the only reason it exists; removing it is blocked by these builders |
| V3/V4 sync `update()` accepts `io` but ignores it (ARG002) | `v3_pool_builder.py:335`, `v4_pool_builder.py:324` | Dead parameter; ruff suppresses with `noqa: ARG002`.async `update()` already uses `io` when provided — inconsistency between sync and async |

## Solution

### Step 1: Expand PoolIO protocol

`CurveDataProviderImpl`, `CurvePoolBuilder`, and `Erc20Builder` use methods
not currently on `PoolIO`:

- `get_block_timestamp(block)` — used by Curve builder and
  `CurveDataProviderImpl.block_timestamp()`
- `get_code(address, block)` — used by `Erc20Builder.build()` to verify
  contract deployment
- `get_balance(address, block)` — used by `Erc20Builder.get_ether_balance()`

Add all three to both `PoolIO`/`AsyncPoolIOProtocol` protocols and implement
on `SyncPoolIO`/`AsyncPoolIO` adapters.

```python
# Before
class PoolIO(Protocol):
    def get_block_number(self) -> int: ...
    def get_block(self, block_identifier: int | str) -> BlockData | None: ...
    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes: ...
    def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes: ...

# After
class PoolIO(Protocol):
    def get_block_number(self) -> int: ...
    def get_block(self, block_identifier: int | str) -> BlockData | None: ...
    def get_block_timestamp(self, block: int | None = None) -> int: ...
    def get_code(self, address: str, block: int | None = None) -> HexBytes: ...
    def get_balance(self, address: str, block: int | None = None) -> int: ...
    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes: ...
    def call_raw(self, tx: TxParams, block: BlockIdentifier | None = None) -> HexBytes: ...
```

### Step 2: Migrate Curve detection modules and `CurveDataProviderImpl` to PoolIO

Combine the detection module and data provider migration into one slice since
they share the `call_raw` → `io.call_raw` pattern and both break the same
Curve test files.

**Detection modules** — each function takes `provider: ProviderAdapter` and
uses only `provider.call_raw()`. Change to `io: PoolIO`:

```python
# Before (coin_discovery.py)
def discover_coins(
    provider: ProviderAdapter,
    pool_address: ChecksumAddress,
    *,
    block_identifier: int | None = None,
) -> CoinDiscoveryResult:

# After
def discover_coins(
    io: PoolIO,
    pool_address: ChecksumAddress,
    *,
    block_identifier: int | None = None,
) -> CoinDiscoveryResult:
```

Same pattern for `detect_crypto_params`, `detect_a_ramping`,
`detect_lending_tokens`, `detect_metapool`, `find_lp_token`, and the private
`_resolve_base_pool_address` helper in `metapool_detector.py`.

`_fetch_pool_params()` (a module-level function in `curve_pool_builder.py`,
not a detection module) follows the same pattern — takes `provider:
ProviderAdapter` and uses `provider.call_raw()`.

**CurveDataProviderImpl** — replace `self._provider: ProviderAdapter` with
`self._io: PoolIO` and update the shared helper methods:

```python
# Before
def __init__(self, *, provider: ProviderAdapter, ...):
    self._provider = provider

def _call(self, to, method_sig, return_types, block_number):
    data = self._provider.call_raw(
        {"to": to, "data": Web3.keccak(text=method_sig)[:4]}, block=block_number
    )

# After
def __init__(self, *, io: PoolIO, ...):
    self._io = io

def _call(self, to, method_sig, return_types, block_number):
    data = self._io.call_raw(
        {"to": to, "data": Web3.keccak(text=method_sig)[:4]}, block=block_number
    )
```

`block_timestamp()` shifts from `self._provider.get_block_timestamp()` to
`self._io.get_block_timestamp()`. `block_number()` shifts from
`self._provider.get_block_number()` to `self._io.get_block_number()`.

### Step 3: Migrate CurvePoolBuilder to full PoolIO

Replace all `self._connections` usage with `io`:

```python
# Before
chain_id = chain_id or self._connections.default_chain_id
provider = self._connections.get_provider(chain_id)
coins = discover_coins(provider, pool_address, ...)

# After
chain_id = chain_id or self._default_chain_id
coins = discover_coins(io, pool_address, ...)
```

Remove `self._connections = ctx.connections`; store
`self._default_chain_id = ctx.default_chain_id`. Assert `io is not None` in
`build()` and `update()`.

### Step 4: Migrate tick_data_fetcher to PoolIO

Replace `provider_lookup: Callable[[], ProviderAdapter]` with `io: PoolIO`.
The lazy closure is no longer needed — `PoolIO` already encapsulates the
provider reference.

```python
# Before
def make_tick_data_fetcher(
    pool_lookup: Callable[[], ...],
    provider_lookup: Callable[[], ProviderAdapter],
    types: TickDataTypes,
    ...
) -> Callable[[int, int], None]:
    def fetcher(word_position, block_number):
        provider = provider_lookup()
        _fetch_v3(provider=provider, ...)

# After
def make_tick_data_fetcher(
    pool_lookup: Callable[[], ...],
    io: PoolIO,
    types: TickDataTypes,
    ...
) -> Callable[[int, int], None]:
    def fetcher(word_position, block_number):
        _fetch_v3(io=io, ...)
```

`_fetch_v3()` and `_fetch_v4()` replace `provider: ProviderAdapter` with
`io: PoolIO`, replacing `raw_call(provider, ...)` with `io.call_raw(...)`
and `provider.call(...)` with `io.call(...)`.

### Step 5: Migrate V3/V4 builders to full PoolIO

Remove `self._connections` from `V3PoolBuilder` and `V4PoolBuilder`:

```python
# Before (V3 build — dual path)
call_fn = io.call if io is not None else provider.call
...
provider = self._connections.get_provider(chain_id)

# After — io exclusively
assert io is not None
...
# No provider variable, no fallback
```

`update()` must also be migrated — currently the sync `build()` has a
dual-path `call_fn = io.call if io else provider.call` while the sync
`update()` ignores `io` entirely and uses `provider` directly. Both must use
`io` exclusively. This matches what `AsyncV3PoolBuilder.update()` already
does (it uses `io` when provided, falling back to `self._connections` only
when `io is None` — after this slice, the fallback is removed).

`_make_tick_data_fetcher` passes `io=io` instead of
`provider_lookup=lambda: self._connections.get_provider(chain_id)`.

Note: The tick data fetcher is a long-lived callback stored on the pool.
It holds a reference to `io: PoolIO` which wraps a `ProviderAdapter`. Since
`ProviderAdapter` is stored on `ConnectionManager` (which outlives pools),
the reference stays valid. No lazy lookup needed.

### Step 6: Migrate Erc20Builder to PoolIO

Migrate `Erc20Builder` alongside the pool builders. It currently uses:

| Method | PoolIO has it? | Usage |
|--------|---------------|-------|
| `provider.call()` | ✅ | `fetch_name/symbol/decimals` |
| `provider.get_block_number()` | ✅ | `_resolve_block_number` helper |
| `provider.get_code()` | ✅ (added in Step 1) | Contract existence check in `build()` |
| `provider.get_balance()` | ✅ (added in Step 1) | `get_ether_balance()` |

```python
# Before
def build(self, address, *, chain_id=None, silent=False, io=None):
    chain_id = chain_id or self._connections.default_chain_id
    provider = self._connections.get_provider(chain_id)
    if not provider.get_code(address):
        ...
    Erc20Token.fetch_name_symbol_decimals_batched(address, provider)

# After
def build(self, address, *, chain_id=None, silent=False, io=None):
    assert io is not None
    chain_id = chain_id or self._default_chain_id
    if not io.get_code(address):
        ...
    name, symbol, decimals = self._fetch_name_symbol_decimals_batched(address, io)
```

The `Erc20Token.fetch_*()` static methods currently take `ProviderAdapter`
directly. Rather than changing their public signatures (a breaking change for
external callers), inline the fetch logic into `Erc20Builder` as instance
methods that use `io: PoolIO`. `AsyncErc20Builder` already does this — its
`_fetch_name_symbol_decimals_batched` is a `@staticmethod` taking
`AsyncPoolIO`, not `ProviderAdapter`.

`get_ether_balance()`, `get_token_balance()`, `get_token_approval()`,
`get_token_total_supply()` — all receive `io: PoolIO` as a parameter instead
of resolving `provider` from `self._connections`.

Remove `self._connections`; store `self._default_chain_id`.

### Step 7: Remove `connections` from BuilderContext

Once no builder uses `self._connections`, remove the field from
`BuilderContext` and `AsyncBuilderContext`. Both `Bot` and `AsyncBot` stop
passing `connections` when constructing the context.

```python
# Before
@dataclass(frozen=True)
class BuilderContext:
    connections: ConnectionManager
    db: DatabaseSessionManager
    pools: PoolRegistry
    tokens: TokenRegistry
    erc20_builder: Erc20Builder
    default_chain_id: ChainId | None = None

# After
@dataclass(frozen=True)
class BuilderContext:
    db: DatabaseSessionManager
    pools: PoolRegistry
    tokens: TokenRegistry
    erc20_builder: Erc20Builder
    default_chain_id: ChainId | None = None
```

`Erc20Builder` no longer takes `connections` — it takes `default_chain_id`
and `db`/`tokens` only. `Bot.__init__` constructs `Erc20Builder` with
`default_chain_id=self.connections.default_chain_id` directly.

### Step 8: Async counterparts — partial

For each async builder (`AsyncV3PoolBuilder`, `AsyncV4PoolBuilder`), apply
the same pattern: use `io: AsyncPoolIO` exclusively, remove
`self._connections`, and store `self._default_chain_id`.

**Async tick data fetcher**: Async V3/V4 builders currently pass
`tick_data_fetcher=None` to pool constructors — they don't create tick data
fetchers. This migration does NOT add async tick data fetchers. The async
builders don't use them, so Slice 5 (sync tick data fetcher migration) has
no async equivalent. This is intentional — async tick fetching requires an
`async def fetcher()` return type which changes the
`ConcentratedLiquidityStateManager` contract. That's a separate concern.

**`call_raw` not on `AsyncPoolIOProtocol`**: Detection modules use
`call_raw({"to": ..., "data": ...}, block=...)`, but
`AsyncProviderAdapter` has no `call_raw()` — only `call()`. A future async
Curve builder would need detection functions rewritten to use
`io.call(to=..., data=..., block=...)` instead of `io.call_raw(...)`. This
is a known limitation for async Curve migration, documented here for future
reference.

**`AsyncErc20Builder`**: Already uses `io: AsyncPoolIO` when provided (falls
back to `self._connections` when `io is None`). Remove the fallback, assert
`io is not None`, and remove `self._connections`. Unlike the sync builder,
`AsyncErc20Builder` doesn't use `get_code()` or `get_balance()`, so no
additional protocol methods are needed.

### Design decisions

- **`io` stored on tick data fetcher vs lazy `PoolIO` lookup**: Store `io` directly. The `PoolIO` adapter wraps a `ProviderAdapter`, which is stored on the bot's `ConnectionManager`. The adapter reference stays valid because `ConnectionManager` outlives pools. The lazy `provider_lookup` closure was a workaround for not having a stable I/O reference — `PoolIO` provides one.

- **`CurveDataProviderImpl` constructor**: Change `provider: ProviderAdapter` → `io: PoolIO`. This is a breaking change for any code constructing `CurveDataProviderImpl` directly. Since it's only constructed by `CurvePoolBuilder` (and test fakes that implement the `CurveDataProvider` protocol, not `CurveDataProviderImpl`), this is safe. The `FakeCurveDataProvider` test double doesn't change since it implements the `CurveDataProvider` protocol, not `CurveDataProviderImpl`.

- **Adding `get_code()` and `get_balance()` to `PoolIO`**: These are needed by `Erc20Builder` and are present on both `ProviderAdapter` and `AsyncProviderAdapter`. Adding them makes PoolIO sufficient for all builder I/O and unblocks `Erc20Builder` migration. The cost is widening the protocol by 2 methods, but every method on the protocol has at least one consumer.

- **Inlining `Erc20Token.fetch_*()` into `Erc20Builder`**: The `@staticmethod` methods on `Erc20Token` take `ProviderAdapter` and are called by `Erc20Builder`. Changing their signatures to accept `PoolIO` would be a breaking change for external callers who import them directly. Instead, `Erc20Builder` and `AsyncErc20Builder` get their own fetch methods that take `PoolIO`/`AsyncPoolIO`. The `Erc20Token.fetch_*()` methods are left unchanged (they still work with `ProviderAdapter` for backward compatibility). This duplicates a small amount of ABI-decode logic but avoids a breaking API change.

- **Async tick data fetcher deferred**: The sync migration establishes the pattern. Async requires `async def fetcher()`, which changes the `ConcentratedLiquidityStateManager` contract. This is a separate concern (async event loop integration) that shouldn't block the sync migration.

- **`call_raw` signature difference**: PoolIO's `call_raw(tx: TxParams, block)` takes a dict; detection modules construct `{"to": addr, "data": calldata}`. No change needed — just pass the dict directly.

- **Sync V3 `update()` was ignoring `io`**: The sync V3/V4 `update()` methods accepted `io: PoolIO | None = None` but ignored it (Plan 048 Slice 6 placed `noqa: ARG002`). This plan migrates `update()` to use `io` exclusively, matching what `AsyncV3PoolBuilder.update()` already does.

- **`_resolve_base_pool_address` in metapool_detector**: Private helper also takes `provider: ProviderAdapter`. Migrated alongside `detect_metapool` since it's in the same module.

## Files Involved

**Primary:**
- `src/degenbot/builders/pool_io.py` — add `get_block_timestamp`, `get_code`, `get_balance` to protocols and adapters
- `src/degenbot/builders/curve_pool_builder.py` — remove `self._connections`, use `io` exclusively; migrate `_fetch_pool_params`
- `src/degenbot/builders/v3_pool_builder.py` — remove `self._connections`, use `io` exclusively; migrate `build()` and `update()`
- `src/degenbot/builders/v4_pool_builder.py` — remove `self._connections`, use `io` exclusively; migrate `build()` and `update()`
- `src/degenbot/builders/erc20_builder.py` — remove `self._connections`, use `io` exclusively; inline fetch methods using PoolIO
- `src/degenbot/builders/context.py` — remove `connections` field
- `src/degenbot/builders/async_context.py` — remove `connections` field
- `src/degenbot/builders/tick_data_fetcher.py` — `io: PoolIO` replaces `provider_lookup`
- `src/degenbot/curve/data_provider_impl.py` — `io: PoolIO` replaces `provider: ProviderAdapter`
- `src/degenbot/curve/detection/coin_discovery.py` — `io: PoolIO` replaces `provider: ProviderAdapter`
- `src/degenbot/curve/detection/crypto_detector.py` — same
- `src/degenbot/curve/detection/a_ramping.py` — same
- `src/degenbot/curve/detection/lending_detector.py` — same
- `src/degenbot/curve/detection/metapool_detector.py` — same (including `_resolve_base_pool_address`)
- `src/degenbot/curve/detection/lp_token.py` — same
- `src/degenbot/bot.py` — remove `connections` from `BuilderContext` construction; pass `default_chain_id` to `Erc20Builder`
- `src/degenbot/async_bot.py` — remove `connections` from `AsyncBuilderContext` construction

**Test files requiring updates:**
- `tests/curve/detection/test_coin_discovery.py` — wrap `make_fake_curve_provider()` in `SyncPoolIO(...)` or use `FakeSyncPoolIO`
- `tests/curve/detection/test_a_ramping.py` — same
- `tests/curve/detection/test_crypto_detector.py` — same
- `tests/curve/detection/test_lending_detector.py` — same
- `tests/curve/detection/test_lp_token.py` — same
- `tests/curve/detection/test_metapool_detector.py` — same
- `tests/curve/test_curve_data_provider.py` — update 18 `CurveDataProviderImpl(provider=...)` to `CurveDataProviderImpl(io=...)`
- `tests/curve/test_curve_stableswap_pool.py` — may need updates if it constructs `CurveDataProviderImpl` directly
- `tests/builders/test_from_chain.py` — update `BuilderContext` construction
- `tests/builders/test_context.py` — update context tests
- `tests/builders/test_pool_io.py` — extend with `get_block_timestamp`, `get_code`, `get_balance` tests

**No change needed:**
- `src/degenbot/curve/types.py` — `CurveDataProvider` protocol unchanged (consumers don't change)
- `src/degenbot/curve/detection/types.py` — frozen dataclasses, no provider references
- `src/degenbot/builders/aerodrome_v2_builder.py` — already fully PoolIO-driven
- `src/degenbot/builders/camelot_builder.py` — already fully PoolIO-driven
- `src/degenbot/builders/v2_pool_builder.py` — already fully PoolIO-driven
- `src/degenbot/erc20/erc20.py` — `Erc20Token.fetch_*()` static methods left unchanged for backward compatibility
- `tests/curve/detection/fake_provider.py` — still creates `ProviderAdapter`; `SyncPoolIO(fake_provider)` wraps it
- `tests/curve/detection/fake_w3.py` — legacy fake Web3, not used by ProviderAdapter-based tests

## Implementation Order

### Slice 1: Expand PoolIO protocol

1. Add `get_block_timestamp(block: int | None = None) -> int` to `PoolIO` protocol
2. Add `get_code(address: str, block: int | None = None) -> HexBytes` to `PoolIO` protocol
3. Add `get_balance(address: str, block: int | None = None) -> int` to `PoolIO` protocol
4. Add matching methods to `AsyncPoolIOProtocol`
5. Implement on `SyncPoolIO` — delegate to `self._provider.get_block_timestamp()`, `self._provider.get_code()`, `self._provider.get_balance()`
6. Implement on `AsyncPoolIO` — delegate to `await self._provider.get_block_timestamp()`, `await self._provider.get_code()`, `await self._provider.get_balance()`
7. Extend `tests/builders/test_pool_io.py` with tests for all 3 new methods (sync + async)
8. Run: `just test-python`

### Slice 2: Migrate Curve detection modules and CurveDataProviderImpl to PoolIO

These are combined into one slice because they share the same `call_raw` →
`io.call_raw` migration pattern and both affect the same Curve test files.
Combining avoids two rounds of test breakage/fix.

1. Change each detection function signature from `provider: ProviderAdapter` to `io: PoolIO`
2. Replace `provider.call_raw(...)` with `io.call_raw(...)` inside each function
3. Migrate `_resolve_base_pool_address` in `metapool_detector.py`
4. Migrate `_fetch_pool_params` in `curve_pool_builder.py` (module-level function, not a detection module — same pattern)
5. Change `CurveDataProviderImpl` constructor: `provider: ProviderAdapter` → `io: PoolIO`
6. Rename `self._provider` → `self._io`; update `_call`, `_call_single`, `_call_raw_single`, `block_timestamp`, `block_number`
7. Create `FakeSyncPoolIO` test helper in `tests/builders/helpers.py` (thin wrapper satisfying `PoolIO` protocol, avoids `ProviderAdapter.__new__` hack)
8. Update `CurvePoolBuilder.build()` to pass `io` to all detection functions and `CurveDataProviderImpl`
9. Update detection test files (6 files, ~72 provider references) to use `FakeSyncPoolIO` or `SyncPoolIO(make_fake_curve_provider(...))`
10. Update `tests/curve/test_curve_data_provider.py` — 18 `CurveDataProviderImpl(provider=...)` → `CurveDataProviderImpl(io=...)`
11. Run: `just test-python`

### Slice 3: Migrate CurvePoolBuilder to full PoolIO

1. Replace `self._connections = ctx.connections` with `self._default_chain_id = ctx.default_chain_id`
2. In `build()`: assert `io is not None`, remove `provider = self._connections.get_provider(chain_id)`, remove `provider.get_block_timestamp()` fallback
3. In `update()`: use `io` instead of `self._connections.get_provider(pool.chain_id)`, make `io: PoolIO` required with assert
4. Remove `noqa: ARG002` from `update()`'s `io` param
5. Run: `just test-python`

### Slice 4: Migrate tick_data_fetcher to PoolIO

1. Change `make_tick_data_fetcher` signature: replace `provider_lookup: Callable[[], ProviderAdapter]` with `io: PoolIO`
2. Update `_fetch_v3()` and `_fetch_v4()`: replace `provider: ProviderAdapter` with `io: PoolIO`
3. Replace `raw_call(provider, ...)` with `io.call_raw(...)`; replace `provider.call(...)` with `io.call(...)`
4. Update V3 `_make_tick_data_fetcher()` to pass `io=io` instead of `provider_lookup=lambda: ...`
5. Update V4 `_make_tick_data_fetcher()` same way
6. Add test: `tests/builders/test_tick_data_fetcher_pool_io.py` — verify fetcher uses `PoolIO` instead of `provider_lookup` closure
7. Run: `just test-python`

### Slice 5: Migrate V3/V4 builders to full PoolIO

1. Replace `self._connections = ctx.connections` with `self._default_chain_id = ctx.default_chain_id`
2. In `build()`: remove all `provider = self._connections.get_provider(chain_id)`, remove dual-path `call_fn = io.call if io else provider.call`, use `io` exclusively
3. In `update()`: use `io` instead of `self._connections.get_provider(pool.chain_id)`, make `io` required with assert
4. Remove `noqa: ARG002` from `update()`'s `io` param
5. Remove `raw_call` import from both builder files
6. V3 `_make_tick_data_fetcher` already updated in Slice 4
7. Run: `just test-python`

### Slice 6: Migrate Erc20Builder to full PoolIO

1. Replace `self._connections = connections` with `self._default_chain_id = default_chain_id`
2. Update `build()`: assert `io is not None`, remove `self._connections.get_provider(chain_id)`, use `io.get_code()` for contract check, use `io.call()` for fetch
3. Inline `Erc20Token.fetch_*()` calls as instance methods on `Erc20Builder` that take `io: PoolIO` (matches `AsyncErc20Builder` pattern). Note: `fetch_decimals` and `fetch_total_supply` use `raw_call(provider, ...)` internally — the inlined versions call `io.call()` + ABI decode directly instead.
4. Update `get_ether_balance()`: take `io: PoolIO` param, use `io.get_balance()`
5. Update `get_token_balance()`, `get_token_approval()`, `get_token_total_supply()`: take `io: PoolIO` param
6. Run: `just test-python`

### Slice 7: Remove `connections` from BuilderContext and async builders

1. Remove `connections` field from `BuilderContext`
2. Remove `connections` field from `AsyncBuilderContext`
3. Update `Bot.__init__` — remove `connections=self.connections` from context construction; pass `default_chain_id=self.connections.default_chain_id` to `Erc20Builder`
4. Update `AsyncBot.__init__` — same pattern
5. Update `AsyncErc20Builder` — remove `self._connections`, use `io` exclusively with `self._default_chain_id`
6. Update `AsyncV3PoolBuilder` — remove `self._connections`, use `io` exclusively with `self._default_chain_id`
7. Update `AsyncV4PoolBuilder` — same
8. Run: `just test-python`

### Slice 8: Validate and clean up

1. Run `just lint` + `just test-all` + `uv run ty check src/`
2. Update `src/degenbot/builders/CONTEXT.md` — note that all builders are now PoolIO-driven
3. Update `CONTEXT-MAP.md` — note that `connections` is no longer on `BuilderContext`; document the 7-method PoolIO protocol
4. Remove any remaining `noqa: ARG002` comments on `io` params in `update()` methods
5. Verify no builder imports `ConnectionManager` or `ProviderAdapter`
6. Run: `just test-python`

## Testing

### Per-slice test runs

Each slice runs `just test-python`. The migration is internal — no public API
change to pool or token classes — so existing integration tests serve as
regression coverage.

### New unit tests

```python
# tests/builders/test_pool_io.py — extend existing

def test_sync_pool_io_get_block_timestamp():
    """SyncPoolIO delegates get_block_timestamp to the wrapped provider."""

def test_sync_pool_io_get_code():
    """SyncPoolIO delegates get_code to the wrapped provider."""

def test_sync_pool_io_get_balance():
    """SyncPoolIO delegates get_balance to the wrapped provider."""

def test_async_pool_io_get_block_timestamp():
    """AsyncPoolIO delegates get_block_timestamp to the wrapped async provider."""

def test_async_pool_io_get_code():
    """AsyncPoolIO delegates get_code to the wrapped async provider."""

def test_async_pool_io_get_balance():
    """AsyncPoolIO delegates get_balance to the wrapped async provider."""
```

```python
# tests/builders/helpers.py — new test helper

class FakeSyncPoolIO:
    """Minimal PoolIO implementation for detection module tests.

    Dispatches call_raw() by method selector, matching FakeCurveW3Eth's
    pattern. Thin replacement for SyncPoolIO(make_fake_curve_provider(...))
    that avoids the ProviderAdapter.__new__ constructor hack.
    """
```

```python
# tests/curve/test_detection_pool_io.py — new file

def test_discover_coins_accepts_pool_io():
    """discover_coins works with PoolIO adapter, not raw ProviderAdapter."""

def test_detect_crypto_params_accepts_pool_io():
    """detect_crypto_params works with PoolIO adapter."""

# ... one test per detection function
```

```python
# tests/builders/test_tick_data_fetcher_pool_io.py — new file

def test_make_tick_data_fetcher_receives_io():
    """Tick data fetcher uses PoolIO instead of provider_lookup closure."""
```

### Updated test files

6 detection test files under `tests/curve/detection/` and
`tests/curve/test_curve_data_provider.py` (18 instances) must update their
construction of `ProviderAdapter` → `PoolIO` wrappers. These aren't new
tests — they're existing tests updated to match the new parameter type.

### Integration tests

Existing test suites cover the full build/update paths:
- `tests/curve/test_curve_stableswap_pool.py` — Curve pool construction and updates
- `tests/uniswap/v3/` — V3 pool construction and updates
- `tests/uniswap/v4/` — V4 pool construction and updates
- `tests/builders/test_from_chain.py` — builder construction from chain data
- `tests/builders/test_bot_pool_io.py` — Bot passes SyncPoolIO to builders

## Benefits

- **Leverage**: One I/O seam (`PoolIO`, 7 methods) for all builders — V2-family already uses it; V3/V4/Curve/ERC20 join the same interface
- **Depth**: `BuilderContext` shrinks from 5 fields to 4 — `connections` (wide I/O surface: any provider for any chain) is replaced by `default_chain_id` (a single config value). Builders receive `io: PoolIO` (narrow typed seam) at call sites.
- **Locality**: Detection modules in `curve/detection/` depend on `PoolIO` (builder seam) instead of `ProviderAdapter` (connection layer) — same module, narrower dependency
- **Testability**: `CurveDataProviderImpl` can be tested with `FakeSyncPoolIO` instead of requiring a full `ProviderAdapter` mock. Detection tests can use `FakeSyncPoolIO` instead of `make_fake_curve_provider()` + `ProviderAdapter.__new__` hack.
- **Async ready**: Detection modules and `CurveDataProviderImpl` can be reused by async builders once `AsyncPoolIO` is available (same interface, async methods). The `call_raw`→`call` conversion for async is documented as a known limitation.
- **Removal**: `connections` on `BuilderContext` is dead weight once all builders use `io` — removing it simplifies the context and prevents future builders from reaching through to the connection layer

## Risks

- **CurveDataProviderImpl constructor is a breaking change**: Any code constructing `CurveDataProviderImpl(provider=provider, ...)` must change to `CurveDataProviderImpl(io=SyncPoolIO(provider), ...)`. Mitigated by the only caller being `CurvePoolBuilder`, which already has `io` available. Test fakes implement the `CurveDataProvider` protocol, not `CurveDataProviderImpl`, so they don't change.

- **Detection module signatures are a breaking change**: `discover_coins(provider, ...)` → `discover_coins(io, ...)`. These are only called by `CurvePoolBuilder` (grep-verified — no external callers), so impact is contained. Tests update in Slice 2.

- **Tick data fetcher lifetime**: The fetcher is stored on the pool and called on every state update. It holds a reference to `io: PoolIO` which wraps a `ProviderAdapter`. If the bot is pickled/unpickled, the `ProviderAdapter` may be `None` (it handles this via `set_provider()`). The `SyncPoolIO` wrapper would also need pickle support. Mitigation: `_tick_data_fetcher` is already in `_pickle_drops` on V3/V4 pool classes (line 118 of `v3_liquidity_pool.py`), so the fetcher is dropped on pickle. The pool loses lazy tick fetching after unpickle — this is already the current behavior.

- **Erc20Token.fetch_*() static methods unchanged**: They still take `ProviderAdapter`. Callers who use them directly are unaffected. `Erc20Builder` inlines its own PoolIO-based fetch logic — this duplicates a small amount of ABI-decode code but avoids a breaking change to `Erc20Token`'s public API.

- **6 detection test files + data provider test file must update**: ~90 test references to `provider` across 7 test files. Large surface area but mechanical (wrap with `SyncPoolIO` or use `FakeSyncPoolIO`). Mitigated by combining in one slice and creating a `FakeSyncPoolIO` helper.

## Relationship to Other Plans

- **Plan 048** (completed): Introduced `PoolIO` seam and migrated V2-family builders. This plan completes the migration for V3/V4/Curve/ERC20, which Plan 048 explicitly deferred.
- **Plan 013** (completed): Made Curve pools I/O-free via `CurveDataProvider` seam. This plan moves the data provider from raw `ProviderAdapter` to `PoolIO`, deepening that seam.
- **Plan 049** (completed): Replaced `CurveFetcherFactory` closures with `CurveDataProviderImpl`. This plan migrates `CurveDataProviderImpl` from `ProviderAdapter` to `PoolIO`, continuing the structural improvement.
- **Plan 017** (completed): Made V2/V3/V4/Aerodrome pool classes I/O-free. This plan makes their *builders* I/O-free through the `PoolIO` seam (builders were not in scope for Plan 017).

## Status

[ ] Slice 1: Expand PoolIO protocol
[ ] Slice 2: Migrate Curve detection modules and CurveDataProviderImpl to PoolIO
[ ] Slice 3: Migrate CurvePoolBuilder to full PoolIO
[ ] Slice 4: Migrate tick_data_fetcher to PoolIO
[ ] Slice 5: Migrate V3/V4 builders to full PoolIO
[ ] Slice 6: Migrate Erc20Builder to full PoolIO
[ ] Slice 7: Remove `connections` from BuilderContext and async builders
[ ] Slice 8: Validate and clean up
