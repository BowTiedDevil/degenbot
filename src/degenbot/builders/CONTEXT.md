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

**AsyncErc20Builder I/O Methods**: Five async methods (`get_token_balance`, `get_token_approval`, `get_token_total_supply`, `get_ether_balance`, plus `_resolve_block_number` module-level helper) that perform on-chain reads for token and ether balances. Accept `io: AsyncPoolIO` parameter; use `Erc20Token` cache before querying chain. `AsyncBot` delegates its public I/O methods to these builder methods instead of duplicating the logic inline. The sync `Erc20Builder` has equivalent methods that `Bot` delegates to.

**Builder Registry**: A `dict[type, PoolBuilder]` (sync) or `dict[type, AsyncPoolBuilder]` (async) mapping concrete pool classes to their builders. Bot/AsyncBot dispatch through this registry after type resolution.

**BuildPoolRequest**: Frozen dataclass (in `request.py`) carrying optional parameters for non-V4 pool construction. Required parameters (`address`, `chain_id`, `io`) remain on `builder.build()`. `BuildPoolRequest` is the optional-parameters bag for V2/V3/Curve/Balancer builders — V4 managed pools use `BuildManagedPoolRequest` instead. Builders read the fields they recognize and ignore the rest. Replaces the previous `dispatch_kwargs` dict + `**kwargs` forwarding pattern.
_Avoid_: request object, builder params, kwargs bag — use **BuildPoolRequest**.

**BuildManagedPoolRequest**: Frozen dataclass (in `request.py`) carrying required and optional parameters for V4 managed-pool construction. `pool_id: str | bytes` is required (V4 pools cannot be discovered without it). V4 immutable data (`state_view_address`, `tokens`, `fee`, `tick_spacing`, `hook_address`) is required when the pool is not in the database. Standalone dataclass — does not inherit from `BuildPoolRequest` — because the two types represent fundamentally different operations with different required data. Used by `Bot.build_managed_pool()` and `AsyncBot.build_managed_pool()`.
_Avoid_: managed pool request, V4 request — use **BuildManagedPoolRequest**.

**BuildRequest**: Type alias `BuildPoolRequest | BuildManagedPoolRequest`. The `PoolBuilder`/`AsyncPoolBuilder` protocol's `build()` parameter uses this union to accept either request shape. Concrete builders assert `isinstance()` to narrow the type at runtime.
_Avoid_: request union — use **BuildRequest** or the specific type name.

**Type Resolution**: Shared pure-logic functions (in `type_resolution.py`) that determine the concrete pool class for an address. Sync/async top-level functions are thin wrappers that delegate to `_build_descriptor_from_db_result` (DB path) and `_descriptor_from_probing_result` (on-chain probing path) — pure functions that take domain objects and return `PoolTypeDescriptor`; the wrappers handle DB sessions and I/O. `pool_class_for_descriptor()` is the final pure lookup from descriptor → class. I/O-bearing steps (`fetch_factory_from_chain`) come in sync/async pairs that accept `PoolIO` / `AsyncPoolIO`.

**V2BuilderBase**: Base class for V2-family sync builders (V2, Aerodrome V2, Camelot). Owns shared pure helpers (`decode_immutable_data`, `extract_db_values`, `resolve_deployer_and_init_hash`, `_fetch_v2_common_data`, `_fetch_reserves`) that `AsyncV2PoolBuilder` calls independently.

**V3BuilderBase**: Base class for V3-family sync builders. Owns shared pure-logic `@staticmethod` helpers: `decode_immutable_data()`, `decode_slot0()`, `extract_db_values()`, `load_tick_snapshot()`, `resolve_tick_data_args()`. Frozen dataclasses `V3ImmutableData`, `V3Slot0Data`, `V3DbValues` carry decoded values between helpers and builders. `AsyncV3PoolBuilder` calls these static methods independently (no inheritance).

**V4BuilderBase**: Base class for V4 sync builder. Owns shared pure-logic `@staticmethod` helpers: `decode_slot0()`, `extract_db_values()`, `load_tick_snapshot()`, `resolve_tick_data_args()`. Frozen dataclasses `V4Slot0Data`, `V4DbValues` carry decoded values. `AsyncV4PoolBuilder` calls these static methods independently (no inheritance). V4's `decode_slot0()` differs from V3's — it unpacks packed protocol fees from the V4 `slot0` format.

**BalancerBuilderBase**: Base class for Balancer sync builder. Owns shared pure-logic `@staticmethod` helpers: `decode_pool_id()`, `decode_vault_tokens()`, `detect_bpt_index()`, `resolve_invariant_version()`. Frozen dataclasses `DecodedPoolId`, `VaultTokensResult` carry decoded values. Internal `_BalancerPoolType` enum replaces string literals for pool type detection. Future `AsyncBalancerBuilder` calls these static methods independently (no inheritance).

