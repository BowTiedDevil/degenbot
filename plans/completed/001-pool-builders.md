# Plan 001: Extract Pool Builders from Bot

> **Note**: References to `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool` are historical — these methods were removed by Plan 059. Use `build_pool()` instead.

**Status: COMPLETE** ✅

## Problem

`Bot` is a 2285-line god class mixing two concerns:
1. **Session lifecycle** — owning registries, connections, config, databases
2. **Pool construction I/O** — orchestrating RPC calls, ABI decoding, DB lookups, class selection, and fetcher injection

The five `build_*` methods (`build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool`, `build_erc20token`) total ~1100 lines. Each embeds low-level RPC call construction, conditional DB fallback, error handling, and subclass selection logic. The deletion test confirms they earn their keep (complexity would reappear across callers), but they're in the wrong module.

## Solution

Extract typed **Pool Builder** classes — one per pool invariant family. Each builder owns the full I/O choreography for its pool type. Bot keeps its session role and delegates construction to builders.

### New modules

```
src/degenbot/builders/
├── __init__.py
├── v2_pool_builder.py      # Constant-product pools (UniswapV2, Sushi, Pancake, Camelot)
├── v3_pool_builder.py      # Concentrated-liquidity pools (UniswapV3, SushiV3, AeroV3, PancakeV3)
├── v4_pool_builder.py      # Singleton-architecture CL pools (UniswapV4)
├── curve_pool_builder.py  # Curve StableSwap pools
├── erc20_builder.py       # ERC-20 token construction
└── base.py                # Abstract builder protocol
```

### Interface

```python
# src/degenbot/builders/base.py


class PoolBuilder(ABC):
    """
    Abstract base for pool construction I/O.

    Builders own the full choreography: DB lookup → RPC fetch → decode →
    construct pool → register. They receive I/O dependencies at construction,
    not from module-level singletons.
    """

    @abstractmethod
    def build(
        self, address: str, *, chain_id: ChainId | None = None, **kwargs
    ) -> AbstractLiquidityPool: ...

    @abstractmethod
    def update(
        self, pool: AbstractLiquidityPool, *, block_number: BlockIdentifier | None = None
    ) -> bool: ...
```

### Key builder methods

#### V2PoolBuilder

```python
class V2PoolBuilder(PoolBuilder):
    def __init__(
        self,
        *,
        connections: ConnectionManager,
        db: DatabaseSessionManager,
        pools: PoolRegistry,
        tokens: TokenRegistry,
    ): ...

    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        state_block: int | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        silent: bool = False,
    ) -> UniswapV2Pool:
        # Current logic from Bot.build_v2_pool (lines 297-474)
        pass

    def update(
        self, pool: AbstractLiquidityPool, *, block_number: BlockIdentifier | None = None
    ) -> bool:
        # Current logic from Bot._update_v2_pool + _update_aerodrome_v2_pool (lines 1868-1905)
        pass
```

#### V3PoolBuilder

```python
class V3PoolBuilder(PoolBuilder):
    def __init__(
        self,
        *,
        connections: ConnectionManager,
        db: DatabaseSessionManager,
        pools: PoolRegistry,
        tokens: TokenRegistry,
        managed_pools: ManagedPoolRegistry,
    ): ...

    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        state_block: int | None = None,
        tick_bitmap: dict | None = None,
        tick_data: dict | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        silent: bool = False,
    ) -> UniswapV3Pool:
        # Current logic from Bot.build_v3_pool (lines 703-969)
        pass

    def update(
        self, pool: AbstractLiquidityPool, *, block_number: BlockIdentifier | None = None
    ) -> bool:
        # Current logic from Bot._update_v3_pool (lines 1906-1950)
        pass
```

#### V4PoolBuilder, CurvePoolBuilder, Erc20Builder

Same pattern — each absorbs the corresponding `build_*` and `_update_*` methods from Bot.

### Bot after extraction

