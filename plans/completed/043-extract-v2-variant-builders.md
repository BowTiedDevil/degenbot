# Plan 043: Extract Per-Variant V2 Sub-Builders

## Overview

Replace the `issubclass` / `isinstance` dispatch inside `V2PoolBuilder` with per-variant sub-builders registered in the `Bot._builders` dict. Adding a new V2 variant becomes a single class + registration, not another `elif` branch inside the monolithic builder.

This extends the Builder Registry pattern (Plan 028) to its logical conclusion: `Bot._builders` already dispatches by `type(pool)` for `update()` — but inside the V2 builder, the same dispatch reappears as `issubclass` / `isinstance` chains.

## Files Involved

**Primary:**
- `src/degenbot/builders/v2_pool_builder.py` — extract `_build_aerodrome_v2` and `_build_camelot` into standalone builder classes; remove `issubclass` / `isinstance` chains from `build()` and `update()`
- `src/degenbot/bot.py` — register `AerodromeV2Pool → AerodromeV2Builder`, `CamelotLiquidityPool → CamelotBuilder` in addition to `UniswapV2Pool → V2PoolBuilder`

**Secondary:**
- `src/degenbot/aerodrome/pools.py` — no change
- `src/degenbot/camelot/pools.py` — no change
- `src/degenbot/builders/protocol.py` — no change (builders already satisfy `PoolBuilder` protocol)
- `src/degenbot/registry/pool_type.py` — no change (registry already maps factories to pool classes)
- `tests/builders/test_v2_pool_builder.py` — split into per-variant builder tests

## Problem

### Deletion test

If you deleted `V2PoolBuilder`, 5 pool types would have no construction path. It earns its keep. But the `issubclass` chain inside `build()` and the `isinstance` chain inside `update()` are the exact pattern that `Bot._builders` was designed to eliminate — they just moved one level down.

### Specific dispatch points

**In `build()`:**
```python
if issubclass(pool_class, AerodromeV2Pool):
    pool = self._build_aerodrome_v2(...)
elif issubclass(pool_class, CamelotLiquidityPool):
    pool = self._build_camelot(...)
else:
    pool = pool_class(...)
```

**In `update()`:**
```python
if isinstance(pool, AerodromeV2Pool):
    return self._update_aerodrome_v2(pool, ...)
if isinstance(pool, UniswapV2Pool):
    return self._update_uniswap_v2(pool, ...)
```

These are the same dispatch pattern that used to live in `Bot._builder_for_pool()` before Plan 028 replaced it with `dict[type, PoolBuilder]` lookup.

### Why this matters for adding a new V2 variant

Current workflow (5 steps):
1. Create the pool class
2. Register it in `pool_type_registry`
3. Add `elif issubclass(pool_class, NewPool): pool = self._build_new_pool(...)` to `V2PoolBuilder.build()`
4. Add `_build_new_pool()` method to `V2PoolBuilder`
5. Add `if isinstance(pool, NewPool): return self._update_new_pool(pool)` to `V2PoolBuilder.update()`

Target workflow (3 steps):
1. Create the pool class
2. Register it in `pool_type_registry`
3. Create `NewPoolBuilder`, register in `bot.register_builder(NewPool, NewPoolBuilder(...))`

## Solution

### Step 1: Define a `V2BuilderBase` with shared I/O orchestration

The common logic in `V2PoolBuilder.build()` that all V2 variants share:

1. Check pool registry for existing pool
2. Look up pool in DB
3. Fetch factory, token0, token1 from DB or chain
4. Build tokens via `Erc20Builder`
5. Fetch reserves via `raw_call`
6. Fetch deployer and init_hash from `pool_type_registry`
7. Resolve pool class from `pool_type_registry`
8. Register pool in `PoolRegistry`

Steps 1–7 are identical for all V2 variants. Only step 8+ (variant-specific construction) differs.

