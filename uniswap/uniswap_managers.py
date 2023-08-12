from threading import Lock
from typing import TYPE_CHECKING, Dict, List, Optional, Tuple, Union

from brownie import Contract, chain  # type: ignore
from eth_typing import ChecksumAddress
from web3 import Web3

from degenbot.constants import ZERO_ADDRESS
from degenbot.exceptions import (
    Erc20TokenError,
    ManagerError,
    PoolNotAssociated,
)
from degenbot.logging import logger
from degenbot.manager import AllPools, Erc20TokenHelperManager
from degenbot.token import Erc20Token
from degenbot.types import HelperManager
from degenbot.uniswap.abi import UNISWAP_V2_FACTORY_ABI, UNISWAP_V3_FACTORY_ABI
from degenbot.uniswap.v2.liquidity_pool import LiquidityPool
from degenbot.uniswap.v3.functions import generate_v3_pool_address
from degenbot.uniswap.v3.snapshot import UniswapV3LiquiditySnapshot
from degenbot.uniswap.v3.tick_lens import TickLens
from degenbot.uniswap.v3.v3_liquidity_pool import V3LiquidityPool

_FACTORIES = {
    1: {
        # Uniswap (V2)
        "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f": {
            "init_hash": "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
        },
        # Uniswap (V3)
        "0x1F98431c8aD98523631AE4a59f267346ea31F984": {
            "init_hash": "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"
        },
        # Sushiswap (V2)
        "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac": {
            "init_hash": "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303"
        },
        # Sushiswap (V3)
        "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F": {
            "init_hash": "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"
        },
    },
    42161: {
        # Uniswap (V3)
        "0x1F98431c8aD98523631AE4a59f267346ea31F984": {
            "init_hash": "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"
        },
        # Sushiswap (V2)
        "0xc35DADB65012eC5796536bD9864eD8773aBc74C4": {
            "init_hash": "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303"
        },
        # Sushiswap (V3)
        "0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e": {
            "init_hash": "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"
        },
    },
}


# Sentinel object, added to the pool tracking table when a pool that is
# associated with the DEX is found.
# e.g. get_pool calls to a Uniswap pool manager will find the sentinel object
# and terminate early instead of attempting to create a pool helper that
# fails the factory address checks
WrongDexSentinel = object()


