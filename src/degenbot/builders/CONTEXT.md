# Builders Module Context

## Terms

**PoolIO**: A protocol describing the I/O operations builders need to fetch on-chain data. Has 7 methods: `call`, `call_raw`, `get_block_number`, `get_block`, `get_block_timestamp`, `get_code`, `get_balance`. The sync variant is `PoolIOProtocol`; the async variant is `AsyncPoolIOProtocol`. Concrete adapters (`SyncPoolIO`, `AsyncPoolIO`) wrap a `ProviderAdapter` or `AsyncProviderAdapter`.
_Avoid_: IO seam, IO adapter, provider wrapper — use **PoolIO** for the protocol, **SyncPoolIO** / **AsyncPoolIO** for the concrete adapters.

**SyncPoolIO**: Concrete adapter implementing `PoolIOProtocol` by delegating to a sync `ProviderAdapter`. Created by `Bot` as `SyncPoolIO(provider)`.
_Avoid_: PoolIO adapter, sync IO adapter.

**AsyncPoolIO**: Concrete adapter implementing `AsyncPoolIOProtocol` by delegating to an `AsyncProviderAdapter`. Created by `AsyncBot` as `AsyncPoolIO(provider)`.

**PoolBuilder**: Protocol for sync pool builders. Defines `build(io: PoolIO)` and `update(io: PoolIO | None = None)`.
_Avoid_: Builder interface, sync builder protocol.

**AsyncPoolBuilder**: Protocol for async pool builders. Defines `build(io: AsyncPoolIO)` and `update(io: AsyncPoolIO | None = None)`.

**BuilderContext**: Frozen dataclass passed to sync builders at construction. Carries shared dependencies: `db_session`, `erc20_builder`, `default_chain_id`, `managed_pools`.
_Avoid_: Builder config, builder deps.

**AsyncBuilderContext**: Frozen dataclass passed to async builders at construction. Mirrors `BuilderContext` with async counterparts: `erc20_builder: AsyncErc20Builder`.

**Builder Registry**: A `dict[type, PoolBuilder]` (sync) or `dict[type, AsyncPoolBuilder]` (async) mapping concrete pool classes to their builders. Bot/AsyncBot dispatch through this registry after type resolution.

**Type Resolution**: Shared pure-logic functions (in `type_resolution.py`) that determine the concrete pool class for an address. Both `Bot` and `AsyncBot` use the same `pool_class_for_descriptor()` pure function; I/O-bearing steps (`fetch_factory_from_chain`, `resolve_pool_type_by_probing`) come in sync/async pairs that accept `PoolIO` / `AsyncPoolIO`.

**V2BuilderBase**: Base class for V2-family sync builders (V2, Aerodrome V2, Camelot). Owns shared pure helpers (`decode_immutable_data`, `extract_db_values`, `resolve_deployer_and_init_hash`, `_fetch_v2_common_data`, `_fetch_reserves`) that `AsyncV2PoolBuilder` calls independently.

**Tick Data Fetcher**: A callable created by `make_tick_data_fetcher()` that fetches tick/bitmap data for V3/V4 pools. Accepts `io: PoolIO` for I/O. Stored on pool instances for lazy tick population.

## Relationships

- **Bot** creates a `SyncPoolIO(provider)` and passes `io=io` to all builder `build()`/`update()` calls
- **AsyncBot** creates an `AsyncPoolIO(provider)` and passes `io=io` to all async builder calls
- **All builders** are fully PoolIO-driven — they use `io.call()` / `io.call_raw()` instead of `self._connections.get_provider()`
- **BuilderContext** no longer carries a `connections` field; builders receive `default_chain_id` for chain resolution and `io: PoolIO` for I/O at call sites
- **Type resolution** functions in `type_resolution.py` replace ~330 lines of duplicated resolution logic that was in both `Bot` and `AsyncBot`

## Resolved ambiguities

### PoolIO vs ProviderAdapter

**Ruling: PoolIO is the builder-facing I/O seam. ProviderAdapter is the connection-layer primitive. Builders receive PoolIO; PoolIO adapters wrap ProviderAdapter internally. Builders should never import or reference ProviderAdapter directly.**

- ✅ "Pass `io=SyncPoolIO(provider)` to the builder"
- ✅ "The builder calls `io.call(...)` to fetch on-chain data"
- ❌ "The builder uses `self._connections.get_provider()` for I/O" (use `io` parameter)

### Required vs Optional `io` Parameter

**Ruling: `io: PoolIO` is required on `build()` (every construction needs I/O). `io: PoolIO | None = None` on `update()` (some update paths are event-driven and don't need chain I/O). Concrete builders assert `io is not None` when they need it.**

### Separate Sync/Async Builder Classes

**Ruling: Separate classes for sync vs async builders, sharing pure-logic helpers on the base class. Making `build()` async on all builders would force async on sync users. Cost: two builder classes per pool family, mitigated by shared helpers.**
