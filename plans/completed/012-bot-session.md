# Plan 012: `Bot` Session — replace module-level singletons with I/O-free domain objects and explicit session ownership

> **Note**: References to `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, `build_curve_pool`, and `get_web3` throughout this plan are historical — these methods were removed by Plan 059. Use `build_pool()` and `get_provider()` instead.
>
> **Status**: Phase 1 ✅ Phase 2 ✅ Phase 3 ✅ Phase 4 ✅ Phase 5 ✅ Phase 6 ✅ Phase 7 ✅ Phase 8 ✅ Phase 9 ✅ Phase 10 ✅ Phase 11 ✅ Phase 12 ✅. All module-level singletons removed. `bot.update()` supports V2, V3, V4, AerodromeV2, Curve. All integration tests updated. `from_exchange()`, `auto_update()`, `_LazyConfig` removed. 1805 passed, 0 failures.
> See [Implementation notes](#implementation-notes) for details on what was built and what was learned.

## Background

### The problem: hidden global state

Degenbot's core classes reach into four module-level singletons at construction time:

| Singleton | Module | Used by |
|-----------|--------|---------|
| `config` | `degenbot.config` | `database/__init__`, `database/operations`, `migrations/env`, `v3_snapshot`, `v4_snapshot`, CLI |
| `db_session` | `degenbot.database` | `erc20/erc20`, `v2/v3/v4_liquidity_pool`, `v3/v4_snapshot`, `pathfinding`, CLI |
| `connection_manager` | `degenbot.connection` | `v2/v3/v4_liquidity_pool`, `v3/v4_snapshot`, `aerodrome/pools`, `aerodrome/managers`, `curve/curve_stableswap`, `chainlink`, `erc20/erc20`, `erc20/ether_placeholder`, `erc20/manager`, `camelot/pools`, `balancer/pools` |
| `pool_registry` / `token_registry` / `managed_pool_registry` | `degenbot.registry` | `v2/v3/v4_liquidity_pool`, `aerodrome/pools`, `aerodrome/managers`, `curve/curve_stableswap`, `erc20/erc20`, `erc20/manager` |

Every `UniswapV3Pool("0x...")` call implicitly:
1. Fetches a `ProviderAdapter` from `connection_manager`
2. Makes RPC calls to fetch pool state
3. Opens a `db_session` to check for cached pool/token data
4. Creates `Erc20Token` instances (which also make RPC calls and access the DB)
5. Registers itself in `pool_registry`
6. Registers its tokens in `token_registry`

These are not optional conveniences — the database is core infrastructure. `Erc20Token.__init__`,
`UniswapV2Pool.__init__`, and `UniswapV3Pool.__init__` all query the database to skip redundant
RPC calls. The database provides persistence across sessions (token metadata, pool parameters,
Aave market state, Uniswap liquidity maps, pathfinding data).

### Why a lazy config proxy was needed

`database/__init__.py` runs at import time:

```python
db_session = DatabaseSessionManager(get_scoped_sqlite_session(database_path=config.database.path))
```

Without the lazy `_LazyConfig` proxy, importing any module that transitively imports
`degenbot.database` (i.e., most of the library) triggers config file reading, directory creation,
and database initialization. The proxy defers this until first attribute access.

### The user's workflow

Users of degenbot:
1. Create a set of pool helpers (via managers or directly)
2. Create arbitrage paths/cycles from those pools
3. Subscribe to events for those pools
4. Apply state changes and recalculate affected paths

This is a **session** — a living context for a complete run. The pools, tokens, connections, and
database all belong to that session.

## Goal

Replace the four module-level singletons with an explicit `Bot` class that owns the session's
runtime state. In the process, make domain objects (pools, tokens) **I/O-free** — they become pure
data + computation with no `provider`, no `db`, no async methods, and no self-registration.

The `Bot` is:

- **Factory** — constructs pools/tokens from pre-fetched data, wiring in the right provider and
  db_session to do the fetching
- **Registry** — tracks what it's created (absorbs `pool_registry`, `token_registry`,
  `managed_pool_registry`); registration is the exclusive responsibility of the manager/Bot,
  not the domain object itself
- **I/O boundary** — all RPC calls and database access flow through `Bot`; domain objects are
  trivially testable with pure data
- **Session** — the lifetime scope for the entire run; multiple `Bot` instances are supported,
  each with independent state

Pool and token constructors receive **pre-fetched data only** — addresses, names, decimals,
reserves, tick maps, etc. No provider, no database, no registry.

A separate `AsyncBot` class mirrors `Bot` with async I/O methods, returning the same I/O-free
domain objects.

## Design

### The `Bot` class

```python
class Bot:
    def __init__(self, config: DegenbotConfig) -> None:
        self.config = config
        self.connections = ConnectionManager()
        self.db = DatabaseSessionManager(
            get_scoped_sqlite_session(database_path=config.database.path)
        )
        self.pools = PoolRegistry()
        self.tokens = TokenRegistry()
        self.managed_pools = ManagedPoolRegistry()
        self._managers: dict[tuple[ChainId, ChecksumAddress], AbstractPoolManager] = {}
        self._token_manager = Erc20TokenManager(bot=self)
        self._check_database_version()

    @classmethod
    def from_config_file(cls) -> Bot:
        return cls(config=_init_config())

    def add_manager[M: AbstractPoolManager](
        self,
        manager_cls: type[M],
        *,
        factory_address: str,
        chain_id: ChainId | None = None,
        **kwargs: Any,
    ) -> M:
        """Create a pool manager within this bot's session."""
        chain_id = chain_id or self.connections.default_chain_id
        manager = manager_cls(
            factory_address=factory_address,
            chain_id=chain_id,
            bot=self,
            **kwargs,
        )
        self._managers[chain_id, get_checksum_address(factory_address)] = manager
        return manager

    def get_token(self, address: str, *, chain_id: ChainId | None = None) -> Erc20Token:
        """Get or create a token. Bot handles DB lookup, RPC calls, and registration."""
        return self._token_manager.get_erc20token(address, chain_id=chain_id)

    def get_provider(self, *, chain_id: ChainId) -> ProviderAdapter:
        return self.connections.get_provider(chain_id)

    def get_web3(self, *, chain_id: ChainId) -> Web3:
        return self.connections.get_web3(chain_id)

    def _check_database_version(self) -> None:
        with self.db() as session:
            current = MigrationContext.configure(
                connection=self.db.connection()
            ).get_current_revision()
        latest = ScriptDirectory.from_config(
            config=get_alembic_config(database_path=self.config.database.path)
        ).get_current_head()
        if current is not None and current != latest:
            logger.warning(...)

    # I/O methods that operate on I/O-free domain objects
    def get_token_balance(
        self,
        token: Erc20Token,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int: ...
    def get_token_approval(
        self,
        token: Erc20Token,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int: ...
    def get_token_total_supply(
        self,
        token: Erc20Token,
        block_identifier: BlockIdentifier | None = None,
    ) -> int: ...
    def get_pool_reserves(
        self,
        pool: UniswapV2Pool,
        block_identifier: BlockIdentifier | None = None,
    ) -> tuple[int, int]: ...
```

### The `AsyncBot` class

`AsyncBot` mirrors `Bot` with `async_connections: AsyncConnectionManager` and `async def` factory
and I/O methods. It returns the same I/O-free domain objects — they don't care who constructed them.

```python
class AsyncBot:
    def __init__(self, config: DegenbotConfig) -> None:
        self.config = config
        self.connections = AsyncConnectionManager()
        self.db = DatabaseSessionManager(...)
        self.pools = PoolRegistry()
        self.tokens = TokenRegistry()
        self.managed_pools = ManagedPoolRegistry()
        ...

    async def get_token(self, address: str, *, chain_id: ChainId | None = None) -> Erc20Token: ...
    async def get_token_balance(self, token: Erc20Token, ...) -> int: ...
    async def get_token_approval(self, token: Erc20Token, ...) -> int: ...
    ...
```

### Domain objects become I/O-free

**Before (current):**
```python
class Erc20Token:
    def __init__(self, address, *, chain_id=None, provider=None, silent=False):
        self._provider = provider or connection_manager.get_provider(chain_id)
        token_from_db = get_token_from_database(self.address, self.chain_id)  # DB access
        if token_from_db is not None:
            self.name = token_from_db.name  # from DB
        else:
            self.name, self.symbol, self.decimals = self.get_name_symbol_decimals_batched(
                provider
            )  # RPC
        token_registry.add(...)  # self-registration
```

**After:**
```python
class Erc20Token:
    """Pure data + computation. No I/O."""

    def __init__(
        self,
        address: ChecksumAddress,
        *,
        name: str,
        symbol: str,
        decimals: int,
        chain_id: ChainId,
        state_cache_depth: int = 8,
    ):
        self.address = address
        self.name = name
        self.symbol = symbol
        self.decimals = decimals
        self._chain_id = chain_id
        self._state_cache_depth = state_cache_depth
        self._cached_balance: dict[ChecksumAddress, BoundedCache[BlockNumber, int]] = {}
        self._cached_approval: dict[tuple[int, ChecksumAddress, ChecksumAddress], int] = {}
        self._cached_total_supply: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth,
        )

    @property
    def chain_id(self) -> int:
        return self._chain_id

    # Cache accessors — not I/O, just dictionary lookups
    def get_cached_balance(self, address: ChecksumAddress, block_number: int) -> int | None:
        return self._cached_balance.get(address, {}).get(block_number)

    def set_cached_balance(self, address: ChecksumAddress, block_number: int, balance: int) -> None:
        if address not in self._cached_balance:
            self._cached_balance[address] = BoundedCache(max_items=self._state_cache_depth)
        self._cached_balance[address][block_number] = balance

    def get_cached_approval(
        self, block_number: int, owner: ChecksumAddress, spender: ChecksumAddress
    ) -> int | None:
        return self._cached_approval.get((block_number, owner, spender))

    def set_cached_approval(
        self, block_number: int, owner: ChecksumAddress, spender: ChecksumAddress, amount: int
    ) -> None:
        self._cached_approval[block_number, owner, spender] = amount

    def get_cached_total_supply(self, block_number: int) -> int | None:
        return self._cached_total_supply.get(block_number)

    def set_cached_total_supply(self, block_number: int, total_supply: int) -> None:
        self._cached_total_supply[block_number] = total_supply

    # Pure computation remains on the domain object
    @property
    def price(self) -> float: ...
```

**I/O moves to Bot:**
```python
class Bot:
    def get_token_balance(self, token: Erc20Token, address: str, block_identifier=...) -> int:
        address = get_checksum_address(address)
        block_number = self._resolve_block(token.chain_id, block_identifier)
        if (balance := token.get_cached_balance(address, block_number)) is not None:
            return balance
        balance = self._fetch_erc20_balance(token.address, address, block_number)
        token.set_cached_balance(address, block_number, balance)
        return balance

    def _fetch_erc20_balance(self, token_address, holder_address, block_number) -> int:
        provider = self.connections.get_provider(...)
        (balance,) = eth_abi.abi.decode(...)
        return balance
```

### Manager as thin ownership layer

Managers no longer do I/O themselves. `Bot` does the I/O and hands the result to the manager:

```python
class UniswapV2PoolManager(AbstractPoolManager[UniswapV2Pool]):
    def __init__(self, factory_address, *, chain_id, bot: Bot, ...):
        self._bot = bot
        ...

    def get_pool(self, pool_address, *, silent=False, ...) -> UniswapV2Pool:
        pool_address = get_checksum_address(pool_address)

        # Check local cache
        with contextlib.suppress(KeyError):
            return self._tracked_pools[pool_address]

        # Check registry
        if (pool := self._bot.pools.get(pool_address=pool_address, chain_id=self.chain_id)):
            if pool.factory == self._factory_address:
                self._add_tracked_pool(pool)
                return pool
            raise PoolNotAssociated(pool_address)

        # Build the pool — Bot does all I/O
        pool = self._bot.build_v2_pool(
            pool_address=pool_address,
            factory_address=self._factory_address,
            deployer_address=self._deployer_address,
            init_hash=self._pool_init_hash,
            silent=silent,
        )

        # Register (no longer self-registration)
        self._bot.pools.add(pool=pool, pool_address=pool_address, chain_id=self.chain_id)
        self._add_tracked_pool(pool)
        return pool
```

`Bot.build_v2_pool()` handles the current `UniswapV2Pool.__init__` logic — DB lookup, RPC calls
for immutable values, token construction, RPC calls for reserves — then passes the pre-fetched data
to the I/O-free constructor:

```python
class Bot:
    def build_v2_pool(self, pool_address, factory_address, deployer_address, init_hash, silent, ...) -> UniswapV2Pool:
        # Try DB first
        with self.db() as session:
            pool_from_db = self._get_pool_from_db(session, pool_address, self.chain_id)

        provider = self.connections.get_provider(self.chain_id)

        if pool_from_db is not None:
            token0 = self.get_token(pool_from_db.token0.address)
            token1 = self.get_token(pool_from_db.token1.address)
            factory = pool_from_db.exchange.factory
            fee_token0 = Fraction(pool_from_db.fee_token0, pool_from_db.fee_denominator)
            fee_token1 = Fraction(pool_from_db.fee_token1, pool_from_db.fee_denominator)
        else:
            factory, (token0_addr, token1_addr), fee_token0, fee_token1 = (
                self._fetch_v2_immutable_values(provider, pool_address)
            )
            token0 = self.get_token(token0_addr)
            token1 = self.get_token(token1_addr)

        reserves0, reserves1, state_block = self._fetch_v2_reserves(provider, pool_address)

        return UniswapV2Pool(
            address=pool_address,
            chain_id=self.chain_id,
            token0=token0,
            token1=token1,
            factory=factory,
            deployer_address=deployer_address,
            init_hash=init_hash,
            fee_token0=fee_token0,
            fee_token1=fee_token1,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
            state_block=state_block,
        )
```

### Manager singleton enforcement: per-Bot

The current `AbstractPoolManager.instances` is a class-level `WeakValueDictionary`, which would
break multi-Bot usage (two Bots can't create managers for the same factory). This enforcement moves
to `Bot._managers`:

```python
class Bot:
    def add_manager(self, manager_cls, *, factory_address, chain_id, **kwargs):
        key = (chain_id, get_checksum_address(factory_address))
        if key in self._managers:
            raise ManagerAlreadyInitialized(...)
        manager = manager_cls(factory_address, chain_id=chain_id, bot=self, **kwargs)
        self._managers[key] = manager
        return manager
```

`AbstractPoolManager.instances` and `get_instance()` are removed.

### User-facing API

**Library user:**

```python
from degenbot import Bot

bot = Bot.from_config_file()

# Register RPC endpoints (replaces set_web3)
bot.connections.register_provider(ProviderAdapter.from_web3(w3))

# Create managers — the bot wires in the session
uniswap_v2 = bot.add_manager(UniswapV2PoolManager, factory_address="0x5C69bEe701ef...")
uniswap_v3 = bot.add_manager(UniswapV3PoolManager, factory_address="0x1F98431c8aD985...")

# Pools come from managers (Bot does all I/O under the hood)
pool = uniswap_v3.get_pool("0x8ad599c3a0ff1de082011efddc58f1908eb6e6d8")

# Tokens come from the bot
weth = bot.get_token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

# I/O methods live on Bot
balance = bot.get_token_balance(weth, "0x...")
approval = bot.get_token_approval(weth, owner="0x...", spender="0x...")

# Pure computation stays on the domain object
result = pool.simulate_swap(token_in=weth.address, amount_in=1000, token_out=usdc.address)

# Arbitrage cycles work the same — they only use pool state, no I/O
cycle = UniswapLpCycle(pools=[pool1, pool2, pool3])
result = cycle.calculate(...)
```

**Async library user:**

```python
from degenbot import AsyncBot

bot = AsyncBot.from_config_file()

await bot.connections.register_provider(...)
uniswap_v3 = await bot.add_manager(UniswapV3PoolManager, factory_address="0x...")
pool = await uniswap_v3.get_pool("0x...")
weth = await bot.get_token("0x...")
balance = await bot.get_token_balance(weth, "0x...")
```

**CLI user:**

```python
@click.group()
@click.pass_context
def cli(ctx: click.Context) -> None:
    ctx.obj = Bot.from_config_file()


@cli.command()
@click.pass_obj
def database_reset(bot: Bot) -> None:
    bot.config.database.path.unlink(missing_ok=True)
    create_new_sqlite_database(bot.config.database.path)
```

### Constructor pattern: I/O-free data classes

Pool and token constructors accept only data, no I/O dependencies:

```python
class UniswapV3Pool:
    def __init__(
        self,
        address: str,
        *,
        chain_id: ChainId,
        token0: Erc20Token,
        token1: Erc20Token,
        factory: str,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        fee: int = ...,
        tick_spacing: int = ...,
        sqrt_price_x96: int = ...,
        tick: int = ...,
        liquidity: int = ...,
        tick_bitmap: dict[int, UniswapV3BitmapAtWord] | None = None,
        tick_data: dict[int, UniswapV3LiquidityAtTick] | None = None,
        state_block: BlockNumber = ...,
        state_cache_depth: int = 8,
    ) -> None:
        # Pure data assignment. No RPC calls, no DB access, no registry.
        self.address = get_checksum_address(address)
        self._chain_id = chain_id
        self._token0 = token0
        self._token1 = token1
        ...
```

Users never see this signature — they go through `manager.get_pool()` which delegates to
`Bot.build_*_pool()`.

## What gets removed from domain objects

For every pool and token class, the following are extracted:

| What | Where it moves |
|------|----------------|
| `provider` / `_provider` / `_provider_from_connection_manager` | `Bot.connections` |
| `connection_manager` imports | `Bot` |
| `db_session` / `db_session()` context manager calls | `Bot.db` |
| `self._fetch_*()` / `self.get_immutable_pool_values()` / `self.get_reserves()` | `Bot._fetch_*()` |
| `get_token_from_database()` / `get_pool_from_database()` module functions | `Bot._get_*_from_db()` |
| `token_registry.add()` / `pool_registry.add()` | `Bot` / manager after construction |
| `Erc20TokenManager` construction for token lookup | `Bot.get_token()` |
| `provider.call(...)` / `raw_call(...)` | `Bot` |
| Cache I/O accessors (`get_balance()`, `get_approval()`, etc.) that make RPC calls | `Bot.get_token_balance()`, `Bot.get_token_approval()`, etc. |
| Async methods (`get_approval_async`, `get_balance_async`, etc.) | `AsyncBot.get_token_balance()`, etc. |
| `ChainlinkPriceContract` construction inside `Erc20Token.__init__` | `Bot` sets `token._price_oracle` after construction, or price fetching lives on `Bot` |
| `_get_provider_for_chain()` dynamic provider resolution | `Bot` always has the right provider |
| `_pickle_drops` / `_pickle_reconstructs` for `_provider` / `_provider_from_connection_manager` | Simplified — no provider on domain object |

## What stays on domain objects

- All pure computation: `simulate_swap()`, `calculate(...)`, state transitions, tick math
- All state: caches (balances, approvals, total supply), tick maps, reserves, liquidity
- Cache accessor methods: `get_cached_balance()`, `set_cached_balance()`, etc. (dictionary ops, no I/O)
- Publisher/subscriber pattern (`PublisherMixin`)
- Pickle support (simplified — no provider/DB to drop/reconstruct)

## Affected files

### New files

| File | Contents |
|------|----------|
| `src/degenbot/bot.py` | `Bot` class |
| `src/degenbot/async_bot.py` | `AsyncBot` class |

### Modified files — domain objects (I/O extraction)

| File | Changes |
|------|---------|
| `src/degenbot/erc20/erc20.py` | Constructor takes data only; remove `provider`, `db_session`, `connection_manager`, `token_registry`, `ChainlinkPriceContract` construction; remove `get_balance()`, `get_approval()`, `get_total_supply()`, all async methods; add cache accessors |
| `src/degenbot/erc20/ether_placeholder.py` | Same pattern as `Erc20Token`; remove `connection_manager`, `get_balance()`; `Bot` handles native ETH balance via separate method |
| `src/degenbot/uniswap/v2_liquidity_pool.py` | Constructor takes data only; remove `provider`, `db_session`, `connection_manager`, `pool_registry`, `Erc20TokenManager`, `get_immutable_pool_values()`, `get_reserves()`, `get_pool_from_database()`, `from_exchange()`; remove `_provider`, `_provider_from_connection_manager`, `_get_provider_for_chain()` |
| `src/degenbot/uniswap/v3_liquidity_pool.py` | Same; remove `provider`, `db_session`, `connection_manager`, `pool_registry`, `Erc20TokenManager`, `get_immutable_pool_values()`, `get_mutable_pool_values()`, `_fetch_and_populate_initialized_ticks()`, `get_pool_from_database()` |
| `src/degenbot/uniswap/v4_liquidity_pool.py` | Same pattern; remove `provider`, `db_session`, `connection_manager`, `managed_pool_registry`, `Erc20TokenManager`, `_get_state_values()`, `get_pool_from_database()` |
| `src/degenbot/aerodrome/pools.py` | Remove `connection_manager`, `pool_registry`, `Erc20TokenManager`, RPC calls; accept pre-fetched data |
| `src/degenbot/curve/curve_stableswap_liquidity_pool.py` | Remove `connection_manager`, `pool_registry`, `Erc20TokenManager`, RPC calls; accept pre-fetched data |
| `src/degenbot/camelot/pools.py` | Remove `connection_manager`, RPC calls; accept pre-fetched data |
| `src/degenbot/balancer/pools.py` | Remove `connection_manager`, `Erc20TokenManager`, RPC calls; accept pre-fetched data |

### Modified files — managers

| File | Changes |
|------|---------|
| `src/degenbot/types/abstract/pool_manager.py` | Remove `instances` class variable and `get_instance()`; add `bot` parameter; remove singleton enforcement (now in `Bot`) |
| `src/degenbot/uniswap/managers.py` | Accept `bot: Bot`; delegate pool construction to `bot.build_*_pool()`; handle registration after construction; remove `connection_manager`, `pool_registry` imports |
| `src/degenbot/aerodrome/managers.py` | Same pattern; accept `bot: Bot` |
| `src/degenbot/erc20/manager.py` | Accept `bot: Bot`; delegate token construction to `bot.build_erc20_token()`; remove `connection_manager`, `token_registry` imports |
| `src/degenbot/pancakeswap/managers.py` | Accept `bot: Bot` via parent; overrides of `_build_pool` delegate to `bot.build_*_pool()` |
| `src/degenbot/sushiswap/managers.py` | Accept `bot: Bot` via parent |
| `src/degenbot/swapbased/managers.py` | Accept `bot: Bot` via parent |

### Modified files — infrastructure

| File | Changes |
|------|---------|
| `src/degenbot/config.py` | Remove `_LazyConfig`, `TYPE_CHECKING` guard; keep `DegenbotConfig` and `_init_config` |
| `src/degenbot/__init__.py` | Add `Bot`, `AsyncBot` to re-exports; deprecate `config`, `connection_manager`, `set_web3`, etc. |
| `src/degenbot/connection/__init__.py` | Keep module-level `connection_manager` with deprecation warning |
| `src/degenbot/database/__init__.py` | Remove module-level `db_session` and version checks; move to `Bot` |
| `src/degenbot/database/operations.py` | Remove `config` import; `get_alembic_config` takes `database_path` param |
| `src/degenbot/chainlink.py` | Accept `provider` or `bot` explicitly; remove `connection_manager` |

### Modified files — snapshots & pathfinding

| File | Changes |
|------|---------|
| `src/degenbot/uniswap/v3_snapshot.py` | `DatabaseSnapshot` accepts `db` and `database_path` explicitly; `fetch_new_events` accepts `provider`; remove `config`, `db_session`, `connection_manager` |
| `src/degenbot/uniswap/v4_snapshot.py` | Same pattern |
| `src/degenbot/pathfinding.py` | Accept `db: DatabaseSessionManager` explicitly; remove `db_session` import. Can also become `Bot.find_paths()` |

### Modified files — CLI

| File | Changes |
|------|---------|
| `src/degenbot/cli/__init__.py` | Create `Bot` at Click entry point, pass through context |
| `src/degenbot/cli/database.py` | Receive `Bot` from Click context instead of importing `config` and `db_session` |
| `src/degenbot/cli/utils.py` | `get_provider_from_config` takes `Bot` instead of reading `config` |
| `src/degenbot/cli/pool.py` | Receive `Bot` from Click context |
| `src/degenbot/cli/aave/commands.py` | Receive `Bot` from Click context |
| `src/degenbot/cli/exchange.py` | Receive `Bot` from Click context |

### Modified files — arbitrage

| File | Changes |
|------|---------|
| `src/degenbot/arbitrage/uniswap_curve_cycle.py` | Remove `connection_manager` import; accept `bot: Bot` or `provider` for block number resolution |

### Modified files — migrations

| File | Changes |
|------|---------|
| `src/degenbot/migrations/env.py` | Use `_init_config()` directly instead of going through `Bot` (Alembic runs standalone) |

### Removed

| What | From |
|------|------|
| `_LazyConfig` class | `config.py` |
| `TYPE_CHECKING` guard on `config` | `config.py` |
| Module-level `db_session` | `database/__init__.py` |
| Module-level version-check block | `database/__init__.py` |
| `AbstractPoolManager.instances` (WeakValueDictionary) | `types/abstract/pool_manager.py` |
| `AbstractPoolManager.get_instance()` | `types/abstract/pool_manager.py` |
| `get_token_from_database()` module function | `erc20/erc20.py` |
| `get_pool_from_database()` module function | `uniswap/v2_liquidity_pool.py`, `v3_liquidity_pool.py`, `v4_liquidity_pool.py` |
| `_provider`, `_provider_from_connection_manager`, `_get_provider_for_chain()` | All pool classes |

## Step-by-step

### Phase 1: Create `Bot` class and `AsyncBot` skeleton ✅

**Status**: Complete.

**What was built**:
- `src/degenbot/bot.py` — `Bot` class with `from_config_file()`, `add_manager()`,
  `build_erc20token()`, `get_token()`, `get_token_balance()`, `get_token_approval()`,
  `get_token_total_supply()`, `get_ether_balance()`, `get_provider()`, `get_web3()`
- `src/degenbot/async_bot.py` — `AsyncBot` skeleton with `__init__`, `from_config_file()`
- `src/degenbot/types/abstract/pool_manager.py` — Removed `instances` WeakValueDictionary and
  `get_instance()`, added `bot: Bot | None` type annotation
- `src/degenbot/uniswap/managers.py` — Added `bot: Bot | None = None` to both
  `AbstractUniswapV2PoolManager.__init__` and `AbstractUniswapV3PoolManager.__init__`, with
  `chain_id` resolution from `bot.connections.default_chain_id` when bot is available
- `src/degenbot/aerodrome/managers.py` — Same pattern: added `bot` param
- `src/degenbot/__init__.py` — Added `Bot` and `AsyncBot` to imports and `__all__`
- `tests/conftest.py` — Removed `AbstractPoolManager.instances.clear()` from autouse fixture
- `tests/test_bot.py` — 16 tests covering Bot construction, `add_manager`, multi-Bot independence,
  AsyncBot construction

**Key decisions**:
- Per-Bot manager enforcement via `Bot._managers` dict (replaces class-level WeakValueDictionary)
- `instances` singleton enforcement removed from `AbstractPoolManager` — this was a prerequisite
  for multi-Bot support
- `bot` parameter uses `TYPE_CHECKING` guard in managers to avoid circular imports at runtime
  (managers import `Bot` only under `TYPE_CHECKING`, use `from __future__ import annotations`)

### Phase 2: Extract I/O from `Erc20Token` + `EtherPlaceholder` ✅

**Status**: Complete (legacy backward-compat shim retained for pool migration).

**What was built**:

- **Dual-mode `Erc20Token.__init__`**: When `name`, `symbol`, `decimals` are provided, the
  I/O-free path is taken (no provider, DB, or registry access). When they're not provided, the
  legacy I/O path runs with a `DeprecationWarning`. The I/O-free path sets `_price_oracle = None`
  and does not self-register in `token_registry`.

- **Cache accessors**: `get_cached_balance()`, `set_cached_balance()`, `get_cached_approval()`,
  `set_cached_approval()`, `get_cached_total_supply()`, `set_cached_total_supply()`. These are
  pure dictionary operations with no I/O.

- **Static fetch methods**: `Erc20Token.fetch_name_symbol_decimals_batched()`, `fetch_name()`,
  `fetch_symbol()`, `fetch_decimals()`. These take `address` and `provider` as arguments
  (no `self` usage) and can be called from `Bot.build_erc20token()`.

- **`Bot.build_erc20token()`**: Full I/O flow — checks `self.tokens` registry, checks for
  `EtherPlaceholder`, queries DB via `get_token_from_database()`, fetches from chain on miss,
  writes back to DB if the DB record was missing data, constructs I/O-free `Erc20Token`,
  registers in `self.tokens`.

- **`Bot.get_token_balance/approval/total_supply()`**: Cache-first I/O methods that use the
  accessors on `Erc20Token` and fall through to RPC via `self.connections.get_provider()` when
  cache misses.

- **`Bot.get_ether_balance()`**: Native ETH balance via `provider.get_balance()`.

- **`Erc20TokenManager`**: Accepts `bot: Bot | None`. When `bot` is provided, delegates to
  `bot.build_erc20token()`. When not provided, checks `token_registry` for existing tokens before
  constructing (legacy path retained with deprecation warning).

- **`EtherPlaceholder`**: Updated to call `super().__init__()` with I/O-free params
  (`name`/`symbol`/`decimals`), then self-registers in `token_registry` (legacy behavior for
  pools not yet migrated).

- **Deprecation warnings** on legacy `Erc20Token` I/O constructor path and on I/O methods
  (`get_balance()`, `get_approval()`, `get_total_supply()` + async variants).

- `tests/test_erc20_io_free.py` — 14 tests covering I/O-free constructor, cache accessors,
  `Bot.build_erc20token()`, multi-Bot token independence, `EtherPlaceholder`.

**Key lessons learned**:

1. **Self-registration causes double-registration errors within a single test**: When the
   `test_create_pool` test creates two `UniswapV2Pool` instances with the same tokens, the
   second `Erc20TokenManager` has no cache of the first manager's tokens, and the legacy
   `Erc20Token.__init__` self-registers in `token_registry` which already has the token. The fix
   was to have `Erc20TokenManager` check `token_registry` before constructing in the legacy
   path. This problem disappears entirely once pools are I/O-free (no self-registration).

2. **`get_token_from_database()` must stay in `erc20.py` during transition**: The function
   requires `Erc20TokenTable` which is imported via `degenbot.database.models`. Moving it to
   `Bot` would create a circular import (`bot` → `erc20` → `bot`). The function now accepts
   `session` as an explicit parameter instead of defaulting to the module-level `db_session`.

3. **Legacy I/O methods must remain on `Erc20Token` until all callers are migrated**: Curve's
   `curve_stableswap_liquidity_pool.py` calls `token.get_balance()` and `token.get_total_supply()`
   directly. `arbitrage/uniswap_curve_cycle.py` calls `token.get_approval()`. These modules
   won't be migrated until Phase 6+. Until then, the legacy methods stay (with deprecation
   docstrings).

4. **`EtherPlaceholder` self-registration is needed during transition**: Pools construct
   `EtherPlaceholder` via `Erc20TokenManager` which calls the legacy path. The
   `EtherPlaceholder.__init__` must still self-register so that subsequent token lookups find
   the placeholder in `token_registry`. Once pools are I/O-free, this registration moves
   exclusively to `Bot.build_erc20token()`.

**What remains for Phase 2 (deferred to later phases)**:
- Remove legacy I/O `__init__` path once all pools are I/O-free (Phase 3+)
- Remove legacy I/O methods (`get_balance()`, etc.) once Curve/arbitrage are migrated (Phase 6+)
- Remove `price` / `ChainlinkPriceContract` from `Erc20Token` (no internal callers)
- Remove `provider` / `async_provider` / `async_w3` properties (blocked on legacy method removal)
- Remove `db_session` / `connection_manager` / `token_registry` imports from `erc20.py`

### Phase 3: Extract I/O from `UniswapV2Pool` + `UniswapV2PoolManager` ✅

**Status**: Complete (legacy backward-compat shim retained).

**What was built**:

- **Dual-mode `UniswapV2Pool.__init__`**: When `token0`, `token1`, `factory`, `fee_token0`,
  `fee_token1`, `reserves_token0`, `reserves_token1` are all provided, the I/O-free path is
  taken (no provider, DB, or registry access). When they're not provided, the legacy I/O path
  runs with a `DeprecationWarning`. The I/O-free path does not set `_provider`,
  `_provider_from_connection_manager`, and does not self-register in `pool_registry`.

- **`Bot.build_v2_pool()`**: Full I/O flow —
  1. Checks DB for pool data (gracefully handles missing DB tables)
  2. If not in DB: fetches factory, token0, token1 addresses via `provider.call()`
  3. Builds tokens via `bot.build_erc20token()`
  4. Fetches reserves via `raw_call()`
  5. Derives deployer/init_hash from `FACTORY_DEPLOYMENTS`
  6. Constructs I/O-free `UniswapV2Pool`
  7. Registers in `bot.pools`

- **`UniswapV2PoolManager.get_pool()`**: When `self._bot` is available,
  checks `bot.pools` instead of `pool_registry`, and delegates to `bot.build_v2_pool()`
  instead of `self.pool_factory()`. When `bot` is not available, falls back to the legacy path.

- `tests/test_v2_pool_io_free.py` — 9 tests covering I/O-free constructor, computation,
  split fees, no self-registration, pickle, `Bot.build_v2_pool()`, and manager integration.

**Key decisions & lessons**:

1. **`auto_update()` won't work on I/O-free pools**: It calls `_get_provider_for_chain()`
   which requires `_provider_from_connection_manager`. This is expected — state updates come
   via `external_update()` (which works) or a future `Bot.update_pool_state()`. The
   `auto_update()` method will be updated in a later phase.

2. **`w3` property still reaches into `connection_manager`**: For I/O-free pools, calling
   `pool.w3` will fail if no global provider is registered. This property will be removed or
   delegated to Bot in Phase 11.

3. **`from_exchange()` classmethod hits the legacy path**: It calls `cls(address=...,
   deployer_address=..., init_hash=...)` without the new I/O-free params. Users should migrate
   to `Bot.build_v2_pool()` instead.

4. **DB query in `build_v2_pool()` must be resilient**: The test DB may not have tables
   (created in `tmp_path` but no migration run). The query is wrapped in
   `contextlib.suppress(Exception)` so a missing table is treated as "not in DB".

5. **`CamelotLiquidityPool` inherits from `UniswapV2Pool`**: It calls `super().__init__()`
   and hits the legacy path. This will be migrated in Phase 6 (remaining pool families).

**What remains for Phase 3 (deferred to later phases)**:
- Remove legacy I/O `__init__` path once all V2 pool subclasses are migrated (Phase 6+)
- Remove `_provider`, `_provider_from_connection_manager`, `_get_provider_for_chain()` from pool
- Remove `w3` property or delegate to Bot
- Replace `auto_update()` with `Bot.update_pool_state()` or `external_update()` pattern
- Remove `from_exchange()` classmethod

### Phase 4: Extract I/O from `UniswapV3Pool` + `UniswapV3PoolManager` ✅

**Status**: Complete (legacy backward-compat shim retained).

**What was built**:

- **Dual-mode `UniswapV3Pool.__init__`**: When `token0`, `token1`, `factory`, `fee`,
  `tick_spacing`, `sqrt_price_x96`, `tick`, `liquidity` are all provided, the I/O-free path is
  taken (no provider, DB, or registry access). The I/O-free path also handles optional
  `tick_bitmap`/`tick_data` — when not provided, uses sparse mode (fetch on demand during swaps).

- **`Bot.build_v3_pool()`**: Full I/O flow —
  1. Checks DB for pool data (gracefully handles missing DB tables)
  2. If not in DB: fetches factory, tokens, fee, tick_spacing via `provider.call()`
  3. Fetches slot0 (sqrtPriceX96, tick) and liquidity via `provider.call()`
  4. Builds tokens via `bot.build_erc20token()`
  5. Fetches initial tick bitmap and tick data (sparse)
  6. If tick bitmap has initialized ticks, fetches their liquidity data
  7. Derives deployer/init_hash from `FACTORY_DEPLOYMENTS`
  8. Constructs I/O-free `UniswapV3Pool`
  9. Registers in `bot.pools`

- **`UniswapV3PoolManager.get_pool()`**: When `self._bot` is available,
  checks `bot.pools` instead of `pool_registry`, and delegates to `bot.build_v3_pool()`
  instead of `self._build_pool()`. When `bot` is not available, falls back to legacy path.

- `tests/test_v3_pool_io_free.py` — 8 tests covering I/O-free constructor, no provider,
  no self-registration, tick data, external_update, pickle, `Bot.build_v3_pool()`,
  and manager integration.

**Key decisions & lessons**:

1. **Sparse tick data handling**: When no initialized ticks exist at the word containing the
   current tick, `build_v3_pool` passes `tick_bitmap=None` and `tick_data=None` to enter sparse
   mode. When ticks ARE populated, both are passed. The condition `working_tick_data if working_tick_data else None`
   ensures both are `None` when empty (avoiding the "provide both tick_bitmap and tick_data" error).

2. **`_calculate_swap()` still fetches on-demand for sparse maps**: I/O-free pools with sparse
   liquidity maps call `_fetch_and_populate_initialized_ticks()` during swaps, which needs a
   provider. This is acceptable during transition — pools with full tick data (from snapshots)
   don't need the provider. For truly I/O-free sparse pools, Bot will need to provide the
   provider on demand.

3. **`update_liquidity_map()` also fetches on-demand**: For sparse maps, it calls
   `_fetch_and_populate_initialized_ticks()`. Same note as above.

4. **V3 slot0 returns 7 values but we only use 3**: The decode uses the full `SLOT0_STRUCT_TYPES`
   but only `sqrt_price_x96`, `tick` matter for the pool state. The rest (feeProtocol,
   unlocked) are discarded.

### Phase 5: Extract I/O from `UniswapV4Pool` + V4-specific managers ✅

**Status**: Complete (legacy backward-compat shim retained).

**What was built**:

- **Dual-mode `UniswapV4Pool.__init__`**: When `token0`, `token1`, `fee`, `tick_spacing`,
  `sqrt_price_x96`, `tick`, `liquidity`, `protocol_fee_zero_for_one`, `protocol_fee_one_for_zero`,
  `lp_fee` are all provided, the I/O-free path is taken. No `_provider`, no DB queries, no
  `_get_state_values()`, no `_fetch_and_populate_initialized_ticks()`, no self-registration
  in `managed_pool_registry`. The pool ID is verified against the calculated ID from the
  pool key.

- **`Bot.build_v4_pool()`**: Full I/O flow —
  1. Checks DB for pool data (gracefully handles missing DB tables)
  2. If not in DB: requires `state_view_address`, `fee`, `tick_spacing`, `tokens` params
  3. Fetches `getSlot0(bytes32)` and `getLiquidity(bytes32)` via `provider.call()` on the state view contract
  4. Decodes slot0: `sqrtPriceX96`, `tick`, packed `protocolFee`, `lpFee`
  5. Extracts two uint12 protocol fees from packed uint24
  6. Fetches initial tick bitmap via `getTickBitmap(bytes32,int16)` and populated ticks if nonzero
  7. Builds tokens via `bot.build_erc20token()`
  8. Constructs I/O-free `UniswapV4Pool`
  9. Registers in `bot.managed_pools`

- `tests/test_v4_pool_io_free.py` — 9 tests covering I/O-free constructor, no provider,
  no self-registration, tick data, external_update, pickle, pool_key/hooks, state_view_address,
  and `Bot.build_v4_pool()`.

**Key decisions & lessons**:

1. **V4 has no dedicated manager class**: Unlike V2/V3, V4 pools are directly constructed
   (no `UniswapV4PoolManager`). Registration goes to `managed_pool_registry` instead of
   `pool_registry`, and the Bot already has `self.managed_pools = ManagedPoolRegistry()`.

2. **V4 protocol_fee is packed**: The `uint24 protocolFee` from slot0 contains two uint12 fees
   (one_for_zero and zero_for_one) packed as `(zero_for_one << 12) | one_for_zero`. The
   I/O-free path takes these as separate params (`protocol_fee_zero_for_one`,
   `protocol_fee_one_for_zero`) and constructs a `ProtocolFee` dataclass.

3. **`_state_view_address` on I/O-free pools**: Set from `state_view_address` parameter (or
   `ZERO_ADDRESS` if not provided). This is needed for V4's on-demand tick fetching when the
   pool has a sparse liquidity map, but could be `ZERO_ADDRESS` for fully-populated pools.

### Phase 6: Extract I/O from remaining pool families ✅

**Status**: Complete (legacy backward-compat shims retained for all).

**What was built**:

- **CamelotLiquidityPool**: I/O-free path when `token0`, `token1`, `factory`, `fee_token0`, `fee_token1`, `fee_denominator`, `reserves_token0`, `reserves_token1`, `stable_swap` are provided. Delegates to `UniswapV2Pool` I/O-free `super().__init__()`. No self-registration in `pool_registry`. 3 tests.

- **AerodromeV2Pool**: I/O-free path when `token0`, `token1`, `factory`, `fee`, `stable`, `reserves_token0`, `reserves_token1` are provided. No `_provider`, no self-registration in `pool_registry`. 3 tests.

- **AerodromeV3Pool**: Already I/O-free — inherits from `UniswapV3Pool` without overriding `__init__`. No additional work needed.

- **BalancerV2Pool**: I/O-free path when `pool_id`, `vault`, `tokens`, `balances`, `fee`, `weights` are provided. No `_provider`, no self-registration. 2 tests.

- **CurveStableswapPool**: I/O-free path when `tokens`, `a_coefficient`, `fee`, `admin_fee`, `balances` are provided. Sets defaults for variant-specific attributes (fee_gamma, mid_fee, offpeg_fee_multiplier, etc.). No `_provider`, no self-registration in `pool_registry`. 2 tests.

`tests/test_remaining_pools_io_free.py` — 10 tests total covering all 4 pool families.

**Key decisions & lessons**:

1. **Camelot delegates to V2 I/O-free**: Camelot's `super().__init__()` calls the V2 I/O-free path with `fee_token0=Fraction(fee_token0, fee_denominator)`. The V2 I/O-free path accepts `Fraction` values for `fee_token0`/`fee_token1`.

2. **Curve's `_pickle_reconstructs` adds `_provider`**: For I/O-free Curve pools, pickle reconstructs `_provider=None` from `_pickle_reconstructs`. This is functionally harmless — the pool won't use `_provider=None`. For a truly clean pickle, we'd need to make `__setstate__` conditional on whether the original had `_provider`.

3. **Curve's variant attributes default to zero/None**: The I/O-free path sets defaults for variant-specific attributes (`fee_gamma`, `mid_fee`, `offpeg_fee_multiplier`, `base_pool`, etc.). Advanced use cases (metapools, lending pools) would need additional params or a future `Bot.build_curve_pool()` that fetches and resolves all variant data.

4. **Balancer has no pool_registry registration**: Unlike Uniswap/Aerodrome, BalancerV2Pool never registered in `pool_registry`. No registration to skip in the I/O-free path.

### Phase 7: Implement `AsyncBot` ✅

**Status**: Complete.

**What was built**:

- **`AsyncBot`** — async counterpart to `Bot`, using `AsyncConnectionManager` and `async def` methods:
  - `async build_erc20token()` — fetches token metadata from DB/async RPC, returns I/O-free `Erc20Token`
  - `async build_v2_pool()` — fetches pool data from DB/async RPC, returns I/O-free `UniswapV2Pool`
  - `async build_v3_pool()` — fetches pool data from DB/async RPC, returns I/O-free `UniswapV3Pool`
  - `async build_v4_pool()` — fetches pool data from DB/async RPC, returns I/O-free `UniswapV4Pool`
  - `async get_token_balance()` — cache-first, falls through to async RPC on miss
  - `async get_token_approval()` — cache-first, falls through to async RPC on miss
  - `async get_token_total_supply()` — cache-first, falls through to async RPC on miss
  - `async get_ether_balance()` — async native ETH balance query
  - `add_manager()` — same as sync Bot (sync — no I/O)
  - `get_token()` — registry lookup (sync — no I/O)

- **`async_raw_call()`** — new function in `degenbot.functions`, async counterpart to `raw_call()`. Uses `await provider.call()` instead of `provider.call()`.

- `tests/test_async_bot.py` — 7 tests covering init, `build_erc20token`, `build_v2_pool`, `build_v3_pool`, `build_v4_pool`, and `get_token_balance`.

**Key decisions & lessons**:

1. **Same I/O-free domain objects**: `AsyncBot` returns the exact same `Erc20Token`, `UniswapV2Pool`, `UniswapV3Pool`, `UniswapV4Pool` objects as `Bot`. The domain objects have no async methods — all async I/O is in the Bot.

2. **Fee defaults for V2**: When pool data is not in the DB, `build_v2_pool` defaults `fee_token0 = fee_token1 = Fraction(3, 1000)` (standard 0.3%), matching the sync `Bot` behavior.

3. **DB access is still sync**: The DB queries in `AsyncBot` use the same sync `self.db()` context manager. This is fine because SQLite is local and fast. If needed, async DB access can be added later.

4. **`_resolve_block_number` is async**: Unlike the sync version, the async `_resolve_block_number` uses `await provider.get_block_number()` since `AsyncProviderAdapter` doesn't support the sync `block_number` property.

### Phase 8: Update snapshots ✅

**Status**: Complete.

**What was built**:

- **`DatabaseSnapshot.__init__`** (both V3 and V4) — now accepts explicit `db: DatabaseSessionManager | None` and `database_path: pathlib.Path | None` keyword-only args. Falls back to `db_session`/`config.database.path` when not provided (backward-compat).

- **`DatabaseSnapshot.get_newest_block`** (V3) — fixed to use `self.session()` instead of module-level `db_session()`. V4 already used `self.session()`.

- **`fetch_new_events()`** (both V3 and V4) — accepts explicit `provider: Any | None = None` keyword-only arg. Falls back to `connection_manager.get_provider(self.chain_id)` when not provided.

- **`fetch_new_events_async()`** (both V3 and V4) — accepts explicit `w3: Any | None = None` keyword-only arg. Falls back to `async_connection_manager.get_web3(self.chain_id)` when not provided.

- `tests/test_snapshot_explicit_deps.py` — 6 tests covering explicit dep injection.

**Key decisions & lessons**:

1. **`db=` takes precedence over `database_path=`**: When `db` is provided, `database_path` is optional (defaults to empty Path). When only `database_path` is provided, a new `DatabaseSessionManager` is created from it. When neither is provided, falls back to globals.

2. **All fallback paths still import globals**: The `config`, `connection_manager`, `async_connection_manager`, `db_session`, `get_scoped_sqlite_session` imports remain for the backward-compat fallback `or` branches. These will be removed in Phase 11.

3. **V3 `get_newest_block` was a bug**: It used module-level `db_session()` instead of `self.session()`. Now fixed to match V4's pattern.

### Phase 9: Update CLI ✅

**Status**: Complete.

**What was built**:

- **`cli/__init__.py`** — Entry point creates `Bot.from_config_file()` and passes through Click's context via `ctx.obj["bot"]`.

- **`cli/database.py`** — All 4 commands (`backup`, `reset`, `upgrade`, `compact`) now receive `bot: Bot` via `@click.pass_obj`, using `bot.db()` and `bot.config.database.path` instead of `db_session` and `config`.

- **`cli/exchange.py`** — All activate/deactivate commands receive `bot: Bot` via `@click.pass_obj`, using `bot.db()` instead of `db_session()`.

- **`cli/pool.py`** — `pool_update` command receives `bot: Bot` via `@click.pass_obj`, using `bot.db()` instead of `db_session()`.

- **`cli/aave/commands.py`** — All aave commands (`activate`, `deactivate`, `update`, `position show`, `position risk`, `market show`) receive `bot: Bot` via `@click.pass_obj`, using `bot.db()` and `bot.db.remove()` instead of `db_session()` and `db_session.remove()`.

**Key decisions & lessons**:

1. **`@click.pass_obj` pattern**: Click passes the `ctx.obj` dict to the command function as the first positional argument. Each command receives `bot: Bot` and can access `bot.db`, `bot.config`, `bot.connections`, etc.

2. **No `db_session` or `config` imports remain in CLI code**: All module-level singleton references replaced with `bot.db()` and `bot.config.*`.

3. **CLI is database-centric**: The CLI doesn't use pool/token domain objects at all — it's a database management tool that uses `bot.db()` for raw SQL operations and `get_provider_from_config()` for RPC. The `Bot` serves as the container for `db` and `config`.

### Phase 10: Update pathfinding ✅

**Status**: Complete.

**What was built**:

- **`find_paths()`** — added optional `db: DatabaseSessionManager | None = None` parameter. Uses `db` when provided, falls back to `db_session` when not provided.

- **`find_paths_async()`** — same `db` parameter added.

**Key decisions & lessons**:

1. **Standalone functions, not Bot methods**: The plan suggested making them Bot methods, but `find_paths` is a pure database query that only needs `db`. Adding it as a Bot method would be over-engineering. The `db` parameter is the minimal, focused injection.

2. **Backward compat**: Existing callers that don't pass `db` still work via the fallback to `db_session`.

### Phase 11: Remove deprecated module-level singletons

#### Resolved design decisions

The following decisions resolve all open questions about what Phase 11 removes and how the remaining I/O surfaces are handled.

| # | Item | Decision |
|---|---|---|
| 1 | `auto_update()` replacement | `bot.update(pool)` — Bot dispatches by `type(pool)`, fetches state via its provider, calls `pool.external_update()`. `auto_update()` is removed from all pool classes. |
| 2 | V3/V4 sparse tick fetch | Moves to `bot.update(pool)`. Pools raise `MissingTickDataError` when a sparse liquidity map encounters an unpopulated word. `bot.update(pool)` catches it, fetches tick data via its provider, and calls `pool.update_tick_bitmap_at_word(...)` with pre-fetched data. Sparse mode is preserved. |
| 3 | Curve runtime I/O | Same `bot.update(pool)` pattern. Bot fetches A coefficient, admin balances, and other runtime state, then applies via `pool.external_update()`. Curve's `_get_A_coefficient()`, `admin_balances()`, and `_get_provider_for_chain()` are removed or converted to accept pre-fetched values only. |
| 4 | `Erc20Token.provider` / `async_w3` / `async_provider` | Clean break — remove properties and all deprecated I/O methods (`get_balance()`, `get_approval()`, `get_total_supply()` and async variants). Callers use `Bot`/`AsyncBot` equivalents. |
| 5 | Snapshot/pathfinding fallbacks | Remove all fallbacks. `db`/`provider`/`w3`/`database_path` become required parameters in `DatabaseSnapshot.__init__`, `fetch_new_events()`, `fetch_new_events_async()`, `find_paths()`, `find_paths_async()`. |
| 6 | `database/operations.py` / `migrations/env.py` | Accept explicit `database_path` parameter instead of reading from `config.database.path`. |
| 7 | `w3` property on V2/AerodromeV2 | Remove — clean break, consistent with I/O-free approach across all domain objects. |
| 8 | `db_session` global | Remove from `database/__init__.py`. `Bot` constructs its own `DatabaseSessionManager`. |
| 9 | `ChainlinkPriceContract` | Convert to I/O-free data container — accepts pre-fetched `address`, `decimals`, `chain_id`. `Bot` fetches the price. Tokens can still hold a reference for price oracle functionality. |
| 10 | `arbitrage/uniswap_curve_cycle.py` | I/O-free. No new parameters on calculation methods. Caller ensures Curve pool state is current via `bot.update(pool)` before calling. The `connection_manager` import and block-number fetch are removed. |
| 11 | `EtherPlaceholder` | Clean up — remove dead `connection_manager` import, remove `provider` parameter, remove `token_registry` self-registration. Static data adapter only. |
| 12 | Manager classes | `bot` is a required parameter on `AbstractPoolManager.__init__`. No legacy singleton fallback. Managers are always associated with a Bot. |
| 13 | `Erc20TokenManager` | Remove entirely. Callers use `bot.build_erc20token()` / `bot.get_token()` directly. |
| 14 | `config` global | Remove `degenbot.config.config` module-level variable. `Bot` holds its own config. `Config` class and `_init_config()` remain for construction. |
| 15 | `connection_manager` / `async_connection_manager` | Remove module-level singletons. `Bot` and `AsyncBot` hold their own `ConnectionManager` / `AsyncConnectionManager` instances. |
| 16 | `pool_registry` / `token_registry` / `managed_pool_registry` | Remove module-level singletons. `Bot` holds its own registry instances. |
| 17 | `contract/__init__.py` docstring | Update example to use `bot.get_provider()`. |

#### Implementation scope

After all consumers are migrated:

- Remove `degenbot.config.config` module-level variable
- Remove `degenbot.database.db_session` module-level variable
- Remove `degenbot.connection.connection_manager` / `async_connection_manager` module-level variables
- Remove `degenbot.connection.set_web3` / `set_provider` / `get_web3` / `get_provider`
- Remove `degenbot.registry.pool_registry` / `token_registry` / `managed_pool_registry`
- Remove `_LazyConfig` from `config.py`
- Remove backward-compat shims from pool/token constructors (legacy I/O paths)
- Remove `Erc20TokenManager` class
- Remove `auto_update()` from all pool classes
- Add `bot.update(pool)` with type dispatch for state fetching
- Add `MissingTickDataError` for sparse tick map signaling
- Remove `_provider`, `_provider_from_connection_manager`, `_get_provider_for_chain()`, `w3` property from all pool classes
- Remove `Erc20Token.provider`, `async_w3`, `async_provider` properties and deprecated I/O methods
- Convert `ChainlinkPriceContract` to I/O-free data container
- Add `database_path` parameter to `database/operations.py` and `migrations/env.py`
- Make `db`/`provider`/`w3`/`database_path` required in snapshots and pathfinding
- Clean up `EtherPlaceholder` (remove dead imports, provider param, self-registration)
- Make `bot` required on all manager `__init__` methods
- Update `contract/__init__.py` docstring

#### Progress (Phase 11 complete)

**Completed:**
- ✅ `UniswapV2Pool`: removed legacy `__init__` path, `from_exchange`, `auto_update()`, `_get_provider_for_chain()`, `w3` property, `get_pool_from_database()`, unused imports
- ✅ `UniswapV3Pool`: removed legacy `__init__` path, `from_exchange`, `auto_update()`, `_get_provider_for_chain()`, `_fetch_and_populate_initialized_ticks()`, `get_pool_from_database()`, unused imports; added `_tick_data_fetcher` param
- ✅ `UniswapV4Pool`: added `_tick_data_fetcher` param, updated sparse tick fetch in `_calculate_swap` and `update_liquidity_map` to use fetcher or raise, updated pickle drops
- ✅ `EtherPlaceholder`: removed `connection_manager` import, `provider` param, `token_registry` self-registration; pure static data adapter
- ✅ `CamelotLiquidityPool`: removed `silent=` from super().__init__ in I/O-free path
- ✅ `Erc20TokenManager`: removed entirely
- ✅ `Bot`: added `_make_tick_data_fetcher_v3()` and `_make_tick_data_fetcher_v4()` methods; `build_v3_pool()` and `build_v4_pool()` pass `tick_data_fetcher` to constructors
- ✅ Test conftests updated: V2, V3 conftests use I/O-free constructors with pre-fetched data
- ✅ Registry tests updated: explicit `pool_registry.add()` and `token_registry.add()` calls
- ✅ `bot.update(pool)`: implemented — V2, V3, V4, AerodromeV2, Curve
- ✅ `CurveStableswapPool`: removed remaining `connection_manager` refs
- ✅ Manager fallback paths to global registries — removed from `uniswap/managers.py`, `aerodrome/managers.py`
- ✅ Remove module-level singletons: `pool_registry`, `token_registry`, `managed_pool_registry`
- ✅ Remove module-level singletons: `config`, `db_session`
- ✅ Remove module-level singletons: `connection_manager`, `async_connection_manager`, `set_web3`, `get_web3`, `set_async_web3`, `get_async_web3`, `set_provider`, `get_provider`
- ✅ `Erc20Token.get_token_from_database()`: removed default `db_session` parameter
- ✅ `cli/utils.py`: removed global `config` import
- ✅ `database/operations.py`: removed global `config` import fallback
- ✅ `ChainlinkPriceContract.decimals`/`price` properties: removed `connection_manager` fallback
- ✅ `migrations/env.py`: accepts `DEGENBOT_DATABASE_PATH` env var
- ✅ Update core integration test files to use I/O-free constructors
- ✅ Update `__init__.py` exports (remove deprecated items)
- ✅ Update `contract/__init__.py` docstring to use `bot.get_provider()` example
- ✅ Remove `from_exchange()` from all managers (UniswapV2PoolManager, UniswapV3PoolManager, AerodromeV2PoolManager)
- ✅ Remove `auto_update` from `StateManageablePool` protocol
- ✅ Remove `_LazyConfig` class and module-level `config` variable from `config.py`
- ✅ Move `UNKNOWN_NAME/SYMBOL/DECIMALS` from `Erc20Token` class attributes to module-level constants
- ✅ Remove legacy `_build_pool()` from abstract V3 manager and concrete V3/PancakeswapV3 managers
- ✅ Abstract V3 `get_pool()` now delegates to `bot.build_v3_pool()` (fixes broken Aerodrome V3 path)
- ✅ Rename misleading `test_auto_update` tests → `test_bot_update_state` (aerodrome), `test_pool_state_at_different_blocks` (curve)
- ✅ Fix `test_pathfinding.py` — `db` fixture now properly injected via test function parameters
- ✅ 1805 offline tests pass (zero failures)

### Phase 12: Update tests

With I/O-free domain objects, testing becomes dramatically simpler:

- **Pool construction tests**: Pass test data directly — no `FakeProvider`, `FakeDbSession`,
  `FakePoolRegistry` needed
- **Manager tests**: Construct managers with a test `Bot`
- **Integration tests**: Create a `Bot` at the test level instead of relying on globals
- **I/O tests**: Test `Bot.build_*()` methods with mocked providers
- **Cache tests**: Construct a token, call `set_cached_balance()`, verify `get_cached_balance()`

Database test fixtures (`tests/database/conftest.py`) currently swap `db_session._session` via
`DatabaseSessionManager._reset()`. These will instead construct a `Bot` with a test database path.

## Relationship to existing plans

### Plan 5 (injectable pool registry) — merged into this plan

Plan 5's goal (make registry an explicit dependency) is achieved. Registration moves to the
manager/Bot (our resolved design decision), and pools/tokens lose all registry references.

### Plan 6 (injectable provider) — merged into this plan

Plan 6's goal (make provider an explicit dependency) is superseded. Domain objects don't have a
provider at all — I/O flows through Bot.

### Plan 2 (pool lifecycle mixin) — still valid, do after

Plan 2 extracts shared lifecycle code (pickle, registry self-registration) into a mixin. After
this plan, the mixin is simpler because:
- No provider to manage / pickle / reconstruct
- No `_get_provider_for_chain()` dynamic resolution
- Registration is external, so no `_register_in_pool_registry()` needed
- Pickle drops/reconstructs for `_provider` and `_provider_from_connection_manager` are gone

Do Plan 2 after this plan. The mixin will be much smaller and cleaner.

## Risks

- **Breaking change**: Removing the ability to construct pools directly (e.g.,
  `UniswapV3Pool("0x...")`) is a breaking API change. Users must go through `Bot` → manager →
  pool. Mitigate with deprecation warnings during the transition in Phases 2–6.

- **Large refactor scope**: ~35+ files need changes. Mitigate by doing phases incrementally,
  keeping the codebase working at each step. The backward-compat shims in Phases 2–6 ensure no
  functionality breaks during transition.

- **`database/__init__.py` import-time coupling**: The version check and `db_session` creation
  at module import time are the hardest couplings to break. `Bot.__init__` absorbs these.
  Code that imports `from degenbot.database import db_session` must be migrated.

- **State update I/O**: Resolved — `auto_update()` is removed from all pool classes. State updates flow through `bot.update(pool)`, which dispatches by pool type, fetches state via the bot's provider, and calls `pool.external_update()`. For sparse tick maps, pools raise `MissingTickDataError` and `bot.update(pool)` fetches and applies the missing data.

- **Alembic migrations**: `migrations/env.py` uses `degenbot.config` to find the database path.
  This is a standalone script invoked by `alembic` — it doesn't have a `Bot`. It should import
  `_init_config()` directly instead of going through `Bot`. This is already handled by the
  `degenbot_config` alias in the current code.

- **`from_exchange()` class methods**: These currently exist on pool classes (e.g.,
  `UniswapV2Pool.from_exchange(address, exchange)`) and construct pools with deployment info.
  These will be removed from the pool class and become `Bot` methods or manager methods that
  have access to `FACTORY_DEPLOYMENTS`.

- **Arbitrage classes**: Resolved — `UniswapCurveCycle` becomes I/O-free. The `connection_manager` import and block-number fetch are removed. The caller ensures Curve pool state is current via `bot.update(pool)` before calling calculation methods. No new parameters needed on the calculation methods.

## PR sequence

| PR | Phase | Description | Status |
|----|-------|-------------|--------|
| 1 | 1 | Create `Bot` class with `from_config_file()`, `AsyncBot` skeleton | ✅ Done |
| 2 | 2 | Extract I/O from `Erc20Token` + `EtherPlaceholder`, add `Bot.build_erc20token()` and I/O methods | ✅ Done |
| 3 | 3 | Extract I/O from `UniswapV2Pool` + `UniswapV2PoolManager`, add `Bot.build_v2_pool()` | ✅ Done |
| 4 | 4 | Extract I/O from `UniswapV3Pool` + `UniswapV3PoolManager`, add `Bot.build_v3_pool()` | ✅ Done |
| 5 | 5 | Extract I/O from `UniswapV4Pool` + V4 managers, add `Bot.build_v4_pool()` | ✅ Done |
| 6 | 6a | Extract I/O from Aerodrome pools + managers, add `Bot.build_aerodrome_*_pool()` | ✅ Done |
| 7 | 6b | Extract I/O from Curve, Camelot, Balancer pools, add corresponding `Bot.build_*()` | ✅ Done |
| 8 | 7 | Implement `AsyncBot` with async `build_*` and I/O methods | ✅ Done |
| 9 | 8 | Update snapshots to accept explicit deps | ✅ Done |
| 10 | 9 | Update CLI to use `Bot` | ✅ Done |
| 11 | 10 | Update pathfinding to accept explicit `db` | ✅ Done |
| 12 | 11 | Remove deprecated globals, backward-compat shims, `auto_update`, `Erc20TokenManager`; add `bot.update(pool)`, `MissingTickDataError`; convert `ChainlinkPriceContract` to data container; make `bot` required on managers; make deps required in snapshots/pathfinding | ✅ Done |
| 13 | 12 | Update tests | ✅ Done |

## Implementation notes

### Data flow: old vs. new

**Old flow** (current, legacy path):
```
Pool.__init__() 
  → Erc20TokenManager(chain_id, provider) 
    → Erc20Token(address, provider=..., chain_id=...)   # I/O in constructor
      → get_token_from_database(...)                     # DB read
      → provider.call(...)                               # RPC calls
      → token_registry.add(...)                          # self-registration
  → pool_registry.add(...)                                # self-registration
```

**New flow** (via Bot):
```
Bot.build_erc20token(address, chain_id)               # Bot does all I/O
  → self.db() / get_token_from_database(...)            # DB read
  → self.connections.get_provider(...).call(...)         # RPC calls
  → Erc20Token(address, name=..., symbol=..., decimals=...)  # I/O-free constructor
  → self.tokens.add(...)                                 # registration by Bot

Bot.build_v2_pool(address, ...)
  → self.build_erc20token(token0_address)                # Token via Bot
  → self.build_erc20token(token1_address)
  → self._fetch_v2_reserves(provider, address)           # RPC call
  → UniswapV2Pool(address, token0=..., token1=..., reserves=...)  # I/O-free
  → self.pools.add(...)                                  # registration by Bot
```

### Transition strategy

Each pool family follows the same transition:
1. Add `bot: Bot | None` to the manager constructor
2. Add `Bot.build_*_pool()` method
3. Manager delegates to Bot when `self._bot is not None`
4. Pool constructor gains I/O-free path (data params) alongside legacy path (deprecation)
5. Once all pools in a family are I/O-free, remove the legacy constructor path
6. Once all families are I/O-free, remove the legacy I/O methods from `Erc20Token`

The backward-compat shims ensure the codebase is always in a working state.
