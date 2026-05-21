# Plan 072: Extract `build_managed_pool`, Consolidate Deployer Registries, and Reduce `BuildPoolRequest`

## Overview

Three related changes that unlock each other:

1. **Extract `build_managed_pool()`** — V4 managed pools have a fundamentally different addressing model and discovery contract. They deserve their own method with a dedicated `BuildManagedPoolRequest` where `pool_id` is required, not optional.
2. **Consolidate `FACTORY_DEPLOYMENTS` into `pool_type_registry`** — Two structures hold the same `(chain_id, factory) → (deployer, pool_init_hash)` data. The pool constructors silently re-resolve from `FACTORY_DEPLOYMENTS`, making the builder's resolution from `pool_type_registry` coincidentally correct. Consolidating on `pool_type_registry` makes it the sole source of truth and eliminates the three-layer resolution chain.
3. **Reduce `BuildPoolRequest`** — With registries consolidated and V4 extracted, the remaining `deployer_address`/`init_hash` fields on `BuildPoolRequest` can be removed entirely (no more internal transport needed), and `build_pool()` drops from 14 kwargs to 6.

## Problem

### Deletion test

If you deleted `FACTORY_DEPLOYMENTS`, the V4 fast path, the `pool_id` parameter, and the 5 V4-only kwargs from `build_pool()`, every non-V4 pool would still build correctly — `pool_type_registry` already provides the deployer/init_hash data at the builder layer. V4 pools are a different operation sharing one method, and `FACTORY_DEPLOYMENTS` is a redundant copy of `pool_type_registry`'s deployment data.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| V4 fast path forks control flow at the top of `build_pool()` | `bot.py:273`, `async_bot.py:213` | Two operations in one method; `pool_id` is really a dispatch discriminator, not an option |
| V4-only kwargs on the shared `build_pool()` signature | `bot.py:230-240` | 5 kwargs (`state_view_address`, `tokens`, `fee`, `tick_spacing`, `hook_address`) are meaningless for V2/V3/Curve/Balancer pools |
| `pool_id` is optional but V4 pools cannot be built without it | `bot.py:225`, `request.py` | `pool_id: str \| bytes \| None = None` is the wrong type — it should be required for V4, absent for everyone else |
| V4 fields pollute `BuildPoolRequest` | `request.py` | 7 of 14 fields are V4-specific; every non-V4 builder carries them as dead weight |
| Two registries hold the same deployer/init_hash data | `deployments.py`, `pool_type.py` | `FACTORY_DEPLOYMENTS` is a hardcoded dict. `pool_type_registry` is populated *from* it by each DEX `__init__.py`. They can diverge if only one is updated. |
| Pool constructors silently re-resolve from `FACTORY_DEPLOYMENTS` | `v2_liquidity_pool.py:104-108`, `v3_liquidity_pool.py:177-182` | Builders resolve from `pool_type_registry`, pass values to constructor, then constructor overwrites them from `FACTORY_DEPLOYMENTS`. For user-registered factories, the constructor's `FACTORY_DEPLOYMENTS` lookup hits a KeyError (silently suppressed) and preserves the builder's values — correct by accident, not by design. |
| `deployer_address`/`init_hash` are public kwargs that bypass registries | `bot.py:226-227`, `request.py` | These kwargs predate both registries. They remain on `BuildPoolRequest` as internal transport (V3 builder's `dataclasses.replace()` pattern, V2 builder's `resolve_deployer_and_init_hash()`). The public API shouldn't expose them. |
| `bpt_idx`/`invariant_version` are dead on the public API | `request.py` | Only `balancer_builder.py` reads them, but `Bot.build_pool()` doesn't expose them — there's no way to pass them from the caller |
| Balancer's `request.pool_id` is V4 semantics leaking | `balancer_builder.py:99` | Balancer fetches its own pool ID on-chain; the `request.pool_id` override is an artifact of the shared field |

### The three-layer resolution chain

Today, deployer/init_hash is resolved three times along the same construction path:

```
DEX __init__.py:
    FACTORY_DEPLOYMENTS[chain_id][factory]  →  pool_type_registry.register(deployer=..., pool_init_hash=...)

Builder (V2BuilderBase.resolve_deployer_and_init_hash / V3PoolBuilder.build):
    pool_type_registry.get_deployment(chain_id, factory)  →  deployer, init_hash
    ↓ passes to constructor

Pool constructor (UniswapV2Pool.__init__ / UniswapV3Pool.__init__):
    FACTORY_DEPLOYMENTS[chain_id][factory]  →  overwrites builder's values
    (KeyError silently suppressed for unregistered factories)
```