class UniswapLiquidityPoolManager(HelperManager):
    """
    Single-concern base class to allow derived classes to share state
    """

    _state: Dict[int, Dict] = dict()

    @classmethod
    def add_chain(cls, chain_id: int) -> None:
        """
        Add a new chain ID.
        """
        if not _FACTORIES.get(chain_id):
            _FACTORIES[chain_id] = {}

    @classmethod
    def add_factory(cls, chain_id: int, factory_address: str) -> None:
        """
        Add a new factory address at a given chain ID.
        """
        cls.add_chain(chain_id=chain_id)

        factory_address = Web3.toChecksumAddress(factory_address)

        if not _FACTORIES[chain_id].get(factory_address):
            _FACTORIES[chain_id][factory_address] = {}

    @classmethod
    def add_pool_init_hash(
        cls, chain_id: int, factory_address: str, pool_init_hash: str
    ):
        """
        Add a pool_init_hash for a factory at a given chain ID.
        """

        factory_address = Web3.toChecksumAddress(factory_address)

        cls.add_factory(chain_id=chain_id, factory_address=factory_address)

        if not _FACTORIES[chain_id][factory_address].get("init_hash"):
            _FACTORIES[chain_id][factory_address]["init_hash"] = pool_init_hash

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
        if chain_id is None:
            chain_id = chain.id

        factory_address = Web3.toChecksumAddress(factory_address)

        super().__init__(
            factory_address=factory_address,
            chain_id=chain_id,
        )

        self.__dict__ = self._state[chain_id][factory_address]

        if not self.__dict__:
            # initialize internal attributes
            self.chain_id = chain_id
            self._factory_address = factory_address
            self._brownie_factory_contract = Contract.from_abi(
                name="Uniswap V2: Factory",
                address=factory_address,
                abi=UNISWAP_V2_FACTORY_ABI,
                persist=False,
            )
            self._lock = Lock()
            self._pools_by_address: Dict[
                ChecksumAddress, LiquidityPool
            ] = dict()
            self._pools_by_tokens: Dict[
                Tuple[str, str], LiquidityPool
            ] = dict()
            self._token_manager: Erc20TokenHelperManager = self._state[
                chain_id
            ]["erc20token_manager"]
            self._factory_init_hash = _FACTORIES[chain_id][
                self._factory_address
            ]["init_hash"]

    def _add_pool(self, pool_helper: LiquidityPool):
        with self._lock:
            pool_key = (
                pool_helper.token0.address,
                pool_helper.token1.address,
            )
            self._pools_by_address[pool_helper.address] = pool_helper
            self._pools_by_tokens[pool_key] = pool_helper

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

        pool_helper: LiquidityPool

        if pool_address is not None:
            pool_address = Web3.toChecksumAddress(pool_address)

            try:
                pool_helper = self._pools_by_address[pool_address]
            except KeyError:
                pass
            else:
                return pool_helper

            try:
                pool_helper = LiquidityPool(
                    address=pool_address,
                    silent=silent,
                    state_block=state_block,
                )
            except Exception as e:
                raise ManagerError(
                    f"Could not build V2 pool {pool_address}: {e}"
                )

            self._add_pool(pool_helper)

        elif token_addresses is not None:
            if len(token_addresses) != 2:
                raise ValueError(
                    f"Expected two tokens, found {len(token_addresses)}"
                )

            try:
                erc20token_helpers: List[Erc20Token] = [
                    self._token_manager.get_erc20token(
                        address=token_address,
                        min_abi=True,
                        silent=silent,
                        unload_brownie_contract_after_init=True,
                    )
                    for token_address in token_addresses
                ]
            except Erc20TokenError:
                raise ManagerError(
                    f"Could not build Erc20Token helpers for pool {pool_address}"
                )

            tokens_key: Tuple[str, str]
            tokens_key = tuple(
                [token.address for token in sorted(erc20token_helpers)]
            )  # type: ignore [assignment]

            try:
                return self._pools_by_tokens[tokens_key]
            except KeyError:
                pass

            if (
                pool_address := self._brownie_factory_contract.getPair(
                    *tokens_key
                )
            ) == ZERO_ADDRESS:
                raise ManagerError("No V2 LP available")

            # check if the AllPools collection already has this pool
            pool_helper = AllPools(chain.id).get(pool_address)
            if pool_helper:
                self._add_pool(pool_helper)
                return pool_helper

            # the pool is new, so build it
            try:
                pool_helper = LiquidityPool(
                    address=pool_address,
                    tokens=erc20token_helpers,
                    silent=silent,
                    update_method=update_method,
                    factory_address=self._factory_address,
                    factory_init_hash=self._factory_init_hash,
                    state_block=state_block,
                )
            except Exception as e:
                raise ManagerError(
                    f"Could not build V2 pool {pool_address}: {e}"
                )
            else:
                self._add_pool(pool_helper)

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
        if chain_id is None:
            chain_id = chain.id

        factory_address = Web3.toChecksumAddress(factory_address)

        super().__init__(
            factory_address=factory_address,
            chain_id=chain_id,
        )

        self.__dict__ = self._state[chain_id][factory_address]

        if self.__dict__ == {}:
            # initialize internal attributes
            self.chain_id = chain_id
            self._factory_address = Web3.toChecksumAddress(factory_address)
            self._brownie_factory_contract = Contract.from_abi(
                name="Uniswap V3: Factory",
                address=factory_address,
                abi=UNISWAP_V3_FACTORY_ABI,
                persist=False,
            )
            self._lens = TickLens(self._factory_address)
            self._lock = Lock()
            self._tracked_pools: Dict[
                ChecksumAddress, Union[V3LiquidityPool, object]
            ] = {}
            self._token_manager: Erc20TokenHelperManager = self._state[
                chain_id
            ]["erc20token_manager"]
            self._factory_init_hash = _FACTORIES[chain_id][
                self._factory_address
            ]["init_hash"]
            self._snapshot = snapshot

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
        Get the pool object from its address, or a tuple of ordered token
        addresses and fee
        """

        def apply_liquidity_updates(pool: V3LiquidityPool):
            logger.debug(f"Applying liquidity updates to {pool}")
            if not self._snapshot:
                return
            for external_update in self._snapshot.get_pool_updates(
                pool.address
            ):
                pool.external_update(update=external_update)

        def find_or_build(
            pool_address: ChecksumAddress,
            state_block: Optional[int] = None,
        ) -> V3LiquidityPool:
            if TYPE_CHECKING:
                assert isinstance(v3liquiditypool_kwargs, dict)

            # Check if the AllPools collection already has this pool
            pool_helper: V3LiquidityPool = AllPools(chain.id).get(pool_address)
            if pool_helper:
                if pool_helper.factory == self._factory_address:
                    self._store_pool(pool_helper)
                    assert pool_helper.address in self._tracked_pools
                else:
                    # print(f'Setting "WRONG DEX" for {pool_helper.address}')
                    self._tracked_pools[pool_helper.address] = WrongDexSentinel
                    raise ManagerError(
                        f"Pool {pool_address} is not associated with this DEX"
                    )

                return pool_helper

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
                raise ManagerError(
                    f"Could not build V3 pool {pool_address}: {e}"
                ) from e
            else:
                apply_liquidity_updates(pool_helper)
                self._store_pool(pool_helper)
                assert pool_helper.address in self._tracked_pools
                return pool_helper

        if not (pool_address is None) ^ (
            token_addresses is None and pool_fee is None
        ):
            raise ValueError(
                f"Insufficient arguments provided. Pass address OR tokens & fee"
            )

        if v3liquiditypool_kwargs is None:
            v3liquiditypool_kwargs = dict()

        if pool_address is not None:
            if token_addresses is not None or pool_fee is not None:
                raise ValueError(
                    f"Conflicting arguments provided. Pass address OR tokens+fee"
                )
            pool_address = Web3.toChecksumAddress(pool_address)
        elif token_addresses is not None and pool_fee is not None:
            if len(token_addresses) != 2:
                raise ValueError(
                    f"Expected two tokens, found {len(token_addresses)}"
                )
            try:
                erc20token_helpers: List[Erc20Token] = [
                    self._token_manager.get_erc20token(
                        address=token_address,
                        min_abi=True,
                        silent=silent,
                        unload_brownie_contract_after_init=True,
                    )
                    for token_address in token_addresses
                ]
            except Erc20TokenError:
                raise ManagerError("Could not build Erc20Token helpers")

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

        try:
            pool_helper = self._tracked_pools[pool_address]
        except KeyError:
            pool_helper = find_or_build(pool_address, state_block)

        if pool_helper is WrongDexSentinel:
            raise PoolNotAssociated(
                f"Pool address {pool_address} not associated with factory {self._factory_address}"
            )

        if TYPE_CHECKING:
            assert isinstance(pool_helper, V3LiquidityPool)

        return pool_helper
