from __future__ import annotations

import contextlib
import warnings
from typing import TYPE_CHECKING, Any

import eth_abi.abi
from alembic.runtime.migration import MigrationContext
from alembic.script import ScriptDirectory
from sqlalchemy import select

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.builders.aerodrome_v2_builder import AerodromeV2Builder
from degenbot.builders.camelot_builder import CamelotBuilder
from degenbot.builders.curve_pool_builder import CurvePoolBuilder
from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.builders.protocol import PoolBuilder
from degenbot.builders.v2_pool_builder import V2PoolBuilder
from degenbot.builders.v3_pool_builder import V3PoolBuilder
from degenbot.builders.v4_pool_builder import V4PoolBuilder
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DegenbotConfig, _init_config
from degenbot.connection.connection_manager import ConnectionManager
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.database.models.pools import LiquidityPoolTable
from degenbot.database.operations import get_alembic_config, get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.pool import TrackerAlreadyInitialized
from degenbot.logging import logger
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.pool_protocols import ConcentratedLiquidityPool, ConstantProductPool
from degenbot.types.pool_type import PoolFamily, PoolTypeDescriptor, derive_kind
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool
from degenbot.version import __version__

if TYPE_CHECKING:
    from collections.abc import Sequence

    from eth_typing import ChecksumAddress
    from web3.types import BlockIdentifier

    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.abstract.pool_tracker import AbstractPoolTracker
    from degenbot.types.aliases import ChainId
    from degenbot.uniswap.v3_types import UniswapV3BitmapAtWord, UniswapV3LiquidityAtTick
    from degenbot.uniswap.v4_types import UniswapV4BitmapAtWord, UniswapV4LiquidityAtTick


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

        # Builders own I/O orchestration; Bot hands them its I/O dependencies
        self._erc20_builder = Erc20Builder(
            connections=self.connections, db=self.db, tokens=self.tokens
        )
        self._v2_builder = V2PoolBuilder(
            connections=self.connections,
            db=self.db,
            pools=self.pools,
            tokens=self.tokens,
            erc20_builder=self._erc20_builder,
        )
        self._aerodrome_v2_builder = AerodromeV2Builder(
            connections=self.connections,
            db=self.db,
            pools=self.pools,
            tokens=self.tokens,
            erc20_builder=self._erc20_builder,
        )
        self._camelot_builder = CamelotBuilder(
            connections=self.connections,
            db=self.db,
            pools=self.pools,
            tokens=self.tokens,
            erc20_builder=self._erc20_builder,
        )
        self._v3_builder = V3PoolBuilder(
            connections=self.connections,
            db=self.db,
            pools=self.pools,
            tokens=self.tokens,
            managed_pools=self.managed_pools,
            erc20_builder=self._erc20_builder,
        )
        self._v4_builder = V4PoolBuilder(
            connections=self.connections,
            db=self.db,
            pools=self.pools,
            tokens=self.tokens,
            managed_pools=self.managed_pools,
            erc20_builder=self._erc20_builder,
        )
        self._curve_builder = CurvePoolBuilder(
            connections=self.connections,
            db=self.db,
            pools=self.pools,
            tokens=self.tokens,
            erc20_builder=self._erc20_builder,
        )

        # Builder registry: concrete pool type → builder
        # Used by update() for O(1) dict lookup instead of isinstance chain
        self._builders: dict[type, PoolBuilder] = {}
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
            with self.db() as session:
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
        return self._erc20_builder.build(address, chain_id=chain_id, silent=silent)

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
            return self._v4_builder.build(**v4_kwargs)

        # Check pool registry — return existing pool if already built
        existing = self.pools.get(chain_id=chain_id, pool_address=address)
        if existing is not None:
            return existing

        # Resolve the pool type and dispatch to the appropriate builder
        #
        # If type resolution fails (e.g. Curve pools lack a factory() method),
        # fall back to the typed builder methods which handle their own discovery.
        try:
            pool_type = self._resolve_pool_type(address, chain_id=chain_id)
        except DegenbotValueError:
            # Fallback: try Curve builder as last resort
            return self._curve_builder.build(
                address,
                chain_id=chain_id,
                state_block=state_block,
                silent=silent,
                state_cache_depth=state_cache_depth,
            )

        # Look up the concrete pool class from the registry
        pool_class = self._pool_class_for_descriptor(pool_type, chain_id=chain_id)
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
            **dispatch_kwargs,
        )

    def _dispatch_build(
        self,
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

    def _resolve_pool_type(
        self,
        address: ChecksumAddress,
        *,
        chain_id: ChainId,
    ) -> PoolTypeDescriptor:
        """
        Resolve the pool type for the given address.

        Consults these sources in order:
        1. Database `kind` column (exact polymorphic type)
        2. PoolTypeRegistry registration (factory address → descriptor)
        3. On-chain probing (slot0 vs getReserves) when factory is unknown

        Raises DegenbotValueError if the type cannot be determined.
        """
        # Step 1: DB lookup — the `kind` column is the most direct signal
        with contextlib.suppress(Exception), self.db() as session:
            pool_from_db = session.scalar(
                select(LiquidityPoolTable).where(
                    LiquidityPoolTable.address == address,
                    LiquidityPoolTable.chain == chain_id,
                )
            )
            if pool_from_db is not None:
                kind = pool_from_db.kind
                descriptor = pool_type_registry.get_descriptor_by_kind(kind)
                if descriptor is not None:
                    return PoolTypeDescriptor(
                        family=descriptor.family,
                        variant=descriptor.variant,
                        kind=descriptor.kind,
                        factory=get_checksum_address(pool_from_db.exchange.factory),
                    )

        # Step 2: Factory address lookup via PoolTypeRegistry
        factory = self._fetch_factory_from_chain(address, chain_id=chain_id)
        if factory is not None:
            # Check if the factory is registered in the pool type registry
            registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
            if registry_descriptor is not None:
                return registry_descriptor

            # Step 3: No registry match — probe the contract to determine invariant
            return self._resolve_pool_type_by_probing(address, chain_id=chain_id, factory=factory)

        raise DegenbotValueError(
            message=f"Cannot resolve pool type for address {address} on chain {chain_id}. "
            f"The factory() call failed and no database entry exists. "
            f"Cannot resolve pool type for address {address} on chain {chain_id}. "
            f"The factory() call failed and no database entry exists."
        )

    def _resolve_pool_type_by_probing(
        self,
        address: ChecksumAddress,
        *,
        chain_id: ChainId,
        factory: ChecksumAddress,
    ) -> PoolTypeDescriptor:
        """
        Determine pool type by probing the contract on-chain.

        Tries V3 methods first (slot0), then V2 methods (getReserves),
        then Curve methods (coins). This is the fallback when neither
        the DB nor the registry identifies the factory.
        """
        provider = self.connections.get_provider(chain_id)

        # Try V3: slot0() exists → CONCENTRATED_LIQUIDITY
        try:
            provider.call(
                to=address,
                data=encode_function_calldata("slot0()", None),
            )
            # If we got here without reverting, it's a V3 pool
            registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
            if registry_descriptor is not None:
                return registry_descriptor
            return PoolTypeDescriptor(
                family=PoolFamily.CONCENTRATED_LIQUIDITY,
                variant=None,
                kind=derive_kind(PoolFamily.CONCENTRATED_LIQUIDITY, None),
                factory=factory,
            )
        except Exception:
            pass

        # Try V2: getReserves() exists → CONSTANT_PRODUCT
        try:
            provider.call(
                to=address,
                data=encode_function_calldata("getReserves()", None),
            )
            registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
            if registry_descriptor is not None:
                return registry_descriptor
            return PoolTypeDescriptor(
                family=PoolFamily.CONSTANT_PRODUCT,
                variant=None,
                kind=derive_kind(PoolFamily.CONSTANT_PRODUCT, None),
                factory=factory,
            )
        except Exception:
            pass

        # Fall through to Curve — assume STABLESWAP if nothing else matched
        return PoolTypeDescriptor(
            family=PoolFamily.STABLESWAP,
            variant=None,
            kind=derive_kind(PoolFamily.STABLESWAP, None),
            factory=factory,
        )

    def _pool_class_for_descriptor(
        self,
        pool_type: PoolTypeDescriptor,
        *,
        chain_id: ChainId,
    ) -> type[AbstractLiquidityPool]:
        """Resolve a PoolTypeDescriptor to a concrete pool class.

        Consults the pool_type_registry to find the registered class
        for this factory on this chain. Falls back to a default class
        based on the family if no specific registration exists.
        """
        if pool_type.factory is not None:
            pool_class = pool_type_registry.get_class(chain_id, pool_type.factory)
            if pool_class is not None:
                return pool_class

        # Default classes when no factory-specific registration exists
        match pool_type.family:
            case PoolFamily.CONSTANT_PRODUCT:
                return (
                    pool_type_registry.get_v2_class(chain_id, pool_type.factory or "")
                    or UniswapV2Pool
                )
            case PoolFamily.CONCENTRATED_LIQUIDITY:
                return (
                    pool_type_registry.get_v3_class(chain_id, pool_type.factory or "")
                    or UniswapV3Pool
                )
            case PoolFamily.STABLESWAP:
                return CurveStableswapPool
            case _:
                msg = f"No pool class for family {pool_type.family.value!r}"
                raise DegenbotValueError(message=msg)

    def _fetch_factory_from_chain(
        self, address: ChecksumAddress, *, chain_id: ChainId
    ) -> ChecksumAddress | None:
        """Fetch the factory address from the pool contract's factory() method."""
        provider = self.connections.get_provider(chain_id)
        try:
            factory_result = provider.call(
                to=address,
                data=encode_function_calldata("factory()", None),
            )
            (factory_raw,) = eth_abi.abi.decode(types=["address"], data=factory_result)
            return get_checksum_address(factory_raw)
        except Exception:
            return None

    def build_v2_pool(
        self,
        pool_address: str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        state_block: int | None = None,
        silent: bool = False,
    ) -> ConstantProductPool:
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
        factory = self._fetch_factory_from_chain(pool_address, chain_id=chain_id)
        if factory is not None:
            pool_class = pool_type_registry.get_v2_class(chain_id, factory)
            if issubclass(pool_class, AerodromeV2Pool):
                return self._aerodrome_v2_builder.build(
                    pool_address,
                    chain_id=chain_id,
                    deployer_address=deployer_address,
                    init_hash=init_hash,
                    state_block=state_block,
                    silent=silent,
                )
            if issubclass(pool_class, CamelotLiquidityPool):
                return self._camelot_builder.build(
                    pool_address,
                    chain_id=chain_id,
                    deployer_address=deployer_address,
                    init_hash=init_hash,
                    state_block=state_block,
                    silent=silent,
                )

        return self._v2_builder.build(
            pool_address,
            chain_id=chain_id,
            deployer_address=deployer_address,
            init_hash=init_hash,
            state_block=state_block,
            silent=silent,
        )

    def get_token_balance(
        self,
        token: Erc20Token,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the ERC-20 balance for the given address."""
        return self._erc20_builder.get_token_balance(
            token, address, block_identifier=block_identifier
        )

    def get_token_approval(
        self,
        token: Erc20Token,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the amount that can be spent by `spender` on behalf of `owner`."""
        return self._erc20_builder.get_token_approval(
            token, owner, spender, block_identifier=block_identifier
        )

    def get_token_total_supply(
        self,
        token: Erc20Token,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the total supply for this token."""
        return self._erc20_builder.get_token_total_supply(token, block_identifier=block_identifier)

    def get_ether_balance(
        self,
        chain_id: ChainId,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the native ETH balance for the given address."""
        return self._erc20_builder.get_ether_balance(
            chain_id, address, block_identifier=block_identifier
        )

    def build_v3_pool(
        self,
        pool_address: str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        state_block: int | None = None,
        tick_bitmap: dict[int, UniswapV3BitmapAtWord] | None = None,
        tick_data: dict[int, UniswapV3LiquidityAtTick] | None = None,
        silent: bool = False,
    ) -> ConcentratedLiquidityPool:
        """.. deprecated:: 0.x
        Use ``build_pool(address)`` instead. Type resolution automatically
        selects the correct builder.
        """
        warnings.warn(
            "build_v3_pool() is deprecated — use build_pool(address) instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        return self._v3_builder.build(
            pool_address,
            chain_id=chain_id,
            deployer_address=deployer_address,
            init_hash=init_hash,
            state_block=state_block,
            tick_bitmap=tick_bitmap,
            tick_data=tick_data,
            silent=silent,
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
        tick_bitmap: dict[int, UniswapV4BitmapAtWord] | None = None,
        tick_data: dict[int, UniswapV4LiquidityAtTick] | None = None,
        silent: bool = False,
    ) -> UniswapV4Pool:
        """.. deprecated:: 0.x
        Use ``build_pool(address, pool_id=...)`` instead.
        """
        warnings.warn(
            "build_v4_pool() is deprecated — use build_pool(address, pool_id=...) instead.",
            DeprecationWarning,
            stacklevel=2,
        )
        return self._v4_builder.build(
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
        )

    def build_curve_pool(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        state_block: int | None = None,
        silent: bool = False,
        state_cache_depth: int = 8,
    ) -> CurveStableswapPool:
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
        )

    def get_provider(self, *, chain_id: ChainId) -> Any:
        return self.connections.get_provider(chain_id)

    def get_web3(self, *, chain_id: ChainId) -> Any:
        """.. deprecated:: 0.x
        Use ``get_provider(chain_id)`` instead.
        """
        return self.connections.get_web3(chain_id)

    def update(self, pool: Any, *, block_number: BlockIdentifier | None = None) -> bool:
        """
        Fetch the current state of a pool from the chain and apply it via
        ``pool.external_update()``.

        Returns True if the state changed, False if unchanged.
        """
        builder = self._builder_for_pool(pool)
        return builder.update(pool, block_number=block_number)

    def _builder_for_pool(
        self,
        pool: Any,
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
