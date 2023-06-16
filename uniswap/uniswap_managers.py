from threading import Lock
from typing import Dict, List, Optional, Tuple, Union

from brownie import Contract, chain  # type: ignore
from web3 import Web3

from degenbot.constants import ZERO_ADDRESS
from degenbot.exceptions import Erc20TokenError, ManagerError
from degenbot.manager.base import Manager
from degenbot.manager.token_manager import Erc20TokenHelperManager
from degenbot.token import Erc20Token
from degenbot.uniswap.abi import UNISWAPV2_FACTORY_ABI
from degenbot.uniswap.v2.liquidity_pool import LiquidityPool
from degenbot.uniswap.abi import UNISWAP_V3_FACTORY_ABI
from degenbot.uniswap.v3.functions import generate_v3_pool_address
from degenbot.uniswap.v3.tick_lens import TickLens
from degenbot.uniswap.v3.v3_liquidity_pool import V3LiquidityPool
from degenbot.manager import AllPools

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

# _all_pools: Dict[
#     int,
#     Dict[str, Union[LiquidityPool, V3LiquidityPool]],
# ] = {}


# class AllPools:
#     def __init__(self, chain_id):
#         try:
#             _all_pools[chain_id]
#         except KeyError:
#             _all_pools[chain_id] = {}
#         finally:
#             self.pools = _all_pools[chain_id]

#     def __delitem__(self, pool_address: str):
#         del self.pools[pool_address]

#     def __getitem__(self, pool_address: str):
#         return self.pools[pool_address]

#     def __setitem__(
#         self,
#         pool_address: str,
#         pool_helper: Union[LiquidityPool, V3LiquidityPool],
#     ):
#         self.pools[pool_address] = pool_helper

#     def __len__(self):
#         return len(self.pools)

#     def get(self, pool_address: str):
#         try:
#             return self.pools[pool_address]
#         except KeyError:
#             return None


class UniswapLiquidityPoolManager(Manager):
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
                abi=UNISWAPV2_FACTORY_ABI,
                persist=False,
            )
            self._lock = Lock()
            self._pools_by_address: Dict[str, LiquidityPool] = dict()
            self._pools_by_tokens: Dict[
                Tuple[str, str], LiquidityPool
            ] = dict()
            self._token_manager: Erc20TokenHelperManager = self._state[
                chain_id
            ]["erc20token_manager"]
            self._factory_init_hash = _FACTORY_INIT_HASH[chain_id][
                self._factory_address
            ]
            self.all_pools = AllPools(chain_id)

        # from pprint import pprint
        # pprint(self._state)

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

            with self._lock:
                self._pools_by_address[pool_address] = pool_helper
                self._pools_by_tokens[
                    (
                        pool_helper.token0.address,
                        pool_helper.token1.address,
                    )
                ] = pool_helper
                self.all_pools[pool_address] = pool_helper

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
                pool_helper = self._pools_by_tokens[tokens_key]
            except KeyError:
                pass
            else:
                return pool_helper

            if (
                pool_address := self._brownie_factory_contract.getPair(
                    *tokens_key
                )
            ) == ZERO_ADDRESS:
                raise ManagerError("No V2 LP available")

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

            with self._lock:
                self._pools_by_address[pool_address] = pool_helper
                self._pools_by_tokens[tokens_key] = pool_helper
                self.all_pools[pool_address] = pool_helper

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
            self._factory_address = factory_address
            self._brownie_factory_contract = Contract.from_abi(
                name="Uniswap V3: Factory",
                address=factory_address,
                abi=UNISWAP_V3_FACTORY_ABI,
                persist=False,
            )
            self._lens = TickLens()
            self._lock = Lock()
            self._pools_by_address: Dict[str, V3LiquidityPool] = {}
            self._pools_by_tokens_and_fee: Dict[
                Tuple[str, str, int], V3LiquidityPool
            ] = {}
            self._token_manager = self._state[chain_id]["erc20token_manager"]
            self._factory_init_hash = _FACTORY_INIT_HASH[chain_id][
                self._factory_address
            ]
            self.all_pools = AllPools(chain_id)

    def get_pool(
        self,
        pool_address: Optional[str] = None,
        token_addresses: Optional[Tuple[str, str]] = None,
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
                pool_helper = self._pools_by_address[pool_address]
            except KeyError:
                pass
            else:
                return pool_helper

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

            pool_fee = pool_helper.fee

            token_addresses = (
                pool_helper.token0.address,
                pool_helper.token1.address,
            )

            with self._lock:
                dict_key = *token_addresses, pool_fee
                self._pools_by_address[pool_address] = pool_helper
                self._pools_by_tokens_and_fee[dict_key] = pool_helper
                self.all_pools[pool_address] = pool_helper

        elif token_addresses is not None and pool_fee is not None:
            if len(token_addresses) != 2:
                raise ValueError(
                    f"Expected two tokens, found {len(token_addresses)}"
                )

            try:
                erc20token_helpers = [
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
                token_addresses=tokens_key, fee=pool_fee
            )

            try:
                pool_helper = self._pools_by_address[pool_address]
            except KeyError:
                pass
            else:
                return pool_helper

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

            with self._lock:
                self._pools_by_address[pool_address] = pool_helper
                self._pools_by_tokens_and_fee[dict_key] = pool_helper
                self.all_pools[pool_address] = pool_helper

        return pool_helper