```python
class V2BuilderBase:
    """Base class for V2-style pool builders.
    
    Provides shared I/O orchestration (DB lookup, chain fetch,
    token construction, reserve fetch, registry lookup).
    Subclasses implement variant-specific construction and update.
    """
    
    def __init__(
        self,
        *,
        connections: ConnectionManager,
        db: DatabaseSessionManager,
        pools: PoolRegistry,
        tokens: TokenRegistry,
        erc20_builder: Erc20Builder,
    ) -> None:
        self._connections = connections
        self._db = db
        self._pools = pools
        self._tokens = tokens
        self._erc20_builder = erc20_builder
    
    def _fetch_v2_common_data(
        self,
        pool_address: str,
        *,
        chain_id: ChainId,
        state_block: int,
        deployer_address: str | None,
        init_hash: str | None,
        provider: ProviderAdapter,
    ) -> V2CommonData:
        """Fetch data shared by all V2 variants.
        
        Returns a frozen dataclass with all values needed
        for variant-specific construction.
        """
        ...
    
    def _register_pool(self, pool: AbstractLiquidityPool, *, chain_id: ChainId) -> None:
        self._pools.add(pool_address=pool.address, chain_id=chain_id, pool=pool)
    
    @abstractmethod
    def build(self, address: str, *, chain_id: ChainId | None = None, ...) -> AbstractLiquidityPool: ...
    
    @abstractmethod
    def update(self, pool: Any, *, block_number: BlockIdentifier | None = None) -> bool: ...
```

### Step 2: Define `V2CommonData` frozen dataclass

```python
@dataclass(frozen=True)
class V2CommonData:
    """Data fetched from DB/chain that all V2 variants need."""

    pool_address: ChecksumAddress
    chain_id: ChainId
    factory: ChecksumAddress
    token0: Erc20Token
    token1: Erc20Token
    reserves0: int
    reserves1: int
    deployer: ChecksumAddress
    init_hash: str
    fee_token0: Fraction
    fee_token1: Fraction
    state_block: int
```

This replaces the ~50 lines of common preamble in `build()` with a single call to `self._fetch_v2_common_data(...)`.

### Step 3: Create `AerodromeV2Builder`

```python
class AerodromeV2Builder(V2BuilderBase):
    """Builds and updates Aerodrome V2 pools."""

    def build(
        self,
        pool_address: str,
        *,
        chain_id=None,
        deployer_address=None,
        init_hash=None,
        state_block=None,
        silent=False,
        state_cache_depth=8,
    ) -> AerodromeV2Pool:
        pool_address = get_checksum_address(pool_address)
        chain_id = chain_id or self._connections.default_chain_id
        provider = self._connections.get_provider(chain_id)
        state_block = state_block or provider.get_block_number()

        common = self._fetch_v2_common_data(
            pool_address,
            chain_id=chain_id,
            state_block=state_block,
            deployer_address=deployer_address,
            init_hash=init_hash,
            provider=provider,
        )

        # Aerodrome-specific: fetch stable flag and fee
        stable_result = provider.call(
            to=pool_address, data=encode_function_calldata("stable()", None), block=state_block
        )
        (stable,) = eth_abi.abi.decode(types=["bool"], data=stable_result)

        fee_result = provider.call(
            to=common.factory,
            data=encode_function_calldata("getFee(address,bool)", [pool_address, stable]),
            block=state_block,
        )
        (fee_raw,) = eth_abi.abi.decode(types=["uint256"], data=fee_result)
        fee = Fraction(fee_raw, AerodromeV2Pool.FEE_DENOMINATOR)

        pool = AerodromeV2Pool(
            address=common.pool_address,
            token0=common.token0,
            token1=common.token1,
            factory=common.factory,
            fee=fee,
            stable=stable,
            reserves_token0=common.reserves0,
            reserves_token1=common.reserves1,
            chain_id=common.chain_id,
            deployer_address=common.deployer,
            state_block=common.state_block,
        )

        self._register_pool(pool, chain_id=chain_id)

        if not silent:
            logger.info(pool.name)
            logger.info(f"• Token 0: {common.token0} - Reserves: {common.reserves0}")
            logger.info(f"• Token 1: {common.token1} - Reserves: {common.reserves1}")

        return pool

    def update(self, pool, *, block_number=None) -> bool:
        if not isinstance(pool, AerodromeV2Pool):
            msg = f"AerodromeV2Builder cannot update {type(pool).__name__}"
            raise TypeError(msg)

        provider = self._connections.get_provider(pool.chain_id)
        _block_number = block_number or provider.get_block_number()
        reserves0, reserves1 = raw_call(
            provider,
            address=pool.address,
            calldata=encode_function_calldata("getReserves()", None),
            return_types=["uint256", "uint256"],
            block_identifier=_block_number,
        )

        if pool.reserves_token0 == reserves0 and pool.reserves_token1 == reserves1:
            return False

        update = AerodromeV2PoolExternalUpdate(
            block_number=_block_number,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
        )
        pool.external_update(update)
        return True
```

