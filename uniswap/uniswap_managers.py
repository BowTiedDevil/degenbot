from threading import Lock
from typing import Dict, List, Optional, Tuple, Union

from brownie import Contract, chain  # type: ignore
from eth_typing import ChecksumAddress
from web3 import Web3

from degenbot.constants import ZERO_ADDRESS
from degenbot.exceptions import Erc20TokenError, ManagerError
from degenbot.manager import AllPools, Erc20TokenHelperManager
from degenbot.token import Erc20Token
from degenbot.types import HelperManager
from degenbot.uniswap.abi import UNISWAP_V3_FACTORY_ABI, UNISWAP_V2_FACTORY_ABI
from degenbot.uniswap.v2 import LiquidityPool
from degenbot.uniswap.v3 import TickLens, V3LiquidityPool
from degenbot.uniswap.v3.functions import generate_v3_pool_address

_FACTORY_INIT_HASH = {
    1: {
        # Uniswap (V2)
        "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f": "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
        # Uniswap (V3)
        "0x1F98431c8aD98523631AE4a59f267346ea31F984": "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
        # Sushiswap (V2)
        "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac": "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
        # Sushiswap (V3)
        "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F": "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    },
    42161: {
        # Uniswap (V3)
        "0x1F98431c8aD98523631AE4a59f267346ea31F984": "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
        # Sushiswap (V2)
        "0xc35DADB65012eC5796536bD9864eD8773aBc74C4": "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
        # Sushiswap (V3)
        "0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e": "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    },
}


class UniswapLiquidityPoolManager(HelperManager):
    """
    Single-concern base class to allow derived classes to share state
    """

    _state: dict = {}

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

    def __init__(self, factory_address: str, chain_id: Optional[int] = None):
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
            self._factory_init_hash = _FACTORY_INIT_HASH[chain_id][
                self._factory_address
            ]

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
                )
            except:
                raise ManagerError(f"Could not build V2 pool: {pool_address=}")

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
                )
            except Exception as e:
                raise ManagerError(
                    f"Could not build V2 pool: {pool_address=}: {e}"
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

    def __init__(self, factory_address: str, chain_id: Optional[int] = None):
        if chain_id is None:
            chain_id = chain.id

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
            self._pools_by_address: Dict[ChecksumAddress, V3LiquidityPool] = {}
            self._pools_by_tokens_and_fee: Dict[
                Tuple[str, str, int], V3LiquidityPool
            ] = {}
            self._token_manager = self._state[chain_id]["erc20token_manager"]
            self._factory_init_hash = _FACTORY_INIT_HASH[chain_id][
                self._factory_address
            ]

    def _add_pool(self, pool_helper: V3LiquidityPool):
        with self._lock:
            pool_key = (
                pool_helper.token0.address,
                pool_helper.token1.address,
                pool_helper.fee,
            )

            self._pools_by_address[pool_helper.address] = pool_helper
            self._pools_by_tokens_and_fee[pool_key] = pool_helper

    def get_pool(
        self,
        pool_address: Optional[str] = None,
        token_addresses: Optional[
            Tuple[Union[str, ChecksumAddress], Union[str, ChecksumAddress]]
        ] = None,
        pool_fee: Optional[int] = None,
        silent: bool = False,
        # keyword arguments passed to the `V3LiquidityPool` constructor
        v3liquiditypool_kwargs: Optional[dict] = None,
    ) -> V3LiquidityPool:
        """
        Get the pool object from its address, or a tuple of token
        addresses and fee
        """

        if not (pool_address is None) ^ (
            token_addresses is None and pool_fee is None
        ):
            raise ValueError(
                f"Insufficient arguments provided. Pass address OR tokens+fee"
            )

        dict_key: tuple[str, str, int]
        pool_helper: V3LiquidityPool

        if v3liquiditypool_kwargs is None:
            v3liquiditypool_kwargs = {}

        if pool_address is not None:
            if token_addresses is not None or pool_fee is not None:
                raise ValueError(
                    f"Conflicting arguments provided. Pass address OR tokens+fee"
                )

            pool_address = Web3.toChecksumAddress(pool_address)

            try:
                return self._pools_by_address[pool_address]
            except KeyError:
                pass

            # check if the collection already has this pool
            pool_helper = AllPools(chain.id).get(pool_address)
            if pool_helper:
                self._add_pool(pool_helper)
                return pool_helper

            # the pool is new, so build it
            try:
                pool_helper = V3LiquidityPool(
                    address=pool_address,
                    lens=self._lens,
                    silent=silent,
                    factory_address=self._factory_address,
                    factory_init_hash=self._factory_init_hash,
                    **v3liquiditypool_kwargs,
                )
            except Exception as e:
                raise ManagerError(
                    f"Could not build V3 pool: {pool_address=}: {e}"
                ) from e
            else:
                self._add_pool(pool_helper)

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
            tokens_key: Tuple[str, str]
            tokens_key = tuple(
                [token.address for token in erc20token_helpers]
            )  # type:ignore
            dict_key = *tokens_key, pool_fee

            try:
                pool_helper = self._pools_by_tokens_and_fee[dict_key]
            except KeyError:
                pass
            else:
                return pool_helper

            pool_address = generate_v3_pool_address(
                token_addresses=tokens_key,
                fee=pool_fee,
                factory_address=self._factory_address,
                init_hash=self._factory_init_hash,
            )

            try:
                pool_helper = self._pools_by_address[pool_address]
            except KeyError:
                pass
            else:
                return pool_helper

            # check if the AllPools collection already has this pool
            pool_helper = AllPools(chain.id).get(pool_address)
            if pool_helper:
                self._add_pool(pool_helper)
                return pool_helper

            # the pool is new, so build it
            try:
                pool_helper = V3LiquidityPool(
                    address=pool_address,
                    tokens=list(erc20token_helpers),
                    lens=self._lens,
                    silent=silent,
                    **v3liquiditypool_kwargs,
                )
            except:
                raise ManagerError(
                    f"Could not build V3 pool: {pool_address=}, {pool_fee=}"
                )

            self._add_pool(pool_helper)

        return pool_helper
