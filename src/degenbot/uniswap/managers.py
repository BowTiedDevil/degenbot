import contextlib
from threading import Lock
from typing import TYPE_CHECKING, Any, cast

from eth_typing import BlockIdentifier, ChecksumAddress
from eth_utils.address import to_checksum_address
from typing_extensions import Self
from web3 import Web3
from web3.exceptions import ContractLogicError

from ..config import web3_connection_manager
from ..exceptions import (
    AddressMismatch,
    DegenbotError,
    ManagerAlreadyInitialized,
    ManagerError,
    PoolNotAssociated,
)
from ..functions import encode_function_calldata, get_number_for_block_identifier, raw_call
from ..logging import logger
from ..registry.all_pools import pool_registry
from ..types import AbstractLiquidityPool, AbstractPoolManager
from ..uniswap.deployments import UniswapV2ExchangeDeployment, UniswapV3ExchangeDeployment
from .deployments import FACTORY_DEPLOYMENTS
from .v2_liquidity_pool import UniswapV2Pool
from .v3_functions import generate_v3_pool_address
from .v3_liquidity_pool import UniswapV3Pool
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
                raise ManagerError(
                    "Cannot create UniswapV2 pool manager without factory address and pool init hash."  # noqa:E501
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
        self._tracked_pools: dict[ChecksumAddress, AbstractLiquidityPool] = dict()
        self._untracked_pools: set[ChecksumAddress] = set()

    def __delitem__(self, pool: AbstractLiquidityPool | ChecksumAddress | str) -> None:
        pool_address: ChecksumAddress

        if isinstance(pool, AbstractLiquidityPool):
            pool_address = pool.address
        else:
            pool_address = to_checksum_address(pool)

        with contextlib.suppress(KeyError):
            del self._tracked_pools[pool_address]

        self._untracked_pools.discard(pool_address)

    def __repr__(self) -> str:  # pragma: no cover
        return f"UniswapV2PoolManager(factory={self._factory_address})"

    def _add_tracked_pool(self, pool_helper: AbstractLiquidityPool) -> None:
        with self._lock:
            self._tracked_pools[pool_helper.address] = pool_helper

    @property
    def chain_id(self) -> int:
        return self._chain_id

    def get_pair_from_factory(
        self,
        w3: Web3,
        token0: ChecksumAddress,
        token1: ChecksumAddress,
        block_identifier: BlockIdentifier | None = None,
    ) -> str:
        pool_address, *_ = raw_call(
            w3=w3,
            address=self._factory_address,
            calldata=encode_function_calldata(
                function_prototype="getPair(address,address)",
                function_arguments=[token0, token1],
            ),
            return_types=["address"],
            block_identifier=get_number_for_block_identifier(block_identifier, w3),
        )
        return cast(str, pool_address)

    def get_pool(
        self,
        pool_address: ChecksumAddress | str,
        silent: bool = False,
        state_block: int | None = None,
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> AbstractLiquidityPool:
        """
        Get a pool from its address
        """
        return self._build_pool(
            pool_address=to_checksum_address(pool_address),
            silent=silent,
            state_block=state_block,
            pool_class_kwargs=pool_class_kwargs,
        )

    def get_pool_from_tokens(
        self,
        token_addresses: tuple[str, str],
        silent: bool = False,
        state_block: int | None = None,
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> AbstractLiquidityPool:
        """
        Get a pool by its token addresses
        """
        pool = self._build_pool(
            pool_address=to_checksum_address(
                self.get_pair_from_factory(
                    w3=web3_connection_manager.get_web3(self.chain_id),
                    token0=to_checksum_address(token_addresses[0]),
                    token1=to_checksum_address(token_addresses[1]),
                    block_identifier=None,
                )
            ),
            silent=silent,
            state_block=state_block,
            pool_class_kwargs=pool_class_kwargs,
        )
        assert isinstance(pool, UniswapV2Pool)
        return pool

    def _build_pool(
        self,
        pool_address: ChecksumAddress,
        silent: bool,
        state_block: int | None,
        pool_class_kwargs: dict[str, Any] | None,
    ) -> AbstractLiquidityPool:
        with contextlib.suppress(KeyError):
            result = self._tracked_pools[pool_address]
            if TYPE_CHECKING:
                assert isinstance(result, UniswapV2Pool)
            return result

        if pool_address in self._untracked_pools:
            raise PoolNotAssociated(
                f"Pool address {pool_address} not associated with factory {self._factory_address}"
            )

        # Check if the pool registry already has this pool
        if (
            known_pool_helper := pool_registry.get(
                pool_address=pool_address, chain_id=self._chain_id
            )
        ) is not None:
            if TYPE_CHECKING:
                assert isinstance(known_pool_helper, UniswapV2Pool)
            if known_pool_helper.factory == self._factory_address:
                self._add_tracked_pool(known_pool_helper)
                return known_pool_helper
            else:
                self._untracked_pools.add(pool_address)
                raise PoolNotAssociated(f"Pool {pool_address} is not associated with this DEX")

        if pool_class_kwargs is None:
            pool_class_kwargs = dict()

        try:
            new_pool_helper = self.Pool(
                address=pool_address,
                silent=silent,
                state_block=state_block,
                **pool_class_kwargs,
            )
        except AddressMismatch:
            self._untracked_pools.add(pool_address)
            raise PoolNotAssociated from None
        except (DegenbotError, ContractLogicError) as exc:
            raise ManagerError(f"Could not build V2 pool {pool_address}: {exc}") from exc
        else:
            self._add_tracked_pool(new_pool_helper)
            return new_pool_helper


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

    def __delitem__(self, pool: AbstractLiquidityPool | ChecksumAddress | str) -> None:
        pool_address: ChecksumAddress

        if isinstance(pool, AbstractLiquidityPool):
            pool_address = pool.address
        else:
            pool_address = to_checksum_address(pool)

        with contextlib.suppress(KeyError):
            del self._tracked_pools[pool_address]

        self._untracked_pools.discard(pool_address)

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

    def get_pool(
        self,
        pool_address: ChecksumAddress | str,
        silent: bool = False,
        state_block: int | None = None,
        # keyword arguments passed to the pool class constructor
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> UniswapV3Pool:
        return self._build_pool(
            pool_address=to_checksum_address(pool_address),
            silent=silent,
            state_block=state_block,
            pool_class_kwargs=pool_class_kwargs,
        )

    def get_pool_by_tokens_and_fee(
        self,
        token_addresses: tuple[
            ChecksumAddress | str,
            ChecksumAddress | str,
        ],
        pool_fee: int,
        silent: bool = False,
        state_block: int | None = None,
        # keyword arguments passed to the pool class constructor
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> UniswapV3Pool:
        return self.get_pool(
            pool_address=generate_v3_pool_address(
                token_addresses=sorted(token_addresses),
                fee=pool_fee,
                deployer_address=self._deployer_address,
                init_hash=self._pool_init_hash,
            ),
            silent=silent,
            state_block=state_block,
            pool_class_kwargs=pool_class_kwargs,
        )

    def _build_pool(
        self,
        pool_address: ChecksumAddress,
        silent: bool,
        state_block: int | None,
        pool_class_kwargs: dict[str, Any] | None,
    ) -> UniswapV3Pool:
        with contextlib.suppress(KeyError):
            result = self._tracked_pools[pool_address]
            if TYPE_CHECKING:
                assert isinstance(result, UniswapV3Pool)
            return result

        if pool_address in self._untracked_pools:
            raise PoolNotAssociated(
                f"Pool address {pool_address} not associated with factory {self._factory_address}"
            )

        # Check if the pool registry already has this pool
        if (
            known_pool_helper := pool_registry.get(
                pool_address=pool_address, chain_id=self._chain_id
            )
        ) is not None:
            if TYPE_CHECKING:
                assert isinstance(known_pool_helper, UniswapV3Pool)
            if known_pool_helper.factory == self._factory_address:
                self._add_tracked_pool(known_pool_helper)
                return known_pool_helper
            else:
                self._untracked_pools.add(pool_address)
                raise PoolNotAssociated(f"Pool {pool_address} is not associated with this DEX")

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
            logger.info(f"Initializing pool without liquidity snapshot, {self._factory_address=}")
            logger.info(f"{self._snapshot=}")

        # The pool is unknown, so build and add it
        try:
            new_pool_helper = self.Pool(
                address=pool_address,
                silent=silent,
                state_block=state_block,
                **pool_class_kwargs,
            )
        except AddressMismatch:
            self._untracked_pools.add(pool_address)
            raise PoolNotAssociated from None
        except (DegenbotError, ContractLogicError) as exc:
            raise ManagerError(f"Could not build V3 pool {pool_address}: {exc}") from exc
        else:
            self._apply_pending_liquidity_updates(new_pool_helper)
            self._add_tracked_pool(new_pool_helper)
            return new_pool_helper
