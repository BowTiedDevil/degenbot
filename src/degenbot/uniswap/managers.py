import contextlib
from threading import Lock
from typing import TYPE_CHECKING, Any, TypeAlias, cast

from eth_typing import BlockIdentifier, ChecksumAddress
from eth_utils.address import to_checksum_address
from typing_extensions import Self
from web3 import Web3

from .. import config
from ..constants import ZERO_ADDRESS
from ..exceptions import DegenbotError, Erc20TokenError, ManagerError, PoolNotAssociated
from ..functions import encode_function_calldata, get_number_for_block_identifier, raw_call
from ..logging import logger
from ..managers.erc20_token_manager import Erc20TokenHelperManager
from ..registry.all_pools import AllPools
from ..types import AbstractPoolManager
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

    from .v2_liquidity_pool import UniswapV2Pool as pool_creator

    PoolCreatorType: TypeAlias = pool_creator
    _tracked_pools: dict[ChecksumAddress, pool_creator]

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
        deployer_address: ChecksumAddress | str | None = None,
        chain_id: int | None = None,
        pool_init_hash: str | None = None,
    ):
        chain_id = chain_id if chain_id is not None else config.get_web3().eth.chain_id
        factory_address = to_checksum_address(factory_address)

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
        self._tracked_pools = dict()
        self._untracked_pools: set[ChecksumAddress] = set()

    def __delitem__(self, pool: PoolCreatorType | ChecksumAddress | str) -> None:
        pool_address: ChecksumAddress

        if isinstance(pool, UniswapV2Pool):
            pool_address = pool.address
        else:
            pool_address = to_checksum_address(pool)

        with contextlib.suppress(KeyError):
            del self._tracked_pools[pool_address]

        self._untracked_pools.discard(pool_address)
        assert pool_address not in self._untracked_pools

    def __repr__(self) -> str:  # pragma: no cover
        return f"UniswapV2PoolManager(factory={self._factory_address})"

    def _add_pool(self, pool_helper: PoolCreatorType) -> None:
        with self._lock:
            self._tracked_pools[pool_helper.address] = pool_helper

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
            block_identifier=get_number_for_block_identifier(block_identifier),
        )
        return cast(str, pool_address)

    def get_pool(
        self,
        pool_address: str | None = None,
        token_addresses: tuple[str, str] | None = None,
        silent: bool = False,
        state_block: int | None = None,
        liquiditypool_kwargs: dict[str, Any] | None = None,
    ) -> PoolCreatorType:
        """
        Get the pool object from its address, or a tuple of token addresses
        """

        if liquiditypool_kwargs is None:
            liquiditypool_kwargs = dict()

        if token_addresses is not None:
            checksummed_token_addresses = tuple(
                [to_checksum_address(token_address) for token_address in token_addresses]
            )
            token_manager = Erc20TokenHelperManager(chain_id=self._chain_id)

            try:
                for token_address in checksummed_token_addresses:
                    token_manager.get_erc20token(
                        address=token_address,
                        silent=silent,
                    )
            except Erc20TokenError:
                raise ManagerError("Could not get both Erc20Token helpers") from None

            pool_address = to_checksum_address(
                self.get_pair_from_factory(
                    w3=config.get_web3(),
                    token0=checksummed_token_addresses[0],
                    token1=checksummed_token_addresses[1],
                    block_identifier=None,
                )
            )
            if pool_address == ZERO_ADDRESS:
                raise ManagerError("No V2 LP available")

        if TYPE_CHECKING:
            assert pool_address is not None
        # Address is now known, check if the pool is already being tracked
        pool_address = to_checksum_address(pool_address)

        if pool_address in self._untracked_pools:
            raise PoolNotAssociated(
                f"Pool address {pool_address} not associated with factory {self._factory_address}"
            )

        try:
            return self._tracked_pools[pool_address]
        except KeyError:
            pass

        # Check if the AllPools collection already has this pool
        known_pool_helper = AllPools(self._chain_id).get(pool_address)
        if known_pool_helper is not None:
            if TYPE_CHECKING:
                assert isinstance(known_pool_helper, UniswapV2Pool)
            if known_pool_helper.factory == self._factory_address:
                self._add_pool(known_pool_helper)
                return known_pool_helper
            else:
                self._untracked_pools.add(pool_address)
                raise PoolNotAssociated(f"Pool {pool_address} is not associated with this DEX")

        try:
            new_pool_helper = self.pool_creator(
                address=pool_address,
                silent=silent,
                state_block=state_block,
                # factory_address=self._factory_address,
                # factory_init_hash=self._pool_init_hash,
                **liquiditypool_kwargs,
            )
        except DegenbotError as exc:
            self._untracked_pools.add(pool_address)
            raise ManagerError(f"Could not build V2 pool {pool_address}: {exc}") from None
        else:
            self._add_pool(new_pool_helper)
            return new_pool_helper


