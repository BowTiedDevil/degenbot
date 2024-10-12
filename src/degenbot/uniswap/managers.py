import contextlib
from threading import Lock
from typing import Any

from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address
from typing_extensions import Self

from ..config import web3_connection_manager
from ..exceptions import (
    AddressMismatch,
    LiquidityPoolError,
    ManagerAlreadyInitialized,
    ManagerError,
    PoolNotAssociated,
)
from ..logging import logger
from ..registry.all_pools import pool_registry
from ..types import AbstractLiquidityPool, AbstractPoolManager
from ..uniswap.deployments import UniswapV2ExchangeDeployment, UniswapV3ExchangeDeployment
from ..uniswap.v2_functions import generate_v2_pool_address
from .deployments import FACTORY_DEPLOYMENTS
from .v3_functions import generate_v3_pool_address
from .v3_snapshot import UniswapV3LiquiditySnapshot


class UniswapV2PoolManager(AbstractPoolManager):
    """
    A class that generates and tracks Uniswap V2 liquidity pool helpers.
    """

    from .v2_liquidity_pool import UniswapV2Pool as Pool

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

    def __init__(
        self,
        factory_address: str,
        *,
        chain_id: int | None = None,
        deployer_address: ChecksumAddress | str | None = None,
        pool_init_hash: str | None = None,
    ):
        factory_address = to_checksum_address(factory_address)

        if chain_id is None:
            chain_id = web3_connection_manager.default_chain_id

        if (chain_id, factory_address) in self.instances:
            raise ManagerAlreadyInitialized(
                "A manager has already been initialized for this address. Access it using the get_instance() class method"  # noqa:E501
            )
        else:
            self.instances[(chain_id, factory_address)] = self  # type:ignore[assignment]

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
                to_checksum_address(deployer_address)
                if deployer_address is not None
                else factory_address
            )

        self._lock = Lock()
        self._chain_id = chain_id
        self._factory_address = factory_address
        self._deployer_address = deployer_address
        self._pool_init_hash = pool_init_hash
        self._tracked_pools: dict[ChecksumAddress, AbstractLiquidityPool] = dict()
        self._untracked_pools: set[ChecksumAddress] = set()

    def __repr__(self) -> str:  # pragma: no cover
        return f"UniswapV2PoolManager(factory={self._factory_address})"

    def _add_tracked_pool(self, pool_helper: Pool) -> None:
        with self._lock:
            self._tracked_pools[pool_helper.address] = pool_helper

    @property
    def chain_id(self) -> int:
        return self._chain_id

    def _build_pool(
        self,
        pool_address: ChecksumAddress,
        silent: bool,
        state_block: int | None,
        pool_class_kwargs: dict[str, Any] | None,
    ) -> Pool:
        if pool_class_kwargs is None:
            pool_class_kwargs = dict()

        return self.Pool(
            address=pool_address,
            silent=silent,
            state_block=state_block,
            **pool_class_kwargs,
        )

    def get_pool(
        self,
        pool_address: ChecksumAddress | str,
        *,
        silent: bool = False,
        state_block: int | None = None,
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> Pool:
        """
        Get a pool from its address
        """

        pool_address = to_checksum_address(pool_address)

        with contextlib.suppress(KeyError):
            result = self._tracked_pools[pool_address]
            assert isinstance(result, self.Pool)
            return result

        if pool_address in self._untracked_pools:
            raise PoolNotAssociated(
                f"Pool address {pool_address} not associated with factory {self._factory_address}"
            )

        # Check if the pool registry already has this pool
        if (
            pool_from_registry := pool_registry.get(
                pool_address=pool_address,
                chain_id=self._chain_id,
            )
        ) is not None:
            assert isinstance(pool_from_registry, self.Pool)
            if pool_from_registry.factory == self._factory_address:
                self._add_tracked_pool(pool_from_registry)
                return pool_from_registry
            else:
                self._untracked_pools.add(pool_address)
                raise PoolNotAssociated(f"Pool {pool_address} is not associated with this DEX")

        try:
            new_pool = self._build_pool(
                pool_address=pool_address,
                silent=silent,
                state_block=state_block,
                pool_class_kwargs=pool_class_kwargs,
            )
        except AddressMismatch:
            self._untracked_pools.add(pool_address)
            raise PoolNotAssociated from None
        except LiquidityPoolError as exc:
            self._untracked_pools.add(pool_address)
            raise ManagerError(f"Could not build V2 pool {pool_address}: {exc}") from exc
        else:
            self._add_tracked_pool(new_pool)
            return new_pool

    def get_pool_from_tokens(
        self,
        token_addresses: tuple[str, str],
        *,
        silent: bool = False,
        state_block: int | None = None,
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> Pool:
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
            state_block=state_block,
            pool_class_kwargs=pool_class_kwargs,
        )


class UniswapV3PoolManager(AbstractPoolManager):
    """
    A class that generates and tracks Uniswap V3 liquidity pool helpers.
    """

    from .v3_liquidity_pool import UniswapV3Pool as Pool

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

    def __init__(
        self,
        factory_address: ChecksumAddress | str,
        deployer_address: ChecksumAddress | str | None = None,
        chain_id: int | None = None,
        pool_init_hash: str | None = None,
        snapshot: UniswapV3LiquiditySnapshot | None = None,
    ):
        if chain_id is None:
            chain_id = web3_connection_manager.default_chain_id

        factory_address = to_checksum_address(factory_address)

        if (chain_id, factory_address) in self.instances:
            raise ManagerAlreadyInitialized(
                "A manager has already been initialized for this address. Access it using the get_instance() class method"  # noqa:E501
            )
        else:
            self.instances[(chain_id, factory_address)] = self  # type:ignore[assignment]

        try:
            factory_deployment = FACTORY_DEPLOYMENTS[chain_id][factory_address]
            deployer_address = (
                factory_deployment.deployer
                if factory_deployment.deployer is not None
                else factory_address
            )
            pool_init_hash = factory_deployment.pool_init_hash
        except KeyError:
            if pool_init_hash is None:
                raise ManagerError(
                    "Cannot create UniswapV3 pool manager without factory address and pool init hash."  # noqa:E501
                ) from None
            deployer_address = (
                to_checksum_address(deployer_address)
                if deployer_address is not None
                else factory_address
            )

        self._lock = Lock()
        self._chain_id = chain_id
        self._factory_address = factory_address
        self._deployer_address = deployer_address
        self._pool_init_hash = pool_init_hash
        self._snapshot = snapshot
        self._tracked_pools: dict[ChecksumAddress, AbstractLiquidityPool] = dict()
        self._untracked_pools: set[ChecksumAddress] = set()

    def __repr__(self) -> str:  # pragma: no cover
        return f"UniswapV3PoolManager(factory={self._factory_address})"

    def _add_tracked_pool(self, pool_helper: Pool) -> None:
        with self._lock:
            self._tracked_pools[pool_helper.address] = pool_helper

    def _apply_pending_liquidity_updates(self, pool: Pool) -> None:
        """
        Apply all pending updates from the snapshot.
        """

        if not self._snapshot:
            return

        starting_state_block = pool.update_block

        # Apply liquidity modifications
        for i, liquidity_update in enumerate(
            self._snapshot.get_new_liquidity_updates(pool.address)
        ):
            if i == 0:
                pool._update_block = liquidity_update.block_number
            pool.external_update(liquidity_update)

        # Restore the slot0 values state at the original creation block
        pool.auto_update(block_number=starting_state_block)

    def _build_pool(
        self,
        pool_address: ChecksumAddress,
        silent: bool,
        state_block: int | None,
        pool_class_kwargs: dict[str, Any] | None,
    ) -> Pool:
        if pool_class_kwargs is None:
            pool_class_kwargs = dict()

        if self._snapshot is not None:
            pool_class_kwargs.update(
                {
                    "tick_bitmap": self._snapshot.get_tick_bitmap(pool_address),
                    "tick_data": self._snapshot.get_tick_data(pool_address),
                }
            )
        else:
            logger.debug("Initializing pool without liquidity snapshot")

        return self.Pool(
            address=pool_address,
            silent=silent,
            state_block=state_block,
            **pool_class_kwargs,
        )

    def get_pool(
        self,
        pool_address: ChecksumAddress | str,
        *,
        silent: bool = False,
        state_block: int | None = None,
        # keyword arguments passed to the pool class constructor
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> Pool:
        """
        Get a pool from its address
        """

        pool_address = to_checksum_address(pool_address)

        with contextlib.suppress(KeyError):
            result = self._tracked_pools[pool_address]
            assert isinstance(result, self.Pool)
            return result

        if pool_address in self._untracked_pools:
            raise PoolNotAssociated(
                f"Pool address {pool_address} not associated with factory {self._factory_address}"
            )

        # Check if the pool registry already has this pool
        if (
            pool_from_registry := pool_registry.get(
                pool_address=pool_address,
                chain_id=self._chain_id,
            )
        ) is not None:
            assert isinstance(pool_from_registry, self.Pool)
            if pool_from_registry.factory == self._factory_address:
                self._add_tracked_pool(pool_from_registry)
                return pool_from_registry
            else:
                self._untracked_pools.add(pool_address)
                raise PoolNotAssociated(f"Pool {pool_address} is not associated with this DEX")

        try:
            new_pool = self._build_pool(
                pool_address=to_checksum_address(pool_address),
                silent=silent,
                state_block=state_block,
                pool_class_kwargs=pool_class_kwargs,
            )
        except AddressMismatch:
            self._untracked_pools.add(pool_address)
            raise PoolNotAssociated from None
        except LiquidityPoolError as exc:
            self._untracked_pools.add(pool_address)
            raise ManagerError(f"Could not build V3 pool {pool_address}: {exc}") from exc
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
        state_block: int | None = None,
        # keyword arguments passed to the pool class constructor
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> Pool:
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
            state_block=state_block,
            pool_class_kwargs=pool_class_kwargs,
        )
