from threading import Lock
from typing import TYPE_CHECKING, Dict, List, Optional, Set, Tuple, Union

from eth_typing import ChecksumAddress
from eth_utils import to_checksum_address
from web3 import Web3

from ..config import get_web3
from ..constants import ZERO_ADDRESS
from ..dex.uniswap import FACTORY_ADDRESSES, TICKLENS_ADDRESSES
from ..exceptions import ManagerError, PoolNotAssociated
from ..logging import logger
from ..manager import AllPools, Erc20TokenHelperManager
from ..erc20_token import Erc20Token
from ..baseclasses import HelperManager
from .abi import UNISWAP_V2_FACTORY_ABI
from .v2_liquidity_pool import LiquidityPool
from .v3_functions import generate_v3_pool_address
from .v3_snapshot import UniswapV3LiquiditySnapshot
from .v3_tick_lens import TickLens
from .v3_liquidity_pool import V3LiquidityPool


class UniswapLiquidityPoolManager(HelperManager):
    """
    Single-concern base class to allow derived classes to share state
    """

    _state: Dict[int, Dict] = dict()

    def __init__(
        self,
        factory_address: str,
        chain_id: int,
    ):
        """
        Initialize the specific state dictionary for the given chain id and
        factory address
        """

        # the internal state data for all child objects is held in a nested
        # class-level dictionary, keyed by chain ID and factory address
        try:
            self._state[chain_id]
        except KeyError:
            self._state[chain_id] = {}
            self._state[chain_id][
                "erc20token_manager"
            ] = Erc20TokenHelperManager(chain_id)

        try:
            self._state[chain_id][factory_address]
        except KeyError:
            self._state[chain_id][factory_address] = {}

    @classmethod
    def add_chain(cls, chain_id: int) -> None:
        """
        Add a new chain ID.
        """
        if not FACTORY_ADDRESSES.get(chain_id):
            FACTORY_ADDRESSES[chain_id] = {}

    @classmethod
    def add_factory(cls, chain_id: int, factory_address: str) -> None:
        """
        Add a new factory address at a given chain ID.
        """
        cls.add_chain(chain_id=chain_id)

        factory_address = to_checksum_address(factory_address)

        if not FACTORY_ADDRESSES[chain_id].get(factory_address):
            FACTORY_ADDRESSES[chain_id][factory_address] = {}

    @classmethod
    def add_pool_init_hash(
        cls, chain_id: int, factory_address: str, pool_init_hash: str
    ):
        """
        Add a pool_init_hash for a factory at a given chain ID.
        """

        factory_address = to_checksum_address(factory_address)

        cls.add_factory(chain_id=chain_id, factory_address=factory_address)

        if not FACTORY_ADDRESSES[chain_id][factory_address].get("init_hash"):
            FACTORY_ADDRESSES[chain_id][factory_address][
                "init_hash"
            ] = pool_init_hash