### Step 4: Create `CamelotBuilder`

Same pattern — extends `V2BuilderBase`, fetches Camelot-specific chain data (`stableSwap()`, `FEE_DENOMINATOR()`, `token0FeePercent()`, `token1FeePercent()`), constructs `CamelotLiquidityPool`.

### Step 5: Simplify `V2PoolBuilder` to handle only base V2 pools

After extracting Aerodrome and Camelot, the `V2PoolBuilder.build()` method has no more `issubclass` branches:

```python
class V2PoolBuilder(V2BuilderBase):
    """Builds and updates base Uniswap V2-style pools."""
    
    def build(self, pool_address, *, chain_id=None, ...) -> UniswapV2Pool:
        pool_address = get_checksum_address(pool_address)
        chain_id = chain_id or self._connections.default_chain_id
        provider = self._connections.get_provider(chain_id)
        state_block = state_block or provider.get_block_number()
        
        common = self._fetch_v2_common_data(...)
        
        pool_class = pool_type_registry.get_v2_class(chain_id, common.factory)
        
        pool = pool_class(
            address=common.pool_address,
            chain_id=common.chain_id,
            token0=common.token0,
            token1=common.token1,
            factory=common.factory,
            fee_token0=common.fee_token0,
            fee_token1=common.fee_token1,
            reserves_token0=common.reserves0,
            reserves_token1=common.reserves1,
            state_block=common.state_block,
            deployer_address=common.deployer,
            init_hash=common.init_hash,
        )
        
        self._register_pool(pool, chain_id=chain_id)
        
        if not silent:
            logger.info(pool.name)
        
        return pool
    
    def update(self, pool, *, block_number=None) -> bool:
        if not isinstance(pool, UniswapV2Pool):
            msg = f"V2PoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)
        ...  # same as current _update_uniswap_v2
```

No `issubclass` or `isinstance` chains. Each builder handles exactly one pool type (plus subclasses for Sushiswap/Pancakeswap/Swapbased which share the same constructor).

### Step 6: Update `Bot.__init__` to register per-variant builders

```python
class Bot:
    def __init__(self, config: DegenbotConfig) -> None:
        ...
        self._erc20_builder = Erc20Builder(...)
        self._v2_builder = V2PoolBuilder(...)
        self._aerodrome_v2_builder = AerodromeV2Builder(...)
        self._camelot_builder = CamelotBuilder(...)
        ...
        
        self._builders: dict[type, PoolBuilder] = {}
        self.register_builder(UniswapV2Pool, self._v2_builder)
        self.register_builder(AerodromeV2Pool, self._aerodrome_v2_builder)
        self.register_builder(CamelotLiquidityPool, self._camelot_builder)
        # Sushiswap, Pancakeswap, Swapbased are subclasses of UniswapV2Pool,
        # so they're handled by the MRO fallback in _builder_for_pool()
```

### Step 7: Remove `_build_aerodrome_v2` and `_build_camelot` from `V2PoolBuilder`

They've moved to their own builders. The `update()` isinstance chain is also gone.

## Implementation Order

1. **Create `V2CommonData` frozen dataclass** in a new `src/degenbot/builders/v2_common.py`
2. **Create `V2BuilderBase`** with `_fetch_v2_common_data` and `_register_pool` helper methods
3. **Create `AerodromeV2Builder`** extending `V2BuilderBase` — extract from `V2PoolBuilder._build_aerodrome_v2()` and `_update_aerodrome_v2()`
4. **Create `CamelotBuilder`** extending `V2BuilderBase` — extract from `V2PoolBuilder._build_camelot()`
5. **Simplify `V2PoolBuilder`** — remove `issubclass` and `isinstance` chains, use `V2BuilderBase`
6. **Update `Bot.__init__`** — register `AerodromeV2Pool → AerodromeV2Builder`, `CamelotLiquidityPool → CamelotBuilder`
7. **Verify all tests pass** — pools build and update correctly via the new builder dispatch
8. **Add per-variant builder tests** — `test_aerodrome_v2_builder.py`, `test_camelot_builder.py`