```python
class Bot:
    def __init__(self, config: DegenbotConfig) -> None:
        self.config = config
        self.connections = ConnectionManager()
        self.db = DatabaseSessionManager(...)
        self.pools = PoolRegistry()
        self.tokens = TokenRegistry()
        self.managed_pools = ManagedPoolRegistry()
        self._managers: dict[tuple[ChainId, str], AbstractPoolManager] = {}
        self._check_database_version()

        # Builders own I/O orchestration; Bot hands them its I/O dependencies
        self._v2_builder = V2PoolBuilder(
            connections=self.connections, db=self.db, pools=self.pools, tokens=self.tokens
        )
        self._v3_builder = V3PoolBuilder(
            connections=self.connections,
            db=self.db,
            pools=self.pools,
            tokens=self.tokens,
            managed_pools=self.managed_pools,
        )
        self._v4_builder = V4PoolBuilder(
            connections=self.connections,
            db=self.db,
            pools=self.pools,
            tokens=self.tokens,
            managed_pools=self.managed_pools,
        )
        self._curve_builder = CurvePoolBuilder(
            connections=self.connections, db=self.db, pools=self.pools, tokens=self.tokens
        )
        self._erc20_builder = Erc20Builder(
            connections=self.connections, db=self.db, tokens=self.tokens
        )

    # Delegating methods — thin wrappers preserving the existing public interface
    def build_v2_pool(self, address: str, **kwargs) -> UniswapV2Pool:
        return self._v2_builder.build(address, **kwargs)

    def build_v3_pool(self, address: str, **kwargs) -> UniswapV3Pool:
        return self._v3_builder.build(address, **kwargs)

    def build_v4_pool(self, *, pool_id: str | bytes, **kwargs) -> UniswapV4Pool:
        return self._v4_builder.build(pool_id=pool_id, **kwargs)

    def build_curve_pool(self, address: str, **kwargs) -> CurveStableswapPool:
        return self._curve_builder.build(address, **kwargs)

    def build_erc20token(self, address: str, **kwargs) -> Erc20Token:
        return self._erc20_builder.build(address, **kwargs)

    def update(self, pool: Any, *, block_number: BlockIdentifier | None = None) -> bool:
        builder = self._builder_for_pool(pool)
        return builder.update(pool, block_number=block_number)

    def _builder_for_pool(self, pool: Any) -> PoolBuilder:
        if isinstance(pool, UniswapV4Pool):
            return self._v4_builder
        if isinstance(pool, UniswapV3Pool):
            return self._v3_builder
        if isinstance(pool, CurveStableswapPool):
            return self._curve_builder
        if isinstance(pool, (UniswapV2Pool, AerodromeV2Pool)):
            return self._v2_builder
        raise TypeError(f"update() not implemented for pool type {type(pool).__name__}")
```

Note: `Bot.update()` still has an isinstance dispatch, but it's now a 4-branch delegation to the same interface on each builder, not a 5-branch dispatch into 5 different private-method implementations. This is acceptable because the builders need to be selected, and the pools don't carry a reference to their builder. If candidate #4 is also implemented, this dispatch moves onto the pool itself.

### AsyncBot

`AsyncBot` (878 lines) duplicates the same `build_*` methods with `async/await`. The same extraction applies:

```
src/degenbot/builders/
├── async_v2_pool_builder.py
├── async_v3_pool_builder.py
├── async_v4_pool_builder.py
├── async_curve_pool_builder.py
├── async_erc20_builder.py
```

Each async builder takes `AsyncConnectionManager` instead of `ConnectionManager`. The sync and async builders share **zero code** — they have different I/O primitives — but they share the same **interface shape** (like `PoolBuilder` vs `AsyncPoolBuilder`).

## Implementation steps

### Phase 1: Create builder modules and move code (mechanical)

1. Create `src/degenbot/builders/` with `__init__.py` and `base.py`.
2. Create `erc20_builder.py`:
   - Move `Bot.build_erc20token()` body into `Erc20Builder.build()`.
   - Move `Bot.get_token_balance()`, `get_token_approval()`, `get_token_total_supply()`, `get_ether_balance()` to the builder (these are token I/O operations).
   - Update `Erc20Builder.__init__` to accept `connections`, `db`, `tokens`.
3. Create `v2_pool_builder.py`:
   - Move `Bot.build_v2_pool()` body into `V2PoolBuilder.build()`.
   - Move `Bot._update_v2_pool()` and `Bot._update_aerodrome_v2_pool()` into `V2PoolBuilder.update()`.
   - Move `_make_tick_data_fetcher_v3` stays on V3 builder (see step 4).