class UniswapV2LiquidityPoolManager(UniswapLiquidityPoolManager):
    """
    A class that generates and tracks Uniswap V2 liquidity pool helpers

    The state dictionary is held using the "Borg" singleton pattern, which
    ensures that all instances of the class have access to the same state data
    """

    def __init__(
        self,
        factory_address: Union[ChecksumAddress, str],
        chain_id: Optional[int] = None,
    ):
        _web3 = get_web3()
        if _web3 is not None:
            pass
        else:
            from brownie import web3 as brownie_web3  # type: ignore[import]

            if brownie_web3.isConnected():
                _web3 = brownie_web3
            else:
                raise ValueError("No connected web3 object provided.")

        if TYPE_CHECKING:
            assert isinstance(_web3, Web3)

        chain_id = chain_id or _web3.eth.chain_id

        factory_address = to_checksum_address(factory_address)

        super().__init__(
            factory_address=factory_address,
            chain_id=chain_id,
        )

        self.__dict__ = self._state[chain_id][factory_address]

        if self.__dict__ == {}:
            self._w3 = _web3
            self.chain_id = chain_id
            self._factory_address = factory_address
            self._w3_contract = self._w3.eth.contract(
                address=factory_address, abi=UNISWAP_V2_FACTORY_ABI
            )
            self._lock = Lock()
            self._tracked_pools: Dict[ChecksumAddress, LiquidityPool] = dict()
            self._token_manager: Erc20TokenHelperManager = self._state[
                chain_id
            ]["erc20token_manager"]
            self._factory_init_hash = FACTORY_ADDRESSES[chain_id][
                self._factory_address
            ]["init_hash"]
            self._untracked_pools: Set[ChecksumAddress] = set()

    def __delitem__(
        self, pool: Union[LiquidityPool, str, ChecksumAddress]
    ) -> None:
        pool_address: ChecksumAddress

        if isinstance(pool, LiquidityPool):
            pool_address = pool.address
        else:
            pool_address = to_checksum_address(pool)

        try:
            del self._tracked_pools[pool_address]
        except KeyError:
            pass

    def __repr__(self):
        return (
            f"UniswapV2LiquidityPoolManager(factory={self._factory_address})"
        )

    def _add_pool(self, pool_helper: LiquidityPool):
        with self._lock:
            self._tracked_pools[pool_helper.address] = pool_helper

    def get_pool(
        self,
        pool_address: Optional[str] = None,
        token_addresses: Optional[Tuple[str, str]] = None,
        silent: bool = False,
        update_method: str = "polling",
        state_block: Optional[int] = None,
    ) -> LiquidityPool:
        """
        Get the pool object from its address, or a tuple of token addresses
        """

        # WIP: re-work to avoid storing pools by token, convert to address first

        pool_helper: LiquidityPool

        if token_addresses is not None:
            if len(token_addresses) != 2:
                raise ValueError("Provide exactly two token addresses")

            checksummed_token_addresses = tuple(
                [
                    to_checksum_address(token_address)
                    for token_address in token_addresses
                ]
            )

            try:
                for token_address in checksummed_token_addresses:
                    self._token_manager.get_erc20token(
                        address=token_address,
                        silent=silent,
                    )
            except Exception:
                raise ManagerError("Could not get both Erc20Token helpers")

            pool_address = to_checksum_address(
                self._w3_contract.functions.getPair(
                    *checksummed_token_addresses
                ).call()
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
            pool_helper = self._tracked_pools[pool_address]
        except KeyError:
            pass
        else:
            assert isinstance(
                pool_helper, LiquidityPool
            ), f"{self} Attempted to return non-V2 pool {pool_helper}! {pool_address=}, {token_addresses=}"
            return pool_helper

        # Check if the AllPools collection already has this pool
        # print(f"(get_pool) no pool found, checking AllPools")
        if pool_helper := AllPools(self.chain_id).get(pool_address):
            if pool_helper.factory == self._factory_address:
                print(
                    "(get_pool) found pool in AllPools, factory address match"
                )
                self._add_pool(pool_helper)
                assert pool_helper.address in self._tracked_pools
                assert isinstance(
                    pool_helper, LiquidityPool
                ), f"{self} Attempted to return non-V2 pool {pool_helper}! {pool_address=}, {token_addresses=}"
                return pool_helper
            else:
                self._untracked_pools.add(pool_address)

        try:
            pool_helper = LiquidityPool(
                address=pool_address,
                silent=silent,
                state_block=state_block,
                factory_address=self._factory_address,
                factory_init_hash=self._factory_init_hash,
                update_method=update_method,
            )
        except Exception as e:
            self._untracked_pools.add(pool_address)
            raise ManagerError(f"Could not build V2 pool {pool_address}: {e}")
        else:
            self._add_pool(pool_helper)
            assert isinstance(
                pool_helper, LiquidityPool
            ), f"{self} Attempted to return non-V2 pool {pool_helper}! {pool_address=}, {token_addresses=}"
            return pool_helper


class UniswapV3LiquidityPoolManager(UniswapLiquidityPoolManager):
    """
    A class that generates and tracks Uniswap V3 liquidity pool helpers

    The state dictionary is held using the "Borg" singleton pattern, which
    ensures that all instances of the class have access to the same state data
    """

    def __init__(
        self,
        factory_address: Union[ChecksumAddress, str],
        chain_id: Optional[int] = None,
        snapshot: Optional[UniswapV3LiquiditySnapshot] = None,
    ):
        _web3 = get_web3()
        if _web3 is not None:
            pass
        else:
            from brownie import web3 as brownie_web3  # type: ignore[import]

            if brownie_web3.isConnected():
                _web3 = brownie_web3
            else:
                raise ValueError("No connected web3 object provided.")

        if TYPE_CHECKING:
            assert isinstance(_web3, Web3)

        chain_id = chain_id or _web3.eth.chain_id

        factory_address = to_checksum_address(factory_address)

        super().__init__(
            factory_address=factory_address,
            chain_id=chain_id,
        )

        self.__dict__ = self._state[chain_id][factory_address]

        if self.__dict__ == {}:
            try:
                self._w3 = _web3
                self.chain_id = chain_id
                self._factory_address = to_checksum_address(factory_address)
                self._lens = TickLens(
                    address=TICKLENS_ADDRESSES[self._w3.eth.chain_id][
                        self._factory_address
                    ]
                )
                self._lock = Lock()
                self._tracked_pools: Dict[
                    ChecksumAddress, V3LiquidityPool
                ] = {}
                self._token_manager: Erc20TokenHelperManager = self._state[
                    chain_id
                ]["erc20token_manager"]
                self._factory_init_hash = FACTORY_ADDRESSES[chain_id][
                    self._factory_address
                ]["init_hash"]
                self._snapshot = snapshot
                self._untracked_pools: Set[ChecksumAddress] = set()
            except:
                self._state[chain_id][factory_address] = {}
                raise

    def __delitem__(
        self, pool: Union[V3LiquidityPool, str, ChecksumAddress]
    ) -> None:
        pool_address: ChecksumAddress

        if isinstance(pool, V3LiquidityPool):
            pool_address = pool.address
        else:
            pool_address = to_checksum_address(pool)

        try:
            del self._tracked_pools[pool_address]
        except KeyError:
            pass

    def __repr__(self):
        return (
            f"UniswapV3LiquidityPoolManager(factory={self._factory_address})"
        )

    def _store_pool(self, pool_helper: V3LiquidityPool):
        with self._lock:
            self._tracked_pools[pool_helper.address] = pool_helper

    def get_pool(
        self,
        pool_address: Optional[Union[ChecksumAddress, str]] = None,
        token_addresses: Optional[
            Tuple[Union[str, ChecksumAddress], Union[str, ChecksumAddress]]
        ] = None,
        pool_fee: Optional[int] = None,
        silent: bool = False,
        # keyword arguments passed to the `V3LiquidityPool` constructor
        v3liquiditypool_kwargs: Optional[dict] = None,
        state_block: Optional[int] = None,
    ) -> V3LiquidityPool:
        """
        Get the pool object from its address, or a tuple of token addresses
        and fee in bips (e.g. 100, 500, 3000, 10000)
        """

        def apply_liquidity_updates(pool: V3LiquidityPool):
            logger.debug(f"Applying liquidity updates to {pool}")
            if not self._snapshot:
                return
            for external_update in self._snapshot.get_pool_updates(
                pool.address
            ):
                pool.external_update(update=external_update, force=True)

        def find_or_build(
            pool_address: ChecksumAddress,
            state_block: Optional[int] = None,
        ) -> V3LiquidityPool:
            if TYPE_CHECKING:
                assert isinstance(v3liquiditypool_kwargs, dict)

            # Check if the AllPools collection already has this pool
            pool_helper: V3LiquidityPool = AllPools(self.chain_id).get(
                pool_address
            )
            if pool_helper:
                if pool_helper.factory == self._factory_address:
                    self._store_pool(pool_helper)
                    assert pool_helper.address in self._tracked_pools
                    assert isinstance(
                        pool_helper, V3LiquidityPool
                    ), f"{self} Attempted to return non-V3 pool {pool_helper}! {pool_address=}, {token_addresses=}, {pool_fee=}"
                    return pool_helper
                else:
                    self._untracked_pools.add(pool_address)
                    raise ManagerError(
                        f"Pool {pool_address} is not associated with this DEX"
                    )

            if self._snapshot:
                v3liquiditypool_kwargs.update(
                    {
                        "tick_bitmap": self._snapshot.get_tick_bitmap(
                            pool_address
                        ),
                        "tick_data": self._snapshot.get_tick_data(
                            pool_address
                        ),
                    }
                )
            else:
                print("SNAPSHOT NOT AVAILABLE!")

            # The pool is unknown, so build and add it
            try:
                pool_helper = V3LiquidityPool(
                    address=pool_address,
                    lens=self._lens,
                    silent=silent,
                    factory_address=self._factory_address,
                    factory_init_hash=self._factory_init_hash,
                    **v3liquiditypool_kwargs,
                    state_block=state_block,
                )
            except Exception as e:
                self._untracked_pools.add(pool_address)
                raise ManagerError(
                    f"Could not build V3 pool {pool_address}: {e}"
                ) from e
            else:
                apply_liquidity_updates(pool_helper)
                self._store_pool(pool_helper)
                assert pool_helper.address in self._tracked_pools
                assert isinstance(
                    pool_helper, V3LiquidityPool
                ), f"{self} Attempted to return non-V3 pool {pool_helper}! {pool_address=}, {token_addresses=}, {pool_fee=}"
                return pool_helper

        if not (pool_address is None) ^ (
            token_addresses is None and pool_fee is None
        ):
            raise ValueError(
                "Insufficient arguments provided. Pass address OR tokens & fee"
            )

        if v3liquiditypool_kwargs is None:
            v3liquiditypool_kwargs = dict()

        if pool_address is not None:
            # print(f"building V3 pool from address")
            if token_addresses is not None or pool_fee is not None:
                raise ValueError(
                    "Conflicting arguments provided. Pass address OR tokens+fee"
                )
            pool_address = to_checksum_address(pool_address)
        elif token_addresses is not None and pool_fee is not None:
            # print(f"building V3 pool from address and fee")
            if len(token_addresses) != 2:
                raise ValueError(
                    f"Expected two tokens, found {len(token_addresses)}"
                )
            try:
                erc20token_helpers: List[Erc20Token] = [
                    self._token_manager.get_erc20token(
                        address=token_address,
                        silent=silent,
                    )
                    for token_address in token_addresses
                ]
            except Exception:
                raise ManagerError("Could not build Erc20Token helper")

            # dictionary key pair is sorted by address
            erc20token_helpers = sorted(erc20token_helpers)
            tokens_key: Tuple[ChecksumAddress, ChecksumAddress] = (
                erc20token_helpers[0].address,
                erc20token_helpers[1].address,
            )

            pool_address = generate_v3_pool_address(
                token_addresses=tokens_key,
                fee=pool_fee,
                factory_address=self._factory_address,
                init_hash=self._factory_init_hash,
            )
        else:
            raise ValueError("THIS BLOCK SHOULD BE UNREACHABLE")

        if pool_address in self._untracked_pools:
            raise PoolNotAssociated(
                f"Pool address {pool_address} not associated with factory {self._factory_address}"
            )

        try:
            pool_helper = self._tracked_pools[pool_address]
        except KeyError:
            pool_helper = find_or_build(pool_address, state_block)

        if TYPE_CHECKING:
            assert isinstance(pool_helper, V3LiquidityPool)

        assert isinstance(
            pool_helper, V3LiquidityPool
        ), f"{self} Attempted to return non-V3 pool {pool_helper}! {pool_address=}, {token_addresses=}, {pool_fee=}"
        return pool_helper
