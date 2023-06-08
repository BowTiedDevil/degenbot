from threading import Lock
from typing import Dict, List, Optional, Tuple, Union

from brownie import Contract, chain  # type: ignore
from web3 import Web3

from degenbot.constants import ZERO_ADDRESS
from degenbot.exceptions import Erc20TokenError, ManagerError
from degenbot.manager.base import Manager
from degenbot.manager.token_manager import Erc20TokenHelperManager
from degenbot.token import Erc20Token
from degenbot.uniswap.v2.abi import UNISWAPV2_FACTORY_ABI
from degenbot.uniswap.v2.liquidity_pool import LiquidityPool
from degenbot.uniswap.v3.abi import UNISWAP_V3_FACTORY_ABI
from degenbot.uniswap.v3.functions import generate_v3_pool_address
from degenbot.uniswap.v3.tick_lens import TickLens
from degenbot.uniswap.v3.v3_liquidity_pool import V3LiquidityPool

_INIT_HASHES_BY_FACTORY = {
    1: {
        # Sushiswap (V2)
        "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac": "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
        # Uniswap (V2)
        "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f": "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
        # Uniswap (V3)
        "0x1F98431c8aD98523631AE4a59f267346ea31F984": "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
        # Sushiswap (V3)
        "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F": "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54",
    },
}


class UniswapLiquidityPoolManager(Manager):
    """
    Single-concern base class to allow derived classes to share state
    """

    _state: dict = {}

    def __init__(self, factory_address: str, chain_id: int):
        # the internal state data for all child objects is held in the
        # class-level _state dictionary, keyed by chain ID and factory address
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
            self.factory_address = factory_address
            self._factory_contract = Contract.from_abi(
                name="Uniswap V2: Factory",
                address=factory_address,
                abi=UNISWAPV2_FACTORY_ABI,
            )
            self._lock = Lock()
            self._pools_by_address: Dict[str, LiquidityPool] = dict()
            self._pools_by_tokens: Dict[
                Tuple[str, str], LiquidityPool
            ] = dict()
            self._token_manager = self._state[chain_id]["erc20token_manager"]
            self.factory_init_hash = _INIT_HASHES_BY_FACTORY[chain_id][
                self.factory_address
            ]

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

        elif token_addresses is not None:
            if len(token_addresses) != 2:
                raise ValueError(
                    f"Expected two tokens, found {len(token_addresses)}"
                )

            try:
                erc20token_helpers: Tuple[Erc20Token, Erc20Token]
                erc20token_helpers = tuple(
                    (
                        self._token_manager.get_erc20token(
                            address=token_address,
                            min_abi=True,
                            silent=silent,
                            unload_brownie_contract_after_init=True,
                        )
                        for token_address in token_addresses
                    )
                )  # type: ignore
            except Erc20TokenError:
                raise ManagerError(
                    f"Could not build Erc20Token helpers for pool {pool_address}"
                )

            # dictionary key pair is sorted by address
            erc20token_helpers = (
                min(erc20token_helpers),
                max(erc20token_helpers),
            )

            tokens_key: Tuple[str, str]
            tokens_key = tuple([token.address for token in erc20token_helpers])  # type: ignore

            try:
                pool_helper = self._pools_by_tokens[tokens_key]
            except KeyError:
                pass
            else:
                return pool_helper

            if (
                pool_address := self._factory_contract.getPair(*tokens_key)
            ) == ZERO_ADDRESS:
                raise ManagerError("No V2 LP available")

            try:
                pool_helper = LiquidityPool(
                    address=pool_address,
                    tokens=list(erc20token_helpers),
                    silent=silent,
                    update_method=update_method,
                    factory_address=self.factory_address,
                    factory_init_hash=self.factory_init_hash,
                )
            except Exception as e:
                raise ManagerError(
                    f"Could not build V2 pool: {pool_address=}: {e}"
                )

            with self._lock:
                self._pools_by_address[pool_address] = pool_helper
                self._pools_by_tokens[tokens_key] = pool_helper

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
            self._factory_contract = Contract.from_abi(
                name="Uniswap V3: Factory",
                address=factory_address,
                abi=UNISWAP_V3_FACTORY_ABI,
            )
            self._lens = TickLens()
            self._lock = Lock()
            self._pools_by_address: Dict[str, V3LiquidityPool] = {}
            self._pools_by_tokens_and_fee: Dict[
                Tuple[str, str, int], V3LiquidityPool
            ] = {}
            self._token_manager = self._state[chain_id]["erc20token_manager"]

    def get_pool(
        self,
        pool_address: Optional[str] = None,
        token_addresses: Optional[Tuple[str, str]] = None,
        pool_fee: Optional[int] = None,
        silent: bool = False,
        # accept any number of keyword arguments, which are
        # passed directly to the `V3LiquidityPool` constructor without validation
        **kwargs,
    ) -> V3LiquidityPool:
        """
        Get the pool object from its address, or a tuple of token addresses and fee
        """

        if not (pool_address is None) ^ (
            token_addresses is None and pool_fee is None
        ):
            raise ValueError(
                f"Insufficient arguments provided. Pass address OR tokens+fee"
            )

        dict_key: tuple[str, str, int]

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
                    **kwargs,
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
                    silent=silent,
                )
            except:
                raise ManagerError(
                    f"Could not build V3 pool: {pool_address=}, {pool_fee=}"
                )

            with self._lock:
                self._pools_by_address[pool_address] = pool_helper
                self._pools_by_tokens_and_fee[dict_key] = pool_helper

        return pool_helper
