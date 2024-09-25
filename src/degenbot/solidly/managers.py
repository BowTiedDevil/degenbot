import contextlib
from threading import Lock
from typing import TYPE_CHECKING, Any, cast

from eth_typing import BlockIdentifier, ChecksumAddress
from eth_utils.address import to_checksum_address
from web3 import Web3

from degenbot.functions import encode_function_calldata, get_number_for_block_identifier, raw_call

from .. import config
from ..constants import ZERO_ADDRESS
from ..exceptions import Erc20TokenError, ManagerError, PoolNotAssociated
from ..exchanges.solidly.deployments import FACTORY_DEPLOYMENTS
from ..exchanges.solidly.types import SolidlyExchangeDeployment
from ..manager.token_manager import Erc20TokenHelperManager
from ..registry.all_pools import AllPools
from ..solidly.solidly_liquidity_pool import AerodromeV2Pool


class SolidlyV2PoolManager:
    def __init__(
        self,
        factory_address: str,
        deployer_address: ChecksumAddress | str | None = None,
        chain_id: int | None = None,
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
        except KeyError:
            deployer_address = (
                to_checksum_address(deployer_address)
                if deployer_address is not None
                else factory_address
            )

        self._lock = Lock()
        self._chain_id = chain_id
        self._factory_address = factory_address
        self._deployer_address = deployer_address
        self._token_manager: Erc20TokenHelperManager = Erc20TokenHelperManager(chain_id=chain_id)
        self._tracked_pools: dict[ChecksumAddress, AerodromeV2Pool] = dict()
        self._untracked_pools: set[ChecksumAddress] = set()

    @classmethod
    def from_exchange(
        cls,
        exchange: SolidlyExchangeDeployment,
    ) -> "SolidlyV2PoolManager":
        return cls(
            factory_address=exchange.factory.address,
            deployer_address=exchange.factory.deployer,
        )

    def __delitem__(self, pool: AerodromeV2Pool | ChecksumAddress | str) -> None:
        pool_address: ChecksumAddress

        if isinstance(pool, AerodromeV2Pool):
            pool_address = pool.address
        else:
            pool_address = to_checksum_address(pool)

        with contextlib.suppress(KeyError):
            del self._tracked_pools[pool_address]

        self._untracked_pools.discard(pool_address)
        assert pool_address not in self._untracked_pools

    def __repr__(self) -> str:  # pragma: no cover
        return f"UniswapV2PoolManager(factory={self._factory_address})"

    def _add_pool(self, pool_helper: AerodromeV2Pool) -> None:
        with self._lock:
            self._tracked_pools[pool_helper.address] = pool_helper
        assert pool_helper.address in self._tracked_pools

    def get_pair_from_factory(
        self,
        w3: Web3,
        token0: ChecksumAddress,
        token1: ChecksumAddress,
        stable: bool,
        block_identifier: BlockIdentifier | None = None,
    ) -> str:
        pool_address, *_ = raw_call(
            w3=w3,
            address=self._factory_address,
            calldata=encode_function_calldata(
                function_prototype="getPool(address,address,bool)",
                function_arguments=[token0, token1, stable],
            ),
            return_types=["address"],
            block_identifier=get_number_for_block_identifier(block_identifier),
        )
        return cast(str, pool_address)

    def get_pool(
        self,
        stable: bool,
        pool_address: str | None = None,
        token_addresses: tuple[str, str] | None = None,
        silent: bool = False,
        state_block: int | None = None,
        liquiditypool_kwargs: dict[str, Any] | None = None,
    ) -> AerodromeV2Pool:
        """
        Get the pool object from its address, or a tuple of token addresses
        """

        if liquiditypool_kwargs is None:
            liquiditypool_kwargs = dict()

        if token_addresses is not None:
            checksummed_token_addresses = tuple(
                [to_checksum_address(token_address) for token_address in token_addresses]
            )

            try:
                for token_address in checksummed_token_addresses:
                    self._token_manager.get_erc20token(
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
                    stable=stable,
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
        pool_helper = AllPools(self._chain_id).get(pool_address)
        if pool_helper:
            if TYPE_CHECKING:
                assert isinstance(pool_helper, AerodromeV2Pool)
            if pool_helper.factory == self._factory_address:
                self._add_pool(pool_helper)
                return pool_helper
            else:
                self._untracked_pools.add(pool_address)
                raise PoolNotAssociated(f"Pool {pool_address} is not associated with this DEX")

        try:
            pool_helper = AerodromeV2Pool(
                address=pool_address,
                silent=silent,
                state_block=state_block,
                # factory_address=self._factory_address,
                # factory_init_hash=self._pool_init_hash,
                **liquiditypool_kwargs,
            )
        except Exception as exc:
            self._untracked_pools.add(pool_address)
            raise ManagerError(f"Could not build V2 pool {pool_address}: {exc}") from exc
        else:
            self._add_pool(pool_helper)
            return pool_helper