## Testing

### Per-step test runs

Each step runs `just test-python`. The migration is incremental — existing tests use `bot.build_pool()` which automatically dispatches to the correct builder via the registry.

### New unit tests

- `tests/builders/test_v2_builder_base.py` — `_fetch_v2_common_data()` returns correct `V2CommonData` for DB-backed and chain-backed pools
- `tests/builders/test_aerodrome_v2_builder.py` — building and updating Aerodrome pools via the dedicated builder
- `tests/builders/test_camelot_builder.py` — building and updating Camelot pools via the dedicated builder

### Integration tests

All existing V2 pool tests pass — `bot.build_pool()` dispatches correctly to the right builder via the registry.

## Benefits

- **No `issubclass` / `isinstance` chains** in any builder — the pattern eliminated from `Bot` by Plan 028 is now also eliminated from inside the builders
- **Adding a new V2 variant = 3 steps** (create pool class, register in pool_type_registry, create builder + register in Bot) — not 5 steps with `elif` branches
- **Locality:** each variant's chain-fetch logic sits in its own builder, not inside a 300-line monolithic builder
- **Leverage:** the shared `V2BuilderBase._fetch_v2_common_data()` eliminates the duplicated DB lookup + factory fetch + reserve fetch preamble across all V2 builders
- **Testability:** each variant builder can be tested independently without constructing fake pools of other types

## Risks

- **Builder duplication:** the `update()` methods for Aerodrome and base V2 are nearly identical (fetch reserves, compare, push update). The `V2BuilderBase` can provide a `_update_v2_reserves()` helper to avoid duplication.
- **`V2CommonData` fragility:** if a new V2 variant needs additional common data (beyond what all V2 pools share), `V2CommonData` needs to be extended. This is acceptable — the dataclass is internal to the V2 builder family.
- **MRO fallback still needed:** `SushiswapV2Pool` is a subclass of `UniswapV2Pool` and uses the same constructor, so `Bot._builder_for_pool()` falls back to the MRO-walk to find the V2 builder. This is correct behavior — Sushiswap uses the Uniswap math and constructor, just with a different factory address and variant name. The MRO fallback is cleaner than registering `SushiswapV2Pool → V2PoolBuilder` explicitly.
- **Breaking change for V2PoolBuilder callers:** if external code calls `V2PoolBuilder.build()` directly (bypassing Bot), the `issubclass` removal means it will no longer handle AerodromeV2Pool or CamelotLiquidityPool. This is correct — those pools should be built through their own builders, accessed via `Bot.build_pool()`.

## Relationship to Other Plans

- **Plan 028** (Builder Registry): Complete. This plan extends the builder registry pattern from Bot's dispatch into the builders themselves. It's the next deepening step.
- **Plan 035** (Builder Protocol): Complete. The `PoolBuilder` protocol is already established. `AerodromeV2Builder` and `CamelotBuilder` satisfy the same protocol.
- **Plan 044** (Thin Bot Pass-Throughs): Orthogonal. That plan addresses Bot's convenience methods; this plan addresses the builder internals.

## Status: Complete

- **V2BuilderBase** created in `builders/v2_builder_base.py` with `_fetch_v2_common_data()` (shared I/O), `_register_pool()`, `_log_pool()`, `_fetch_reserves()`
- **V2CommonData** frozen dataclass carries common DB/chain data for all V2 variants
- **AerodromeV2Builder** extracted to `builders/aerodrome_v2_builder.py` (122 lines)
- **CamelotBuilder** extracted to `builders/camelot_builder.py` (133 lines)
- **V2PoolBuilder** simplified from 375 → 118 lines (68% reduction); no `issubclass`/`isinstance` chains
- **Bot** updated: `AerodromeV2Pool → AerodromeV2Builder`, `CamelotLiquidityPool → CamelotBuilder` registered separately
- **`build_v2_pool()`** now routes to correct builder based on factory/pool class
- All builder + V2 + Curve tests pass
