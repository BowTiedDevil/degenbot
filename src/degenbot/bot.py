from __future__ import annotations

import warnings
from typing import TYPE_CHECKING, Any

from alembic.runtime.migration import MigrationContext
from alembic.script import ScriptDirectory

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.builders.aerodrome_v2_builder import AerodromeV2Builder
from degenbot.builders.camelot_builder import CamelotBuilder
from degenbot.builders.context import BuilderContext
from degenbot.builders.curve_pool_builder import CurvePoolBuilder
from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.builders.pool_io import SyncPoolIO
from degenbot.builders.type_resolution import (
    fetch_factory_from_chain,
    pool_class_for_descriptor,
)
from degenbot.builders.type_resolution import (
    resolve_pool_type as _resolve_pool_type_impl,
)
from degenbot.builders.v2_pool_builder import V2PoolBuilder
from degenbot.builders.v3_pool_builder import V3PoolBuilder
from degenbot.builders.v4_pool_builder import V4PoolBuilder
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DegenbotConfig, _init_config
from degenbot.connection.connection_manager import ConnectionManager
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.database.operations import get_alembic_config, get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.pool import TrackerAlreadyInitialized
from degenbot.logging import logger
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool
from degenbot.version import __version__

if TYPE_CHECKING:
    from collections.abc import Sequence

    from eth_typing import ChecksumAddress
    from web3 import Web3
    from web3.types import BlockIdentifier

    from degenbot.builders.protocol import PoolBuilder
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.abstract.pool_tracker import AbstractPoolTracker
    from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick

from degenbot.provider.interface import AsyncProviderAdapter, ProviderAdapter
from degenbot.provider.subscription import Subscription  # noqa: TC001
from degenbot.types.aliases import ChainId  # noqa: TC001