The builder's resolution is correct for registered factories, but meaningless for unregistered ones — the pool constructor's `FACTORY_DEPLOYMENTS` lookup will overwrite the builder's values anyway. For user-registered factories (in `pool_type_registry` but not in `FACTORY_DEPLOYMENTS`), the constructor's `KeyError` is silently suppressed, and the builder's values survive by accident.

Consolidating on `pool_type_registry` eliminates this: the builder resolves from the registry, passes values to the constructor, and the constructor trusts them — no silent overwrite, no accidental correctness.

## Solution

### Step 1: Create `BuildManagedPoolRequest` (standalone)

A new frozen dataclass carrying the required/optional data for V4 managed-pool construction. `pool_id` is required — not optional — because V4 pools cannot be built without it. This is a standalone dataclass, not inheriting from `BuildPoolRequest`.

```python
# In builders/request.py

@dataclass(slots=True, frozen=True, kw_only=True)
class BuildManagedPoolRequest:
    """Typed request object for V4 managed-pool construction.

    `pool_id` is required — V4 pools cannot be discovered without it.
    Immutable data (`state_view_address`, `tokens`, `fee`,
    `tick_spacing`, `hook_address`) is required when the pool is not
    in the database; otherwise it is fetched from DB.
    """

    # Required
    pool_id: str | bytes

    # Common options (mirrors BuildPoolRequest's universal fields)
    silent: bool = False
    state_block: int | None = None
    state_cache_depth: int = 8

    # V4 immutable data — required if not in DB
    state_view_address: str | None = None
    tokens: Sequence[str] | None = None
    fee: int | None = None
    tick_spacing: int | None = None
    hook_address: str | None = None

    # Pre-fetched tick data (DB snapshot or test fixtures)
    tick_bitmap: dict[int, Any] | None = None
    tick_data: dict[int, Any] | None = None
```

`tick_bitmap`/`tick_data` are intentionally duplicated across both request types — they have the same semantics but belong to different address spaces (V3 vs V4). This is the right kind of duplication (each type owns its own fields).

### Step 2: Add `build_managed_pool()` to Bot and AsyncBot

```python
# In bot.py

def build_managed_pool(
    self,
    address: str,               # PoolManager address
    pool_id: str | bytes,       # REQUIRED
    *,
    chain_id: ChainId | None = None,
    state_block: int | None = None,
    silent: bool = False,
    state_cache_depth: int = 8,
    # V4 immutable data — required if not in DB
    state_view_address: str | None = None,
    tokens: Sequence[str] | None = None,
    fee: int | None = None,
    tick_spacing: int | None = None,
    hook_address: str | None = None,
    # Pre-fetched tick data
    tick_bitmap: dict[int, Any] | None = None,
    tick_data: dict[int, Any] | None = None,
) -> UniswapV4Pool:
    """
    Build a V4 managed pool from a PoolManager address and pool ID.

    `address` is the PoolManager contract. `pool_id` identifies the pool
    within the manager.

    When the pool is not in the database, `state_view_address`, `tokens`,
    `fee`, `tick_spacing` must all be provided.
    """
    address = get_checksum_address(address)
    chain_id = chain_id or self.connections.default_chain_id

    # Check managed pool registry — return existing pool if already built
    pool_id_bytes = HexBytes(pool_id)
    existing = self.managed_pools.get(
        chain_id=chain_id,
        pool_manager_address=address,
        pool_id=pool_id_bytes,
    )
    if existing is not None:
        return existing

    provider = self.connections.get_provider(chain_id)
    io = SyncPoolIO(provider)

    request = BuildManagedPoolRequest(
        pool_id=pool_id,
        silent=silent,
        state_block=state_block,
        state_cache_depth=state_cache_depth,
        state_view_address=state_view_address,
        tokens=tokens,
        fee=fee,
        tick_spacing=tick_spacing,
        hook_address=hook_address,
        tick_bitmap=tick_bitmap,
        tick_data=tick_data,
    )

    return self._dispatch_build(
        builder=self._v4_builder,
        address=address,
        chain_id=chain_id,
        io=io,
        request=request,
    )
```

The `AsyncBot` variant mirrors this with `await` on `_dispatch_build` and `AsyncPoolIO`.

### Step 3: Consolidate `FACTORY_DEPLOYMENTS` into `pool_type_registry`

Make `pool_type_registry` the sole source of truth for `(chain_id, factory) → (deployer, pool_init_hash)`.

1. **Each DEX `__init__.py`** currently reads from `FACTORY_DEPLOYMENTS` and passes to `pool_type_registry.register()`. Replace with direct hardcoded values passed to `register()` — the data was already hardcoded in `deployments.py`, just source it directly.

2. **Pool constructors** — remove the `FACTORY_DEPLOYMENTS` lookup from `UniswapV2Pool.__init__()` and `UniswapV3Pool.__init__()`. The constructors trust the `deployer_address`/`init_hash` they receive from the builder. This eliminates the silent overwrite that made builder resolution "correct by accident" for unregistered factories.