class UniswapV3PoolManager(AbstractPoolManager):
    """
    A class that generates and tracks Uniswap V3 liquidity pool helpers.
    """

    from .v3_liquidity_pool import UniswapV3Pool as pool_creator

    PoolCreatorType: TypeAlias = pool_creator
    _tracked_pools: dict[ChecksumAddress, PoolCreatorType]

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
        chain_id = chain_id if chain_id is not None else config.get_web3().eth.chain_id
        factory_address = to_checksum_address(factory_address)

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
        self._tracked_pools = dict()
        self._untracked_pools: set[ChecksumAddress] = set()

    def __delitem__(self, pool: PoolCreatorType | ChecksumAddress | str) -> None:
        pool_address: ChecksumAddress

        if isinstance(pool, UniswapV3Pool):
            pool_address = pool.address
        else:
            pool_address = to_checksum_address(pool)

        with contextlib.suppress(KeyError):
            del self._tracked_pools[pool_address]

        self._untracked_pools.discard(pool_address)

    def __repr__(self) -> str:  # pragma: no cover
        return f"UniswapV3PoolManager(factory={self._factory_address})"

    def _add_tracked_pool(self, pool_helper: PoolCreatorType) -> None:
        with self._lock:
            self._tracked_pools[pool_helper.address] = pool_helper

    def _apply_pending_liquidity_updates(self, pool: PoolCreatorType) -> None:
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
        pool_address: ChecksumAddress | str | None = None,
        token_addresses: tuple[
            ChecksumAddress | str,
            ChecksumAddress | str,
        ]
        | None = None,
        pool_fee: int | None = None,
        silent: bool = False,
        state_block: int | None = None,
        # keyword arguments passed to the pool class constructor
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> UniswapV3Pool:
        def find_or_build(
            pool_address: ChecksumAddress,
            state_block: int | None = None,
        ) -> UniswapV3Pool:
            if TYPE_CHECKING:
                assert isinstance(pool_class_kwargs, dict)

            # Check if the AllPools collection already has this pool
            if known_pool_helper := AllPools(self._chain_id).get(pool_address):
                if TYPE_CHECKING:
                    assert isinstance(known_pool_helper, UniswapV3Pool)
                if known_pool_helper.factory == self._factory_address:
                    self._add_tracked_pool(known_pool_helper)
                    return known_pool_helper
                else:
                    self._untracked_pools.add(pool_address)
                    raise PoolNotAssociated(f"Pool {pool_address} is not associated with this DEX")

            if self._snapshot:
                pool_class_kwargs.update(
                    {
                        "tick_bitmap": self._snapshot.get_tick_bitmap(pool_address),
                        "tick_data": self._snapshot.get_tick_data(pool_address),
                    }
                )
            else:
                logger.info("Initializing pool without liquidity snapshot")

            # The pool is unknown, so build and add it
            try:
                new_pool_helper = self.pool_creator(
                    address=pool_address,
                    silent=silent,
                    state_block=state_block,
                    **pool_class_kwargs,
                )
            except Exception as e:
                self._untracked_pools.add(pool_address)
                raise ManagerError(f"Could not build V3 pool {pool_address}: {e}") from e
            else:
                self._apply_pending_liquidity_updates(new_pool_helper)
                self._add_tracked_pool(new_pool_helper)
                return new_pool_helper

        if not (pool_address is None) ^ (token_addresses is None and pool_fee is None):
            raise ValueError("Insufficient arguments provided. Pass address OR tokens & fee")

        if pool_class_kwargs is None:
            pool_class_kwargs = dict()

        if pool_address is not None:
            pool_address = to_checksum_address(pool_address)
        elif token_addresses is not None and pool_fee is not None:
            pool_address = generate_v3_pool_address(
                token_addresses=sorted(token_addresses),
                fee=pool_fee,
                deployer_address=self._deployer_address,
                init_hash=self._pool_init_hash,
            )
        else:
            raise ValueError("Provide a pool address or a token address pair and fee")

        if pool_address in self._untracked_pools:
            raise PoolNotAssociated(
                f"Pool address {pool_address} not associated with factory {self._factory_address}"
            )

        try:
            pool_helper = self._tracked_pools[pool_address]
        except KeyError:
            pool_helper = find_or_build(pool_address, state_block)

        if TYPE_CHECKING:
            assert isinstance(pool_helper, UniswapV3Pool)

        assert isinstance(
            pool_helper, UniswapV3Pool
        ), f"{self} Attempted to return non-V3 pool {pool_helper}! {pool_address=}, {token_addresses=}, {pool_fee=}"  # noqa:E501
        return pool_helper