class Bot:
    """
    Explicit session object that owns the runtime state for a degenbot run.

    Replaces the four module-level singletons (`config`, `db_session`,
    `connection_manager`, `pool_registry`/`token_registry`/`managed_pool_registry`)
    with per-session instances owned by this class.

    Bot is:
    - **Factory** — creates pools/tokens via managers, doing all I/O to fetch data
    - **Registry** — tracks what it's created
    - **I/O boundary** — all RPC calls and database access flow through Bot
    - **Session** — the lifetime scope for the entire run
    """

    def __init__(self, config: DegenbotConfig) -> None:
        self.config = config
        self.connections = ConnectionManager()
        self.db = DatabaseSessionManager(
            get_scoped_sqlite_session(database_path=config.database.path)
        )
        self.pools = PoolRegistry()
        self.tokens = TokenRegistry()
        self.managed_pools = ManagedPoolRegistry()
        self._trackers: dict[tuple[ChainId, str], AbstractPoolTracker[Any]] = {}

        # Builders own I/O orchestration; Bot hands them its I/O dependencies.
        # Erc20Builder is a leaf — constructed before BuilderContext.
        self._erc20_builder = Erc20Builder(
            default_chain_id=None, db=self.db, tokens=self.tokens
        )
        ctx = BuilderContext(
            connections=self.connections,
            db=self.db,
            pools=self.pools,
            tokens=self.tokens,
            erc20_builder=self._erc20_builder,
            default_chain_id=None,
            managed_pools=self.managed_pools,
        )
        self._v2_builder = V2PoolBuilder(ctx)
        self._aerodrome_v2_builder = AerodromeV2Builder(ctx)
        self._camelot_builder = CamelotBuilder(ctx)
        self._v3_builder = V3PoolBuilder(ctx)
        self._v4_builder = V4PoolBuilder(ctx)
        self._curve_builder = CurvePoolBuilder(ctx)

        # Builder registry: concrete pool type → builder
        # Used by update() for O(1) dict lookup instead of isinstance chain
        self._builders: dict[type, PoolBuilder] = {}

        # Async adapters for subscriptions, keyed by chain_id
        self._async_adapters: dict[ChainId, AsyncProviderAdapter] = {}
        self.register_builder(UniswapV2Pool, self._v2_builder)
        self.register_builder(UniswapV3Pool, self._v3_builder)
        self.register_builder(UniswapV4Pool, self._v4_builder)
        self.register_builder(CurveStableswapPool, self._curve_builder)
        self.register_builder(AerodromeV2Pool, self._aerodrome_v2_builder)
        self.register_builder(CamelotLiquidityPool, self._camelot_builder)
        # SushiswapV2Pool, PancakeSwapV2Pool, SwapbasedV2Pool, etc. inherit
        # UniswapV2Pool so they are handled by the V2 builder via MRO fallback

        # Check database migration version
        self._check_database_version()

    @property
    def chain_id(self) -> ChainId:
        """Return the default chain ID from the connection manager."""
        return self.connections.default_chain_id

    def _check_database_version(self) -> None:
        """Warn if the database schema is out of date."""

        try:
            with self.db():
                current_version = MigrationContext.configure(
                    connection=self.db.connection()
                ).get_current_revision()
        except Exception:  # noqa: BLE001
            return

        latest_version = ScriptDirectory.from_config(
            config=get_alembic_config(database_path=self.config.database.path)
        ).get_current_head()

        if current_version is not None and current_version != latest_version:
            logger.warning(
                f"The current database revision ({current_version}) does not match the latest "
                f"({latest_version}) for {__package__} version {__version__}!"
                "\n"
                "Database-related features may raise exceptions if you continue. Perform database "
                "migrations with 'degenbot database upgrade'."
            )

    @classmethod
    def from_config_file(cls) -> Bot:
        return cls(config=_init_config())

    def add_tracker[M: AbstractPoolTracker[Any]](
        self,
        manager_cls: type[M],
        *,
        factory_address: str,
        chain_id: ChainId | None = None,
        **kwargs: Any,
    ) -> M:
        """Create a pool manager within this bot's session."""
        factory_address = get_checksum_address(factory_address)
        chain_id = chain_id or self.connections.default_chain_id

        key = (chain_id, factory_address)
        if key in self._trackers:
            raise TrackerAlreadyInitialized(
                message="A manager has already been initialized for this address. "
                "Access it using the bot's manager registry."
            )

        manager = manager_cls(
            factory_address=factory_address,
            chain_id=chain_id,
            bot=self,
            **kwargs,
        )
        self._trackers[key] = manager
        return manager

    def register_builder(
        self,
        pool_class: type[AbstractLiquidityPool],
        builder: PoolBuilder,
    ) -> None:
        """Register a builder for a concrete pool type.

        After registration, ``update()`` will use ``type(pool)`` dict lookup
        instead of isinstance chains to find the right builder.

        Args:
            pool_class: The concrete pool class (e.g. UniswapV2Pool, AerodromeV2Pool).
            builder: The builder instance that handles construction and updates
                for this pool type.
        """
        self._builders[pool_class] = builder

    def build_erc20token(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        silent: bool = False,
    ) -> Erc20Token:
        """Fetch token metadata from DB/RPC and construct an I/O-free Erc20Token."""
        resolved_chain_id = chain_id or self.connections.default_chain_id
        io = SyncPoolIO(self.connections.get_provider(resolved_chain_id))
        return self._erc20_builder.build(address, chain_id=resolved_chain_id, silent=silent, io=io)

    def get_token(self, address: str, *, chain_id: ChainId | None = None) -> Erc20Token:
        """Get or create a token. Bot handles DB lookup, RPC calls, and registration."""
        return self.build_erc20token(address, chain_id=chain_id)

    def build_pool(
        self,
        address: str,
        *,
        pool_id: str | bytes | None = None,
        chain_id: ChainId | None = None,
        state_block: int | None = None,
        silent: bool = False,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        tick_bitmap: dict[int, Any] | None = None,
        tick_data: dict[int, Any] | None = None,
        state_cache_depth: int = 8,
        # V4-specific kwargs
        state_view_address: str | None = None,
        tokens: Sequence[str] | None = None,
        fee: int | None = None,
        tick_spacing: int | None = None,
        hook_address: str | None = None,
    ) -> AbstractLiquidityPool:
        """
        Build a pool from an address, automatically resolving its type.

        When `pool_id` is provided, `address` is interpreted as a V4 PoolManager
        contract. Without it, `address` is a pool contract and the type is resolved
        from the pool registry, database, or factory address.
        """
        address = get_checksum_address(address)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)
        io = SyncPoolIO(provider)

        # V4 fast path: pool_id discriminates V4 managed pools
        if pool_id is not None:
            v4_kwargs: dict[str, Any] = {
                "pool_id": pool_id,
                "pool_manager_address": address,
                "chain_id": chain_id,
                "state_block": state_block,
                "silent": silent,
            }
            if state_view_address is not None:
                v4_kwargs["state_view_address"] = state_view_address
            if tokens is not None:
                v4_kwargs["tokens"] = tokens
            if fee is not None:
                v4_kwargs["fee"] = fee
            if tick_spacing is not None:
                v4_kwargs["tick_spacing"] = tick_spacing
            if hook_address is not None:
                v4_kwargs["hook_address"] = hook_address
            if tick_bitmap is not None:
                v4_kwargs["tick_bitmap"] = tick_bitmap
            if tick_data is not None:
                v4_kwargs["tick_data"] = tick_data
            return self._v4_builder.build(address, io=io, **v4_kwargs)

        # Check pool registry — return existing pool if already built
        existing = self.pools.get(chain_id=chain_id, pool_address=address)
        if existing is not None:
            return existing

        # Resolve the pool type and dispatch to the appropriate builder
        #
        # If type resolution fails (e.g. Curve pools lack a factory() method),
        # fall back to the typed builder methods which handle their own discovery.
        try:
            pool_type = _resolve_pool_type_impl(
                address, chain_id=chain_id, io=io, db=self.db
            )
        except DegenbotValueError:
            # Fallback: try Curve builder as last resort
            return self._curve_builder.build(
                address,
                chain_id=chain_id,
                state_block=state_block,
                silent=silent,
                state_cache_depth=state_cache_depth,
                io=io,
            )

        # Look up the concrete pool class from the registry
        pool_class = pool_class_for_descriptor(pool_type, chain_id=chain_id)
        builder = self._builders.get(pool_class)
        if builder is None:
            # Fallback: walk MRO of pool_class looking for a registered builder
            for base in pool_class.__mro__:
                builder = self._builders.get(base)
                if builder is not None:
                    break

        if builder is None:
            raise DegenbotValueError(message=f"No builder for pool class {pool_class.__name__}")

        # Build kwargs dict — only include non-None optional params
        # so builders that don't accept tick_bitmap/tick_data etc.
        # don't get unexpected keyword arguments.
        dispatch_kwargs: dict[str, Any] = {
            "silent": silent,
            "state_cache_depth": state_cache_depth,
        }
        if deployer_address is not None:
            dispatch_kwargs["deployer_address"] = deployer_address
        if init_hash is not None:
            dispatch_kwargs["init_hash"] = init_hash
        if state_block is not None:
            dispatch_kwargs["state_block"] = state_block
        if tick_bitmap is not None:
            dispatch_kwargs["tick_bitmap"] = tick_bitmap
        if tick_data is not None:
            dispatch_kwargs["tick_data"] = tick_data

        return self._dispatch_build(
            builder=builder,
            address=address,
            chain_id=chain_id,
            io=io,
            **dispatch_kwargs,
        )

    @staticmethod
    def _dispatch_build(
        *,
        builder: PoolBuilder,
        address: ChecksumAddress,
        chain_id: ChainId,
        **kwargs: Any,
    ) -> AbstractLiquidityPool:
        """Dispatch to the builder, forwarding all kwargs.

        Each builder's build() accepts the kwargs it recognizes and raises
        TypeError for unrecognized ones — which is correct behavior if
        build_pool() routes to the wrong builder.
        """
        return builder.build(address, chain_id=chain_id, **kwargs)

    def build_v2_pool(
        self,
        pool_address: str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        state_block: int | None = None,
        silent: bool = False,
    ) -> AbstractLiquidityPool:
        """.. deprecated:: 0.x
        Use ``build_pool(address)`` instead. Type resolution automatically
        selects the correct builder.
        """
        warnings.warn(
            "build_v2_pool() is deprecated — use build_pool(address) instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        pool_address = get_checksum_address(pool_address)
        chain_id = chain_id or self.connections.default_chain_id

        # Determine the factory to identify the correct builder
        provider = self.connections.get_provider(chain_id)
        io = SyncPoolIO(provider)
        factory = fetch_factory_from_chain(pool_address, chain_id=chain_id, io=io)
        if factory is not None:
            pool_class = pool_type_registry.get_v2_class(chain_id, factory)
            if pool_class is not None and issubclass(pool_class, AerodromeV2Pool):
                return self._aerodrome_v2_builder.build(
                    pool_address,
                    chain_id=chain_id,
                    deployer_address=deployer_address,
                    init_hash=init_hash,
                    state_block=state_block,
                    silent=silent,
                    io=io,
                )
            if pool_class is not None and issubclass(pool_class, CamelotLiquidityPool):
                return self._camelot_builder.build(
                    pool_address,
                    chain_id=chain_id,
                    deployer_address=deployer_address,
                    init_hash=init_hash,
                    state_block=state_block,
                    silent=silent,
                    io=io,
                )

        return self._v2_builder.build(
            pool_address,
            chain_id=chain_id,
            deployer_address=deployer_address,
            init_hash=init_hash,
            state_block=state_block,
            silent=silent,
            io=io,
        )

    def get_token_balance(
        self,
        token: Erc20Token,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the ERC-20 balance for the given address."""
        assert token.chain_id is not None
        io = SyncPoolIO(self.connections.get_provider(token.chain_id))
        return self._erc20_builder.get_token_balance(
            token, address, block_identifier=block_identifier, io=io
        )

    def get_token_approval(
        self,
        token: Erc20Token,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the amount that can be spent by `spender` on behalf of `owner`."""
        assert token.chain_id is not None
        io = SyncPoolIO(self.connections.get_provider(token.chain_id))
        return self._erc20_builder.get_token_approval(
            token, owner, spender, block_identifier=block_identifier, io=io
        )

    def get_token_total_supply(
        self,
        token: Erc20Token,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the total supply for this token."""
        assert token.chain_id is not None
        io = SyncPoolIO(self.connections.get_provider(token.chain_id))
        return self._erc20_builder.get_token_total_supply(
            token, block_identifier=block_identifier, io=io
        )

    def get_ether_balance(
        self,
        chain_id: ChainId,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the native ETH balance for the given address."""
        io = SyncPoolIO(self.connections.get_provider(chain_id))
        return self._erc20_builder.get_ether_balance(
            chain_id, address, block_identifier=block_identifier, io=io
        )

    def build_v3_pool(
        self,
        pool_address: str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        state_block: int | None = None,
        tick_bitmap: dict[int, BitmapAtWord] | None = None,
        tick_data: dict[int, LiquidityAtTick] | None = None,
        silent: bool = False,
    ) -> AbstractLiquidityPool:
        """.. deprecated:: 0.x
        Use ``build_pool(address)`` instead. Type resolution automatically
        selects the correct builder.
        """
        warnings.warn(
            "build_v3_pool() is deprecated — use build_pool(address) instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        pool_address = get_checksum_address(pool_address)
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)
        io = SyncPoolIO(provider)
        return self._v3_builder.build(
            pool_address,
            chain_id=chain_id,
            deployer_address=deployer_address,
            init_hash=init_hash,
            state_block=state_block,
            tick_bitmap=tick_bitmap,
            tick_data=tick_data,
            silent=silent,
            io=io,
        )

    def build_v4_pool(
        self,
        *,
        pool_id: str | bytes,
        pool_manager_address: str,
        state_view_address: str | None = None,
        tokens: Sequence[str] | None = None,
        fee: int | None = None,
        tick_spacing: int | None = None,
        hook_address: str | None = None,
        chain_id: ChainId | None = None,
        state_block: int | None = None,
        tick_bitmap: dict[int, BitmapAtWord] | None = None,
        tick_data: dict[int, LiquidityAtTick] | None = None,
        silent: bool = False,
    ) -> AbstractLiquidityPool:
        """.. deprecated:: 0.x
        Use ``build_pool(address, pool_id=...)`` instead.
        """
        warnings.warn(
            "build_v4_pool() is deprecated — use build_pool(address, pool_id=...) instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        chain_id = chain_id or self.connections.default_chain_id
        provider = self.connections.get_provider(chain_id)
        io = SyncPoolIO(provider)
        return self._v4_builder.build(
            pool_manager_address,
            pool_id=pool_id,
            pool_manager_address=pool_manager_address,
            state_view_address=state_view_address,
            tokens=tokens,
            fee=fee,
            tick_spacing=tick_spacing,
            hook_address=hook_address,
            chain_id=chain_id,
            state_block=state_block,
            tick_bitmap=tick_bitmap,
            tick_data=tick_data,
            silent=silent,
            io=io,
        )

    def build_curve_pool(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        state_block: int | None = None,
        silent: bool = False,
        state_cache_depth: int = 8,
    ) -> AbstractLiquidityPool:
        """.. deprecated:: 0.x
        Use ``build_pool(address)`` instead. Type resolution automatically
        selects the correct builder.
        """
        warnings.warn(
            "build_curve_pool() is deprecated — use build_pool(address) instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        return self._curve_builder.build(
            address,
            chain_id=chain_id,
            state_block=state_block,
            silent=silent,
            state_cache_depth=state_cache_depth,
            io=SyncPoolIO(
                self.connections.get_provider(
                    chain_id or self.connections.default_chain_id
                )
            ),
        )

    def get_provider(self, *, chain_id: ChainId) -> ProviderAdapter:
        return self.connections.get_provider(chain_id)

    def get_web3(self, *, chain_id: ChainId) -> Web3:
        """.. deprecated:: 0.x
        Use ``get_provider(chain_id)`` instead.
        """
        return self.connections.get_web3(chain_id)

    async def start_listening(
        self,
        chain_id: ChainId | None = None,
    ) -> tuple[Subscription, Subscription]:
        """Start WS subscriptions for newHeads and unfiltered logs.

        Creates an AsyncProviderAdapter from the configured WS URI for
        the given chain, subscribes to new block headers and unfiltered
        log events (``eth_subscribe("logs", {})``), and returns both
        subscriptions as a tuple ``(heads_sub, logs_sub)``.

        The adapter is cached — calling again for the same chain_id
        returns the existing adapter's subscriptions.

        Args:
            chain_id: Chain to subscribe on. Defaults to the default chain.

        Returns:
            Tuple of ``(heads_subscription, logs_subscription)``.

        Raises:
            DegenbotValueError: If no WS URI is configured for the chain.
            SubscriptionNotSupported: If the provider doesn't support WS.
        """
        chain_id = chain_id or self.connections.default_chain_id

        # Reuse existing adapter if already created
        existing = self._async_adapters.get(chain_id)
        if existing is not None:
            heads = await existing.subscribe_blocks()
            logs = await existing.subscribe_logs()
            return heads, logs

        # Look up WS URI from config
        ws_uri = self.config.ws.get(chain_id)
        if ws_uri is None:
            msg = f"No WS URI configured for chain {chain_id}. Add [ws.{chain_id}] to config."
            raise DegenbotValueError(message=msg)

        # Create async alloy provider from WS URI
        from degenbot.provider import AsyncAlloyProvider  # noqa: PLC0415

        alloy = await AsyncAlloyProvider.create(str(ws_uri))
        adapter = AsyncProviderAdapter.from_alloy(alloy)

        self._async_adapters[chain_id] = adapter

        heads = await adapter.subscribe_blocks()
        logs = await adapter.subscribe_logs()
        return heads, logs

    def update(
        self,
        pool: AbstractLiquidityPool,
        *,
        block_number: BlockIdentifier | None = None,
    ) -> bool:
        """
        Fetch the current state of a pool from the chain and apply it via
        ``pool.external_update()``.

        Returns True if the state changed, False if unchanged.
        """
        builder = self._builder_for_pool(pool)
        resolved_block_number = (
            int(block_number)
            if block_number is not None and not isinstance(block_number, int)
            else block_number
        )
        provider = self.connections.get_provider(pool.chain_id)  # ty: ignore
        io = SyncPoolIO(provider)
        return builder.update(pool, block_number=resolved_block_number, io=io)

    def _builder_for_pool(
        self,
        pool: AbstractLiquidityPool,
    ) -> PoolBuilder:
        """Select the appropriate builder for the pool type.

        Uses the builder registry (dict lookup on type(pool)) first, then
        falls back to isinstance checks for subclasses not explicitly registered
        (e.g. SushiswapV2Pool inherits from UniswapV2Pool).
        """
        # Fast path: exact type match in the registry
        builder = self._builders.get(type(pool))
        if builder is not None:
            return builder

        # Slow path: subclass match (e.g. SushiswapV2Pool is subclass of UniswapV2Pool)
        # Walk the MRO looking for a registered builder
        for base in type(pool).__mro__:
            builder = self._builders.get(base)
            if builder is not None:
                return builder

        msg = f"update() not implemented for pool type {type(pool).__name__}"
        raise TypeError(msg)