3. **Trackers** — switch from `FACTORY_DEPLOYMENTS[chain_id][factory]` to `pool_type_registry.get_deployment(chain_id, factory)`. Same data, canonical source. The tracker still uses deployer/init_hash for address generation (`generate_v2_pool_address`, `generate_v3_pool_address`).

4. **`AsyncBot._resolve_deployment()`** — switch from `_FACTORY_DEPLOYMENTS[chain_id][factory]` to `pool_type_registry.get_deployment(chain_id, factory)`. This method is a duplicate of `V2BuilderBase.resolve_deployer_and_init_hash()` — same three-layer resolution (registry → override → fallback), different source. After consolidation, both use `pool_type_registry`. (This method is then a candidate for removal — the builder already resolves deployer/init_hash, so the async bot doesn't need to.)

5. **Delete `FACTORY_DEPLOYMENTS` dict and `register_exchange()`** from `deployments.py`. Keep the `UniswapFactoryDeployment`, `UniswapV2ExchangeDeployment`, `UniswapV3ExchangeDeployment`, and `UniswapV4ExchangeDeployment` dataclasses (they define the exchange metadata structure used by V4 and by the public API), but remove the `FACTORY_DEPLOYMENTS` dict and the `register_exchange()` function that populates it.

**User-facing impact**: `pool_type_registry.register()` is already the documented public API for adding custom exchanges. After consolidation, users continue to call it exactly as before. The only behavioral difference is positive: user-registered exchanges get correct deployer/init_hash resolution by design (the pool constructor uses the values the builder resolved from the registry) instead of by accident (a silently-suppressed `KeyError` in the constructor's `FACTORY_DEPLOYMENTS` lookup).

### Step 4: Remove `deployer_address`/`init_hash` from `BuildPoolRequest` entirely

With `FACTORY_DEPLOYMENTS` gone and pool constructors trusting builder-provided values, the internal transport of deployer/init_hash through `BuildPoolRequest` is no longer needed:

- **V2 builder**: `resolve_deployer_and_init_hash()` resolves from `pool_type_registry` and passes the result via `V2CommonData` directly to the pool constructor. The `deployer_override`/`init_hash_override` parameters to this method were sourced from `request.deployer_address`/`request.init_hash`. After removal, the overrides are gone — the registry is the sole source. The method signature simplifies to just `(chain_id, factory, default_init_hash)`.

- **V3 builder**: The `dataclasses.replace(request, deployer_address=db_values.deployer_address)` pattern is eliminated. The DB-resolved deployer is passed directly as a local variable to the pool constructor, not smuggled through the request.

- **Trackers**: Already switched to `pool_type_registry.get_deployment()` in Step 3. No longer pass `deployer_address`/`init_hash` to `build_pool()`.

```python
# After
@dataclass(slots=True, frozen=True, kw_only=True)
class BuildPoolRequest:
    """Typed request object carrying optional parameters for pool construction.

    Carries options for non-V4 pool construction. V4 managed pools use
    BuildManagedPoolRequest instead.
    """

    # Common options
    silent: bool = False
    state_block: int | None = None
    state_cache_depth: int = 8

    # Pre-fetched tick data (DB snapshot or test fixtures)
    tick_bitmap: dict[int, Any] | None = None
    tick_data: dict[int, Any] | None = None

    # Balancer overrides — NOT exposed on Bot.build_pool() public API;
    # used internally by BalancerBuilder for test injection
    bpt_idx: int | None = None
    invariant_version: int | None = None
```

```python
# After
def build_pool(
    self,
    address: str,
    *,
    chain_id: ChainId | None = None,
    state_block: int | None = None,
    silent: bool = False,
    state_cache_depth: int = 8,
    tick_bitmap: dict[int, Any] | None = None,
    tick_data: dict[int, Any] | None = None,
) -> AbstractLiquidityPool:
```

That's 6 kwargs, down from 14.

### Step 5: Remove `pool_id` from Balancer builder's `BuildPoolRequest` read

`balancer_builder.py:99` reads `request.pool_id` as an optional override. After removing `pool_id` from `BuildPoolRequest`, the Balancer builder unconditionally fetches pool ID on-chain via `_fetch_pool_id()`, which is what it already does in the `request.pool_id is None` branch. The override path (which was V4 semantics leaking into Balancer) is removed.

### Step 6: Update V4 builders to accept `BuildManagedPoolRequest`

`V4PoolBuilder.build()` and `AsyncV4PoolBuilder.build()` currently accept `request: BuildPoolRequest`. Update their signatures to `request: BuildManagedPoolRequest`. The internal logic reads the same fields — no behavioral change. Remove the `assert request.pool_id is not None` guard (now guaranteed by the type).

The `PoolBuilder` protocol's `build()` signature takes `request: BuildPoolRequest`. Since `BuildManagedPoolRequest` is a standalone type (not inheriting from `BuildPoolRequest`), the protocol needs to accommodate both. Broaden the `request` parameter to a union type `BuildPoolRequest | BuildManagedPoolRequest`. Update `_dispatch_build()` to accept the union and pass it through.

### Design decisions

- **Method extraction over sub-object scoping**: V4 pools have a fundamentally different addressing model (`(manager, pool_id)` vs. `address` alone), a different discovery contract (caller must provide immutable data if not in DB), and a different dispatch path (no type resolution — `pool_id` is the discriminator). Scoping sub-objects on the same request bag would mask these structural differences; a separate method makes the split explicit and truthful.

- **`pool_id` is required on `BuildManagedPoolRequest`**: Today `pool_id: str | bytes | None = None` on `BuildPoolRequest`, but V4 pools *cannot be built without it*. Making it required is both more correct and more discoverable — callers get a type error instead of a runtime `assert`.

- **Standalone dataclass (not inheritance)**: `BuildManagedPoolRequest` does not inherit from `BuildPoolRequest`. The two types represent fundamentally different operations with different required data — `BuildManagedPoolRequest` requires `pool_id`, `state_view_address`, etc., while `BuildPoolRequest` carries `tick_bitmap`, `bpt_idx`. Inheritance would let code treating a `BuildManagedPoolRequest` as a `BuildPoolRequest` access fields like `bpt_idx` that have no meaning for V4. A union type on the protocol is more honest.

- **Protocol request type — union over inheritance**: The `PoolBuilder` protocol's `build()` parameter becomes `request: BuildPoolRequest | BuildManagedPoolRequest`. V4 builder's `build()` asserts `isinstance(request, BuildManagedPoolRequest)`. Other builders assert or read `BuildPoolRequest`-specific fields. This is slightly more verbose than inheritance but correctly models that these are two separate request shapes, not a subtype hierarchy.

- **`pool_type_registry` as sole source of truth**: `pool_type_registry.register()` is already the documented public API for adding custom exchanges (see its docstring example). Consolidating on it means:
  - User-registered factories: work by design (constructor trusts builder-provided values from the registry), not by accident (silently-suppressed `KeyError` in `FACTORY_DEPLOYMENTS`).
  - Unregistered factories: the builder raises `DegenbotValueError` — same as today. The pool constructor's `FACTORY_DEPLOYMENTS` lookup didn't help for these either; it would also miss, fall back to `factory` as deployer and a class-level default init_hash, then potentially produce a wrong `_verified_address`.
  - The `deployer_address`/`init_hash` kwargs on `build_pool()` were escape hatches for pre-registry workflows. With `pool_type_registry` as the sole source, the registry itself is the escape hatch — `register()` your factory, then call `build_pool()`.

- **`FACTORY_DEPLOYMENTS` deletion scope**: The `FACTORY_DEPLOYMENTS` *dict* is deleted. The `UniswapV2ExchangeDeployment`, `UniswapV3ExchangeDeployment`, `UniswapV4ExchangeDeployment` dataclasses and the per-exchange instances (e.g. `EthereumMainnetUniswapV2`) are kept — they define the exchange metadata structure used by V4 and are referenced by the V4 builder's `pool_type_registry` lookups. The `register_exchange()` function is deleted — it only populated the `FACTORY_DEPLOYMENTS` dict, which no longer exists.

- **Deduplication in `build_managed_pool()`**: The method checks `self.managed_pools.get()` before constructing a new pool, mirroring `build_pool()`'s existing `self.pools.get()` check. The current `build_pool()` V4 fast path *does not* check the managed pool registry — it always delegates to the builder, which re-registers the pool. This is a latent bug that `build_managed_pool()` fixes.

- **`build_managed_pool` returns `UniswapV4Pool`**: Unlike `build_pool()` which returns `AbstractLiquidityPool`, the managed-pool method returns the concrete type. Callers know they're building a V4 pool and shouldn't need to cast.

- **`bpt_idx`/`invariant_version` stay on `BuildPoolRequest`**: These are not exposed on `Bot.build_pool()` — they're consumed internally by `BalancerBuilder` for test injection. They're not causing public-API pollution. Removing them would require a separate Balancer-specific request type for a minor internal-only gain. Leave them for now.

- **Breaking change for custom `PoolBuilder` implementations**: Broadening the protocol's `request` parameter to `BuildPoolRequest | BuildManagedPoolRequest` is a breaking change for external implementations of `PoolBuilder`. Any custom builder that annotated `request: BuildPoolRequest` will need to accept the union. Mitigation: custom V4 builders are essentially nonexistent today. Non-V4 builders are unaffected (they only ever receive `BuildPoolRequest`).

- **`AsyncBot._resolve_deployment()` is now redundant**: After consolidation, this method reads from `pool_type_registry` — the same source as `V2BuilderBase.resolve_deployer_and_init_hash()`. The async bot calls it before dispatching to builders, but the builders resolve deployer/init_hash themselves. This method can be removed in a follow-up (the async builders already have the registry). Not in this plan's scope to avoid scope creep.

## Files Involved

**Primary:**
- `src/degenbot/builders/request.py` — add `BuildManagedPoolRequest`, remove `deployer_address`/`init_hash` and V4 fields from `BuildPoolRequest`
- `src/degenbot/builders/protocol.py` — broaden `request` parameter to `BuildPoolRequest | BuildManagedPoolRequest`
- `src/degenbot/bot.py` — add `build_managed_pool()`, narrow `build_pool()`, update `_dispatch_build()` signature
- `src/degenbot/async_bot.py` — add `build_managed_pool()`, narrow `build_pool()`, update `_dispatch_build()` signature
- `src/degenbot/uniswap/deployments.py` — delete `FACTORY_DEPLOYMENTS` dict and `register_exchange()`, keep dataclasses and exchange instances
- `src/degenbot/uniswap/v2_liquidity_pool.py` — remove `FACTORY_DEPLOYMENTS` lookup from `__init__()`
- `src/degenbot/uniswap/v3_liquidity_pool.py` — remove `FACTORY_DEPLOYMENTS` lookup from `__init__()`

**Secondary:**
- `src/degenbot/builders/v4_pool_builder.py` — accept `BuildManagedPoolRequest`, remove `assert request.pool_id is not None`
- `src/degenbot/builders/async_v4_pool_builder.py` — accept `BuildManagedPoolRequest`, remove `assert request.pool_id is not None`
- `src/degenbot/builders/balancer_builder.py` — remove `request.pool_id` read (unconditionally fetch on-chain)
- `src/degenbot/builders/v2_pool_builder.py` — remove `deployer_address`/`init_hash` from `_fetch_v2_common_data()` and `resolve_deployer_and_init_hash()` (registry-only)
- `src/degenbot/builders/v3_pool_builder.py` — replace `dataclasses.replace(request, deployer_address=...)` with local variable; remove `request.deployer_address`/`request.init_hash` reads
- `src/degenbot/builders/v2_builder_base.py` — simplify `resolve_deployer_and_init_hash()` (remove override params, registry-only); remove `deployer_address`/`init_hash` from `_fetch_v2_common_data()`
- `src/degenbot/builders/async_v2_pool_builder.py` — same as V2 sync
- `src/degenbot/builders/async_v3_pool_builder.py` — same as V3 sync
- `src/degenbot/builders/camelot_builder.py` — same as V2 sync
- `src/degenbot/builders/aerodrome_v2_builder.py` — same as V2 sync
- `src/degenbot/uniswap/trackers.py` — switch to `pool_type_registry.get_deployment()`, remove `deployer_address`/`init_hash` from `build_pool()` calls
- `src/degenbot/aerodrome/trackers.py` — switch to `pool_type_registry.get_deployment()`
- Each DEX `__init__.py` (uniswap, sushiswap, pancakeswap, aerodrome, camelot, swapbased) — stop reading from `FACTORY_DEPLOYMENTS`; pass hardcoded values directly to `pool_type_registry.register()`

**No change:**
- `src/degenbot/builders/curve_pool_builder.py` — doesn't read removed fields
- `src/degenbot/cli/exchange.py` — imports per-exchange instances (e.g. `BaseUniswapV2`) from `deployments.py`, not `FACTORY_DEPLOYMENTS` itself. These instances are kept by the plan.

**Test:**
- `tests/uniswap/v4/test_uniswap_v4_liquidity_pool.py` — switch to `build_managed_pool()`
- `tests/uniswap/v4/test_v4_pool_io_free.py` — switch to `build_managed_pool()`
- `tests/uniswap/v4/test_v4_simulator.py` — switch to `build_managed_pool()`
- `tests/arbitrage/integration/test_uniswap_2pool_cycle.py` — switch to `build_managed_pool()`
- `tests/test_async_bot.py` — switch to `build_managed_pool()`
- Various V2/V3 test files — remove `deployer_address`/`init_hash` kwargs from `build_pool()` calls
- `tests/exchanges/test_uniswap_exchanges.py` — rewrite from `register_exchange()`/`FACTORY_DEPLOYMENTS` assertions to `pool_type_registry.register()`/`pool_type_registry.get_deployment()` assertions (Slice 4)
- `tests/registry/test_full_exchange_registration.py` — remove `FACTORY_DEPLOYMENTS` cross-checks; `pool_type_registry` is now the source of truth, so tests validate registry data directly (Slice 4)
- `tests/registry/test_pool_type_registry_singleton.py` — same: remove `FACTORY_DEPLOYMENTS` cross-checks (Slice 4)
- `tests/registry/test_pool_type_resolution.py` — rewrite `TestPoolTypeInvariants.test_all_v2_pool_classes_derive_constant_product` to iterate over `pool_type_registry.registrations` instead of `FACTORY_DEPLOYMENTS.items()`; update import (Slice 4)

## Implementation Order

### Slice 1: Create `BuildManagedPoolRequest` and `build_managed_pool()`

1. Add standalone `BuildManagedPoolRequest` to `request.py` with `pool_id: str | bytes` (required) plus the V4-specific optional fields
2. Broaden `PoolBuilder.build()` / `AsyncPoolBuilder.build()` protocol `request` parameter to `BuildPoolRequest | BuildManagedPoolRequest`
3. Update `Bot._dispatch_build()` and `AsyncBot._dispatch_build()` signatures to accept the union
4. Add `build_managed_pool()` to `Bot` — checks managed pool registry for existing pool, then delegates to `_dispatch_build()` with the V4 builder and a `BuildManagedPoolRequest`
5. Add `build_managed_pool()` to `AsyncBot` — same pattern, async, with `AsyncPoolIO`
6. Update `V4PoolBuilder.build()` and `AsyncV4PoolBuilder.build()` to `assert isinstance(request, BuildManagedPoolRequest)` and read `request.pool_id` directly (no longer optional)
7. Run: `just test-python` — existing tests still use `build_pool()` with `pool_id=`; these still work because the old path is not yet removed

### Slice 2: Migrate V4 call sites to `build_managed_pool()`

1. Update all test files that call `build_pool()` with `pool_id=` to use `build_managed_pool()` instead
2. Run: `just test-python` — all green, V4 pools now built through new path

### Slice 3: Remove V4 kwargs and `pool_id` from `build_pool()`

1. Remove `pool_id`, `state_view_address`, `tokens`, `fee`, `tick_spacing`, `hook_address` from `Bot.build_pool()` signature
2. Remove `pool_id`, `state_view_address`, `tokens`, `fee`, `tick_spacing`, `hook_address` from `AsyncBot.build_pool()` signature
3. Remove the V4 fast path (`if pool_id is not None: ...`) from both `build_pool()` methods
4. Remove `pool_id`, `state_view_address`, `tokens`, `fee`, `tick_spacing`, `hook_address` from `BuildPoolRequest`
5. Remove `request.pool_id` read from `balancer_builder.py` (unconditionally fetch on-chain)
6. Run: `just test-python` — all green, `build_pool()` now 8 kwargs

### Slice 4: Consolidate `FACTORY_DEPLOYMENTS` into `pool_type_registry`

1. Read through each DEX `__init__.py` and replace `FACTORY_DEPLOYMENTS.get(chain_id, {}).get(factory)` → pass hardcoded values directly to `pool_type_registry.register()`
2. Switch `uniswap/trackers.py` and `aerodrome/trackers.py` from `FACTORY_DEPLOYMENTS[chain_id][factory]` → `pool_type_registry.get_deployment(chain_id, factory)`
3. Switch `async_bot.py`'s `_resolve_deployment()` from `_FACTORY_DEPLOYMENTS[chain_id][factory]` → `pool_type_registry.get_deployment(chain_id, factory)`
4. Remove the `FACTORY_DEPLOYMENTS` lookup from `UniswapV2Pool.__init__()` and `UniswapV3Pool.__init__()` — constructors trust the builder-provided `deployer_address`/`init_hash`
5. Delete `FACTORY_DEPLOYMENTS` dict and `register_exchange()` from `deployments.py`; keep the exchange dataclasses and per-exchange instances (referenced by V4)
6. Rewrite `tests/exchanges/test_uniswap_exchanges.py` to test `pool_type_registry.register()` instead of `register_exchange()`/`FACTORY_DEPLOYMENTS`
7. Rewrite `TestDeploymentDataMatchesFactoriesModule` in `test_full_exchange_registration.py` and `test_pool_type_registry_singleton.py` — remove `FACTORY_DEPLOYMENTS` cross-checks; `pool_type_registry` is now the source of truth
8. Rewrite `TestPoolTypeInvariants.test_all_v2_pool_classes_derive_constant_product` in `test_pool_type_resolution.py` to iterate over `pool_type_registry.registrations` instead of `FACTORY_DEPLOYMENTS.items()`
9. Run: `just test-python` — all green
10. Run a representative fork test per chain (mainnet V2, mainnet V3, base V2) to verify `_verified_address` computation still matches on-chain addresses

### Slice 5: Remove `deployer_address`/`init_hash` from `build_pool()` public signature and `BuildPoolRequest`

1. Remove `deployer_address` and `init_hash` from `Bot.build_pool()` and `AsyncBot.build_pool()` signatures
2. Remove `deployer_address`/`init_hash` kwargs from `build_pool()` calls in `uniswap/trackers.py` and `aerodrome/trackers.py`
3. Remove `deployer_address`/`init_hash` from `BuildPoolRequest` — no longer needed as internal transport
4. Simplify `V2BuilderBase.resolve_deployer_and_init_hash()` — remove `deployer_override`/`init_hash_override` params (registry-only resolution)
5. Replace `dataclasses.replace(request, deployer_address=...)` in V3/async-V3 builders with direct local variable usage (DB deployer flows to the pool constructor as a local, not through the request)
6. Update `_fetch_v2_common_data()` to resolve deployer/init_hash internally via `resolve_deployer_and_init_hash()` (registry-only — no override parameters). `V2CommonData.deployer`/`V2CommonData.init_hash` fields are kept — they're still consumed by pool constructors. The method signature drops `deployer_address`/`init_hash` parameters; `resolve_deployer_and_init_hash()` drops `deployer_override`/`init_hash_override` parameters.
7. Remove `deployer_address`/`init_hash` parameters from all V2-variant builder `build()` methods that pass them to `_fetch_v2_common_data()`
8. Run: `just test-python` — all green, `build_pool()` now 6 kwargs

### Slice 6: Validate and clean up

1. Run: `just lint` + `just test-all`
2. Update `builders/CONTEXT.md` — document `BuildManagedPoolRequest`, `build_managed_pool()`, removal of `deployer_address`/`init_hash`, consolidation of `FACTORY_DEPLOYMENTS` into `pool_type_registry`
3. Verify no test files still reference removed kwargs (grep for `pool_id=`, `state_view_address=`, `deployer_address=`, `init_hash=` in `build_pool()` call sites)

## Testing

### Per-slice test runs

Each slice runs `just test-python`.

### New unit tests

```python
# tests/builders/test_build_pool_request.py


def test_build_managed_pool_request_pool_id_required():
    """BuildManagedPoolRequest requires pool_id — no default."""

def test_build_managed_pool_request_standalone():
    """BuildManagedPoolRequest does not inherit from BuildPoolRequest."""

def test_build_pool_request_no_v4_fields():
    """BuildPoolRequest no longer carries V4-specific fields after Slice 3."""

def test_build_pool_request_no_deployer_or_init_hash():
    """BuildPoolRequest no longer carries deployer_address/init_hash after Slice 5."""
```

```python
# tests/builders/test_bot_build_managed_pool.py


def test_build_managed_pool_returns_v4_pool():
    """build_managed_pool returns a UniswapV4Pool, not AbstractLiquidityPool."""

def test_build_managed_pool_deduplicates():
    """Calling build_managed_pool twice with same (address, pool_id) returns
    the same pool object by identity."""

def test_build_pool_rejects_pool_id():
    """build_pool() no longer accepts pool_id kwarg after Slice 3."""

def test_build_pool_rejects_deployer_and_init_hash():
    """build_pool() no longer accepts deployer_address or init_hash kwargs
    after Slice 5."""

def test_build_managed_pool_pool_id_required():
    """build_managed_pool() requires pool_id as a positional argument."""
```

```python
# tests/builders/test_deployer_resolution.py


def test_v2_builder_resolves_deployer_from_registry():
    """V2 builder resolves deployer from pool_type_registry, not from
    build_pool() kwargs."""

def test_v3_builder_resolves_deployer_from_registry():
    """V3 builder resolves deployer from pool_type_registry, not from
    build_pool() kwargs."""

def test_pool_constructor_trusts_builder_values():
    """Pool constructor uses the deployer/init_hash passed by the builder,
    not a secondary FACTORY_DEPLOYMENTS lookup."""

def test_user_registered_exchange_builds_correctly():
    """A pool whose factory is registered only in pool_type_registry
    (not in any hardcoded dict) resolves deployer/init_hash correctly."""

def test_v2_pool_verified_address_with_registry_deployer():
    """V2 pool's _verified_address uses deployer from pool_type_registry
    to compute the CREATE2 address."""
```

### Integration tests

Existing V4 integration tests (fork tests, I/O-free tests) cover the V4 build path end-to-end. After Slice 2 they call `build_managed_pool()` instead of `build_pool(pool_id=...)` — same coverage, different entry point.

The deduplication test (`test_build_managed_pool_deduplicates`) is new — the current `build_pool()` V4 fast path has a latent bug where it does not check the managed pool registry before constructing. `build_managed_pool()` fixes this, and the test verifies the fix.

The deployer-resolution tests (`test_pool_constructor_trusts_builder_values`, `test_user_registered_exchange_builds_correctly`) are new — they verify that the `FACTORY_DEPLOYMENTS` deletion (Slice 4) preserves correct behavior, and that the constructor trust model works for user-registered factories.

## Benefits

- **Depth**: `build_pool()` is a correct shallow seam for non-V4 pools. `build_managed_pool()` is a deep seam that reflects V4's fundamentally different addressing model and discovery contract.
- **Locality**: V4-specific data lives in `BuildManagedPoolRequest`, colocated with V4 builder logic, not in the shared type.
- **Leverage**: `pool_type_registry` becomes the single source of truth for deployer/init_hash resolution, used by builders, trackers, and pool constructors through one interface. Adding a new DEX requires only one `register()` call — no need to update a second dict.
- **Correctness**: `pool_id` is required where it's needed and absent where it isn't. No more `assert request.pool_id is not None` at the top of the V4 builder. User-registered exchanges get correct deployer resolution by design, not by silently-suppressed `KeyError`. Deduplication check in `build_managed_pool()` fixes a latent bug.
- **Type narrowing**: `build_managed_pool()` returns `UniswapV4Pool`, not `AbstractLiquidityPool`. Callers get a concrete type.
- **API surface reduction**: `build_pool()` drops from 14 kwargs to 6. `deployer_address`/`init_hash` are removed from both the public API and `BuildPoolRequest` (no more internal transport). `FACTORY_DEPLOYMENTS` dict is deleted. Dead Balancer `pool_id` override is removed.

## Risks

- **Migration surface**: Every `build_pool()` call site that passes `pool_id=` must switch to `build_managed_pool()`. Mitigation: greppable — `grep -n 'pool_id=' tests/`. Only test files and no production code outside `Bot`/`AsyncBot` do this.
- **Protocol union type is a breaking change for custom builders**: External implementations of `PoolBuilder` that annotated `request: BuildPoolRequest` will see a type mismatch when the protocol broadens to `BuildPoolRequest | BuildManagedPoolRequest`. Mitigation: custom V4 builders are essentially nonexistent. Non-V4 builders only ever receive `BuildPoolRequest` at runtime, so custom builders that don't narrow the type are unaffected in practice.
- **`FACTORY_DEPLOYMENTS` deletion removes `register_exchange()`**: Any code calling `register_exchange()` (populating `FACTORY_DEPLOYMENTS` dynamically) must switch to `pool_type_registry.register()`. Mitigation: `register_exchange()` is only called in `deployments.py` itself (the module-level registrations). No external usage found in the codebase.
- **Pool constructor trust model**: Removing the `FACTORY_DEPLOYMENTS` fallback from pool constructors changes the semantics of `deployer_address=None` and `init_hash=None` for direct construction (not via `Bot.build_pool()`). Previously, `FACTORY_DEPLOYMENTS` would silently override these defaults for known factories; now `deployer_address=None` falls through to `self.factory` unconditionally. This is safe because (a) the AGENTS.md convention is to use `Bot.build_pool()` not direct construction, (b) I/O-free construction already provides all data, and (c) the fallback was `self.factory` anyway for unregistered factories. But it should be documented in the Slice 4 commit message as a semantic change.
- **`_verified_address` correctness**: The pool's `_verified_address` property depends on correct `deployer`/`init_hash`. After the `FACTORY_DEPLOYMENTS` fallback is removed, a wrong registry entry could produce a wrong `_verified_address` that previously would have been masked by the constructor's override. Mitigation: existing fork-based integration tests implicitly cover address verification. Slice 4 should explicitly include running at least one fork test per chain to verify address computation.
- **Balancer `pool_id` override removal**: `balancer_builder.py` loses the ability to bypass on-chain pool ID fetching. Mitigation: this override was V4 semantics leaking into Balancer; the builder's `_fetch_pool_id()` is the correct path.

## Relationship to Other Plans

- **Plan 070** (Balancer Builder): Independent. Plan 072 removes `request.pool_id` from the Balancer builder path, which is a minor simplification of Plan 070's code. No prerequisite or blocker.
- **Plan 014** (Async REPL): Orthogonal — `AsyncBot.build_managed_pool()` mirrors `Bot.build_managed_pool()` the same way `build_pool()` is mirrored today.

## Status

[x] Slice 1: Create `BuildManagedPoolRequest` and `build_managed_pool()`
[x] Slice 2: Migrate V4 call sites to `build_managed_pool()`
[x] Slice 3: Remove V4 kwargs and `pool_id` from `build_pool()`
[x] Slice 4: Consolidate `FACTORY_DEPLOYMENTS` into `pool_type_registry`
[x] Slice 5: Remove `deployer_address`/`init_hash` from `build_pool()` and `BuildPoolRequest`
[x] Slice 6: Validate and clean up
