import contextlib
from threading import Lock
from typing import TYPE_CHECKING, Any, Self

from eth_typing import ChecksumAddress

from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import connection_manager
from degenbot.exceptions.liquidity_pool import LiquidityPoolError
from degenbot.exceptions.manager import (
    ManagerAlreadyInitialized,
    PoolCreationFailed,
    PoolNotAssociated,
)
from degenbot.logging import logger
from degenbot.registry import pool_registry
from degenbot.types.abstract import AbstractPoolManager
from degenbot.types.aliases import ChainId
from degenbot.uniswap.deployments import (
    FACTORY_DEPLOYMENTS,
    UniswapV2ExchangeDeployment,
    UniswapV3ExchangeDeployment,
)
from degenbot.uniswap.v2_functions import generate_v2_pool_address
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_functions import generate_v3_pool_address
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_snapshot import UniswapV3LiquiditySnapshot


class AbstractUniswapV2PoolManager[Pool: UniswapV2Pool](AbstractPoolManager[Pool]):
    def __init__(
        self,
        factory_address: str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: ChecksumAddress | str | None = None,
        pool_init_hash: str | None = None,
    ) -> None:
        factory_address = get_checksum_address(factory_address)

        if chain_id is None:
            chain_id = connection_manager.default_chain_id

        if (chain_id, factory_address) in self.instances:
            raise ManagerAlreadyInitialized(
                message="A manager has already been initialized for this address. Access it using the get_instance() class method"  # noqa:E501
            )
        self.instances[(chain_id, factory_address)] = self

        try:
            factory_deployment = FACTORY_DEPLOYMENTS[chain_id][factory_address]
            deployer_address = (
                factory_deployment.deployer
                if factory_deployment.deployer is not None
                else factory_address
            )
            pool_init_hash = factory_deployment.pool_init_hash
        except KeyError:
            if pool_init_hash is None:  # pragma: no branch
                logger.info("Pool init hash is unknown. Using Uniswap V3 mainnet default.")
                pool_init_hash = UniswapV2Pool.UNISWAP_V2_MAINNET_POOL_INIT_HASH
            deployer_address = (
                get_checksum_address(deployer_address)
                if deployer_address is not None
                else factory_address
            )

        self._lock = Lock()
        self._chain_id = chain_id
        self._deployer_address = deployer_address
        self._factory_address = factory_address
        self._pool_init_hash = pool_init_hash
        self._tracked_pools = {}
        self._untracked_pools: set[ChecksumAddress] = set()

    def get_pool(
        self,
        pool_address: ChecksumAddress | str,
        *,
        silent: bool = False,
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> Pool:
        """
        Get a pool from its address. If the pool is already tracked or found in the global registry,
        that instance will be returned. Otherwise, a new one will be built.
        """

        pool_address = get_checksum_address(pool_address)

        with contextlib.suppress(KeyError):
            return self._tracked_pools[pool_address]

        if pool_address in self._untracked_pools:
            raise PoolNotAssociated(pool_address)

        # Check if the pool registry already has this pool
        if (
            pool_from_registry := pool_registry.get(
                pool_address=pool_address,
                chain_id=self.chain_id,
            )
        ) is not None:
            if TYPE_CHECKING:
                assert isinstance(pool_from_registry, self.pool_factory)
            if pool_from_registry.factory == self._factory_address:
                self._add_tracked_pool(pool_from_registry)
                return pool_from_registry
            self._untracked_pools.add(pool_address)
            raise PoolNotAssociated(pool_address)

        try:
            new_pool = self.pool_factory(
                address=pool_address,
                silent=silent,
                **(pool_class_kwargs or {}),
            )
        except LiquidityPoolError as exc:
            raise PoolCreationFailed(
                message=f"Could not build V2 pool {pool_address}: {exc}"
            ) from exc
        else:
            self._add_tracked_pool(new_pool)
            return new_pool


class UniswapV2PoolManager(AbstractUniswapV2PoolManager[UniswapV2Pool], pool_factory=UniswapV2Pool):
    """
    A class that generates and tracks concrete instances of a Uniswap V2 liquidity pool helper or
    one of its child classes.
    """

    @classmethod
    def from_exchange(
        cls,
        exchange: UniswapV2ExchangeDeployment,
    ) -> Self:
        return cls(
            factory_address=exchange.factory.address,
            deployer_address=exchange.factory.deployer,
            pool_init_hash=exchange.factory.pool_init_hash,
        )

    def __repr__(self) -> str:  # pragma: no cover
        return f"{self.__class__.__name__}(factory={self._factory_address})"

    @property
    def pool_init_hash(self) -> str:
        return self._pool_init_hash

    def get_pool_from_tokens(
        self,
        token_addresses: tuple[str, str],
        *,
        silent: bool = False,
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> UniswapV2Pool:
        """
        Get a pool by its token addresses
        """

        pool_address = generate_v2_pool_address(
            deployer_address=self._deployer_address,
            token_addresses=token_addresses,
            init_hash=self._pool_init_hash,
        )

        return self.get_pool(
            pool_address=pool_address,
            silent=silent,
            pool_class_kwargs=pool_class_kwargs,
        )


class AbstractUniswapV3PoolManager[Pool: UniswapV3Pool](AbstractPoolManager[Pool]):
    _chain_id: ChainId
    _deployer_address: ChecksumAddress
    _factory_address: ChecksumAddress
    _tracked_pools: dict[ChecksumAddress, Pool]
    _untracked_pools: set[ChecksumAddress]
    _lock: Lock
    _pool_init_hash: str
    _snapshot: UniswapV3LiquiditySnapshot | None

    def __init__(
        self,
        factory_address: ChecksumAddress | str,
        deployer_address: ChecksumAddress | str | None = None,
        chain_id: ChainId | None = None,
        pool_init_hash: str | None = None,
        snapshot: UniswapV3LiquiditySnapshot | None = None,
    ) -> None:
        if chain_id is None:
            chain_id = connection_manager.default_chain_id

        factory_address = get_checksum_address(factory_address)

        if (chain_id, factory_address) in self.instances:
            raise ManagerAlreadyInitialized(
                message="A manager has already been initialized for this address. Access it using the get_instance() class method"  # noqa:E501
            )
        self.instances[(chain_id, factory_address)] = self

        try:
            factory_deployment = FACTORY_DEPLOYMENTS[chain_id][factory_address]
            deployer_address = (
                factory_deployment.deployer
                if factory_deployment.deployer is not None
                else factory_address
            )
            pool_init_hash = factory_deployment.pool_init_hash
        except KeyError:
            if pool_init_hash is None:  # pragma: no branch
                logger.info("Pool init hash is unknown. Using Uniswap V3 mainnet default.")
                pool_init_hash = UniswapV3Pool.UNISWAP_V3_MAINNET_POOL_INIT_HASH
            deployer_address = (
                get_checksum_address(deployer_address)
                if deployer_address is not None
                else factory_address
            )

        self._lock = Lock()
        self._chain_id = chain_id
        self._factory_address = factory_address
        self._deployer_address = deployer_address
        self._pool_init_hash = pool_init_hash
        self._snapshot = snapshot
        self._tracked_pools = {}
        self._untracked_pools = set()

    def _apply_pending_liquidity_updates(self, pool: UniswapV3Pool) -> None:
        """
        Apply all pending updates from the snapshot.
        """

        if self._snapshot is not None:
            for liquidity_update in self._snapshot.pending_updates(pool.address):
                pool.update_liquidity_map(liquidity_update)

    def _build_pool(
        self,
        pool_address: ChecksumAddress,
        *,
        silent: bool,
        pool_class_kwargs: dict[str, Any] | None,
    ) -> Pool:
        if pool_class_kwargs is None:
            pool_class_kwargs = {}

        if self._snapshot is not None:
            pool = self.pool_factory(
                address=pool_address,
                tick_bitmap=self._snapshot.tick_bitmap(pool_address),
                tick_data=self._snapshot.tick_data(pool_address),
                silent=silent,
                **pool_class_kwargs,
            )
        else:
            logger.info("Initializing pool without liquidity snapshot")
            pool = self.pool_factory(
                address=pool_address,
                silent=silent,
                **pool_class_kwargs,
            )
        return pool

    def get_pool(
        self,
        pool_address: ChecksumAddress | str,
        *,
        silent: bool = False,
        # keyword arguments passed to the pool class constructor
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> Pool:
        """
        Get a pool from its address. If the pool is already tracked or found in the global registry,
        that instance will be returned. Otherwise, a new one will be built.
        """

        pool_address = get_checksum_address(pool_address)

        with contextlib.suppress(KeyError):
            return self._tracked_pools[pool_address]

        if pool_address in self._untracked_pools:
            raise PoolNotAssociated(pool_address)

        # Check if the pool registry already has this pool
        pool_from_registry = pool_registry.get(
            pool_address=pool_address,
            chain_id=self.chain_id,
        )
        if pool_from_registry is not None:
            if TYPE_CHECKING:
                assert isinstance(pool_from_registry, self.pool_factory)
            if pool_from_registry.factory == self._factory_address:
                self._add_tracked_pool(pool_from_registry)
                return pool_from_registry
            self._untracked_pools.add(pool_address)
            raise PoolNotAssociated(pool_address)

        try:
            new_pool = self._build_pool(
                pool_address=pool_address,
                silent=silent,
                pool_class_kwargs=pool_class_kwargs,
            )
        except LiquidityPoolError as exc:  # pragma: no cover
            raise PoolCreationFailed(
                message=f"Could not build V3 pool {pool_address}: {exc}"
            ) from exc
        else:
            self._apply_pending_liquidity_updates(new_pool)
            self._add_tracked_pool(new_pool)
            return new_pool

    def unload_snapshot(self) -> None:
        self._snapshot = None


class UniswapV3PoolManager(AbstractUniswapV3PoolManager[UniswapV3Pool], pool_factory=UniswapV3Pool):
    """
    A class that generates and tracks concrete instances of a Uniswap V3 liquidity pool helper or
    one of its child classes.
    """

    @classmethod
    def from_exchange(
        cls,
        exchange: UniswapV3ExchangeDeployment,
        snapshot: UniswapV3LiquiditySnapshot | None = None,
    ) -> Self:
        return cls(
            factory_address=exchange.factory.address,
            deployer_address=exchange.factory.deployer,
            chain_id=exchange.chain_id,
            snapshot=snapshot,
        )

    def __repr__(self) -> str:  # pragma: no cover
        return f"{self.__class__.__name__}(factory={self._factory_address})"

    def _build_pool(
        self,
        pool_address: ChecksumAddress,
        *,
        silent: bool,
        pool_class_kwargs: dict[str, Any] | None,
    ) -> UniswapV3Pool:
        if pool_class_kwargs is None:
            pool_class_kwargs = {}

        if self._snapshot is not None:
            return UniswapV3Pool(
                address=pool_address,
                tick_bitmap=self._snapshot.tick_bitmap(pool_address),
                tick_data=self._snapshot.tick_data(pool_address),
                silent=silent,
                **pool_class_kwargs,
            )

        logger.info("Initializing pool without liquidity snapshot")
        return UniswapV3Pool(
            address=pool_address,
            silent=silent,
            **pool_class_kwargs,
        )

    def get_pool(
        self,
        pool_address: ChecksumAddress | str,
        *,
        silent: bool = False,
        # keyword arguments passed to the pool class constructor
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> UniswapV3Pool:
        """
        Get a pool from its address. If the pool is already tracked or found in the global registry,
        that instance will be returned. Otherwise, a new one will be built.
        """

        pool_address = get_checksum_address(pool_address)

        with contextlib.suppress(KeyError):
            return self._tracked_pools[pool_address]

        if pool_address in self._untracked_pools:
            raise PoolNotAssociated(pool_address)

        # Check if the pool registry already has this pool
        pool_from_registry = pool_registry.get(
            pool_address=pool_address,
            chain_id=self.chain_id,
        )
        if pool_from_registry is not None:
            if TYPE_CHECKING:
                assert isinstance(pool_from_registry, UniswapV3Pool)
            if pool_from_registry.factory == self._factory_address:
                self._add_tracked_pool(pool_from_registry)
                return pool_from_registry
            self._untracked_pools.add(pool_address)
            raise PoolNotAssociated(pool_address)

        try:
            new_pool = self._build_pool(
                pool_address=pool_address,
                silent=silent,
                pool_class_kwargs=pool_class_kwargs,
            )
        except LiquidityPoolError as exc:  # pragma: no cover
            raise PoolCreationFailed(
                message=f"Could not build V3 pool {pool_address}: {exc}"
            ) from exc
        else:
            self._apply_pending_liquidity_updates(new_pool)
            self._add_tracked_pool(new_pool)
            return new_pool

    def get_pool_from_tokens_and_fee(
        self,
        token_addresses: tuple[
            ChecksumAddress | str,
            ChecksumAddress | str,
        ],
        pool_fee: int,
        *,
        silent: bool = False,
        # keyword arguments passed to the pool class constructor
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> UniswapV3Pool:
        """
        Get a pool by its token addresses
        """

        pool_address = generate_v3_pool_address(
            token_addresses=sorted(token_addresses),
            fee=pool_fee,
            deployer_address=self._deployer_address,
            init_hash=self._pool_init_hash,
        )

        return self.get_pool(
            pool_address=pool_address,
            silent=silent,
            pool_class_kwargs=pool_class_kwargs,
        )
