from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, Any

import eth_abi.abi
from alembic.runtime.migration import MigrationContext
from alembic.script import ScriptDirectory
from sqlalchemy import select

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.builders.curve_pool_builder import CurvePoolBuilder
from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.builders.v2_pool_builder import V2PoolBuilder
from degenbot.builders.v3_pool_builder import V3PoolBuilder
from degenbot.builders.v4_pool_builder import V4PoolBuilder
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DegenbotConfig, _init_config
from degenbot.connection.connection_manager import ConnectionManager
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.database.models.pools import LiquidityPoolTable
from degenbot.database.operations import get_alembic_config, get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.erc20.erc20 import (
    Erc20Token,
)
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.manager import ManagerAlreadyInitialized
from degenbot.functions import encode_function_calldata
from degenbot.logging import logger
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.pool_type import (
    PoolFamily,
    PoolTypeDescriptor,
    derive_kind,
)
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
)
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool
from degenbot.uniswap.v4_types import (
    UniswapV4BitmapAtWord,
    UniswapV4LiquidityAtTick,
)
from degenbot.version import __version__

if TYPE_CHECKING:
    from collections.abc import Sequence

    from eth_typing import ChecksumAddress
    from web3.types import BlockIdentifier

    from degenbot.types.abstract.pool_manager import AbstractPoolManager
    from degenbot.types.aliases import ChainId


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
        self._managers: dict[tuple[ChainId, str], AbstractPoolManager] = {}

        # Builders own I/O orchestration; Bot hands them its I/O dependencies
        self._erc20_builder = Erc20Builder(
            connections=self.connections, db=self.db, tokens=self.tokens
        )
        self._v2_builder = V2PoolBuilder(
            connections=self.connections, db=self.db, pools=self.pools, tokens=self.tokens,
            erc20_builder=self._erc20_builder,
        )
        self._v3_builder = V3PoolBuilder(
            connections=self.connections, db=self.db, pools=self.pools, tokens=self.tokens,
            managed_pools=self.managed_pools, erc20_builder=self._erc20_builder,
        )
        self._v4_builder = V4PoolBuilder(
            connections=self.connections, db=self.db, pools=self.pools, tokens=self.tokens,
            managed_pools=self.managed_pools, erc20_builder=self._erc20_builder,
        )
        self._curve_builder = CurvePoolBuilder(
            connections=self.connections, db=self.db, pools=self.pools, tokens=self.tokens,
            erc20_builder=self._erc20_builder,
        )

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

    def add_manager[M: AbstractPoolManager](
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
        if key in self._managers:
            raise ManagerAlreadyInitialized(
                message="A manager has already been initialized for this address. "
                "Access it using the bot's manager registry."
            )

        manager = manager_cls(
            factory_address=factory_address,
            chain_id=chain_id,
            bot=self,
            **kwargs,
        )
        self._managers[key] = manager
        return manager

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
            return self.build_v4_pool(
                pool_id=pool_id,
                pool_manager_address=address,
                chain_id=chain_id,
                state_block=state_block,
                silent=silent,
            )

        # Check pool registry — return existing pool if already built
        existing = self.pools.get(chain_id=chain_id, pool_address=address)
        if existing is not None:
            return existing

        # Resolve the pool type and dispatch
        pool_type = self._resolve_pool_type(address, chain_id=chain_id)

        match pool_type.family:
            case PoolFamily.CONSTANT_PRODUCT:
                return self.build_v2_pool(
                    address,
                    chain_id=chain_id,
                    deployer_address=deployer_address,
                    init_hash=init_hash,
                    state_block=state_block,
                    silent=silent,
                )
            case PoolFamily.CONCENTRATED_LIQUIDITY:
                return self.build_v3_pool(
                    address,
                    chain_id=chain_id,
                    deployer_address=deployer_address,
                    init_hash=init_hash,
                    state_block=state_block,
                    tick_bitmap=tick_bitmap,
                    tick_data=tick_data,
                    silent=silent,
                )
            case PoolFamily.STABLESWAP:
                return self.build_curve_pool(
                    address,
                    chain_id=chain_id,
                    state_block=state_block,
                    silent=silent,
                )
            case _:
                raise DegenbotValueError(
                    message=f"No builder for pool family {pool_type.family.value!r}"
                )

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
            f"Specify the pool type explicitly via build_v2_pool, build_v3_pool, or build_curve_pool."
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
    ) -> UniswapV2Pool:  # type: ignore[name-defined]
        """Fetch pool data from DB/RPC and construct an I/O-free UniswapV2Pool."""
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
        return self._erc20_builder.get_token_balance(token, address, block_identifier=block_identifier)

    def get_token_approval(
        self,
        token: Erc20Token,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the amount that can be spent by `spender` on behalf of `owner`."""
        return self._erc20_builder.get_token_approval(token, owner, spender, block_identifier=block_identifier)

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
        return self._erc20_builder.get_ether_balance(chain_id, address, block_identifier=block_identifier)

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
    ) -> UniswapV3Pool:
        """Fetch pool data from DB/RPC and construct an I/O-free UniswapV3Pool."""
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
        """Fetch pool data from DB/RPC and construct an I/O-free UniswapV4Pool."""
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
        """Fetch pool data from RPC and construct an I/O-free CurveStableswapPool."""
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

    def _builder_for_pool(self, pool: Any) -> V2PoolBuilder | V3PoolBuilder | V4PoolBuilder | CurvePoolBuilder:
        """Select the appropriate builder for the pool type."""
        if isinstance(pool, (UniswapV2Pool, AerodromeV2Pool)):
            return self._v2_builder
        if isinstance(pool, UniswapV3Pool) and not isinstance(pool, UniswapV4Pool):
            return self._v3_builder
        if isinstance(pool, UniswapV4Pool):
            return self._v4_builder
        if isinstance(pool, CurveStableswapPool):
            return self._curve_builder
        msg = f"update() not implemented for pool type {type(pool).__name__}"
        raise TypeError(msg)