4. Create `v3_pool_builder.py`:
   - Move `Bot.build_v3_pool()` body into `V3PoolBuilder.build()`.
   - Move `Bot._make_tick_data_fetcher_v3()` into the V3 builder.
   - Move `Bot._update_v3_pool()` into `V3PoolBuilder.update()`.
5. Create `v4_pool_builder.py`:
   - Move `Bot.build_v4_pool()` body into `V4PoolBuilder.build()`.
   - Move `Bot._make_tick_data_fetcher_v4()` into the V4 builder.
   - Move `Bot._update_v4_pool()` into `V4PoolBuilder.update()`.
6. Create `curve_pool_builder.py`:
   - Move `Bot.build_curve_pool()` body into `CurvePoolBuilder.build()`.
   - Move all 12 `Bot._make_curve_*` methods into the Curve builder.
   - Move `Bot._update_curve_pool()` into `CurvePoolBuilder.update()`.

### Phase 2: Wire builders into Bot

7. Add builder construction to `Bot.__init__` (pass I/O dependencies).
8. Convert all `build_*` methods on `Bot` to thin delegators.
9. Convert `Bot.update()` to `_builder_for_pool()` + delegation.
10. Remove the old private implementation methods from `Bot`.

### Phase 3: Update callers

11. Update pool managers (`uniswap/managers.py`, `aerodrome/managers.py`, `curve/managers.py`):
    - They currently call `self._bot.build_v2_pool()` etc. — no change needed since Bot's public interface is preserved via delegation.
    - Optional: managers could hold a direct reference to their builder instead of going through Bot.
12. Update `AsyncBot` the same way (create `AsyncPoolBuilder` subclasses and wire them in).

### Phase 4: Tests

13. Existing tests that call `bot.build_v2_pool()` etc. continue to work unchanged (Bot's public interface is preserved).
14. Add new unit tests for each builder:
    - `tests/builders/test_v2_pool_builder.py` — construct a builder with faked I/O, test build logic.
    - `tests/builders/test_v3_pool_builder.py` — same.
    - `tests/builders/test_v4_pool_builder.py` — same.
    - `tests/builders/test_curve_pool_builder.py` — same.
    - `tests/builders/test_erc20_builder.py` — same.
15. Run `just test-all` to verify no regressions.

### Phase 5: Cleanup

16. Remove `_resolve_block_number` from Bot if no other callers remain.
17. Remove `_make_tick_data_fetcher_v3` / `_make_tick_data_fetcher_v4` from Bot.
18. Remove all `_make_curve_*` methods from Bot.
19. Remove all `_update_*` methods from Bot.

## Metrics

| Metric | Before | After |
|--------|--------|-------|
| `bot.py` lines | ~2285 | ~600 (session + delegation) |
| Max method length in Bot | ~500 lines (`build_curve_pool`) | ~10 lines (delegators) |
| I/O code in "session" class | ~1100 lines | 0 lines |
| Number of modules to understand V3 construction | 4 (bot, v3_pool, v3_types, database) | 2 (v3_builder, v3_pool) |

## Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| Breakage of `bot.build_*()` call sites | Bot preserves its public interface via delegation — all callers work unchanged |
| Circular imports (builders import pool types, pool managers import Bot) | Builders don't import Bot; they receive I/O dependencies at construction. Pool managers call `bot.build_*()` which delegates — no new circular |
| AsyncBuilder duplication | Sync and async builders are separate by design (different I/O primitives). The duplication is inherent, not accidental |
| Test coverage gap during migration | Phase 1-3 is mechanical move-and-delegate; existing tests cover the same paths. Phase 4 adds new direct-builder tests |

## Dependencies on other plans

- **Plan 002** (PoolClassRegistry): Can be done independently, but builders are the natural consumer of the registry — builder delegates class selection to registry instead of embedding a dict.
- **Plan 003** (Unified tick fetcher): The V3/V4 builders would be the natural home for the unified fetcher factory.
- **Plan 004** (Update dispatch): Builders already absorb `update()` — this plan subsumes much of #4. If both are implemented, `builder.update()` is the seam, and the `_builder_for_pool()` dispatch on Bot is a 4-branch type-to-builder map, not a 5-branch isinstance chain into separate methods.
- **Plan 005** (Curve fetcher factory): Fully subsumed by this plan — the Curve builder absorbs all 12 fetcher factories.