**Tick Data Fetcher**: A sync callable `Callable[[int, int], None]` created by `make_tick_data_fetcher()` that fetches tick/bitmap data for V3/V4 pools. Accepts `io: PoolIO` for I/O. Stored on pool instances for lazy tick population. Async-built pools currently pass `tick_data_fetcher=None` — an async counterpart is not viable because pool objects are synchronous and call the fetcher synchronously during `external_update()`.

**Builder Base Dataclasses**: Frozen dataclasses (`V3ImmutableData`, `V3Slot0Data`, `V3DbValues`, `V4Slot0Data`, `V4DbValues`) carry decoded pure values between base-class helpers and concrete builders. They replace ad-hoc local variables and make the data flow explicit. Avoid: builder DTOs, builder value objects — use the specific dataclass name.

## Relationships

- **Bot** creates a `SyncPoolIO(provider)` and passes `io=io` to all builder `build()`/`update()` calls
- **AsyncBot** creates an `AsyncPoolIO(provider)` and passes `io=io` to all async builder calls
- **All builders** are fully PoolIO-driven — they use `io.call()` / `io.call_raw()` instead of `self._connections.get_provider()`
- **All builders** accept `request: BuildRequest` (the union of `BuildPoolRequest | BuildManagedPoolRequest`) as the optional-parameters input; `build()` signatures are `(address, *, chain_id, io, request: BuildRequest)` — no `**kwargs` forwarding. V4 builders assert `isinstance(request, BuildManagedPoolRequest)` to narrow the type
- **Bot** constructs a `BuildPoolRequest` from `build_pool()`'s 6 optional kwargs and dispatches through `_dispatch_build()`; `build_managed_pool()` constructs a `BuildManagedPoolRequest` with required `pool_id` and dispatches the same way
- **AsyncBot** mirrors both paths with `build_managed_pool()` for V4 pools
- **BuilderContext** no longer carries a `connections` field; builders receive `default_chain_id` for chain resolution and `io: PoolIO` for I/O at call sites
- **Type resolution** functions in `type_resolution.py` replace duplicated resolution logic that was in both `Bot` and `AsyncBot`; sync/async mirror pairs collapsed to thin wrappers over shared pure functions `_build_descriptor_from_db_result` and `_descriptor_from_probing_result`
- **V3BuilderBase** and **V4BuilderBase** extract duplicated pure-logic per builder family (decode, DB extract, tick snapshot loading, tick-data-args resolution); async builders call the same `@staticmethod` helpers without inheritance — mirrors the V2 pattern
- **`load_tick_snapshot()`** re-queries the `pool_with_data` SQLAlchemy object inside the caller's session scope — lazy-loaded relationships (`initialization_maps`, `liquidity_positions`) require an active session
- **AsyncBot** delegates its 4 public I/O methods (`get_token_balance`, `get_token_approval`, `get_token_total_supply`, `get_ether_balance`) to `AsyncErc20Builder`, matching the pattern where `Bot` delegates to `Erc20Builder`

## Resolved ambiguities

### PoolIO vs ProviderAdapter

**Ruling: PoolIO is the builder-facing I/O seam. ProviderAdapter is the connection-layer primitive. Builders receive PoolIO; PoolIO adapters wrap ProviderAdapter internally. Builders should never import or reference ProviderAdapter directly.**

- ✅ "Pass `io=SyncPoolIO(provider)` to the builder"
- ✅ "The builder calls `io.call(...)` to fetch on-chain data"
- ❌ "The builder uses `self._connections.get_provider()` for I/O" (use `io` parameter)

### Required vs Optional `io` Parameter

**Ruling: `io: PoolIO` is required on `build()` (every construction needs I/O). `io: PoolIO | None = None` on `update()` (some update paths are event-driven and don't need chain I/O). Concrete builders assert `io is not None` when they need it.**

### `**kwargs` vs BuildPoolRequest

**Ruling: All optional parameters flow through `BuildPoolRequest`. No `**kwargs` forwarding in builders. `dispatch_kwargs` dict construction replaced by a single `BuildPoolRequest(...)` call in Bot/AsyncBot. Builders read `request.field` for the fields they need. This eliminates silent typo-swallowing (`**kwargs` would accept any key) and makes the optional-parameter contract explicit and type-checked.**

- ✅ `builder.build(address, chain_id=chain_id, io=io, request=BuildPoolRequest(silent=True))`
- ✅ `request.silent`, `request.pool_id`, `request.tick_bitmap` — typed access
- ❌ `builder.build(address, **dispatch_kwargs)` — untyped dict forwarding
- ❌ `**kwargs: Any` in builder signatures — silently swallows typos

### Separate Sync/Async Builder Classes

**Ruling: Separate classes for sync vs async builders, sharing pure-logic helpers on the base class. Making `build()` async on all builders would force async on sync users. Cost: two builder classes per pool family, mitigated by shared helpers.**
