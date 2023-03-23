from threading import Lock
from typing import Optional, Tuple, Union

from brownie import Contract, chain
from web3 import Web3

from degenbot.constants import ZERO_ADDRESS
from degenbot.exceptions import Erc20TokenError, ManagerError
from degenbot.manager.base import Manager
from degenbot.manager.token_manager import Erc20TokenHelperManager
from degenbot.uniswap.functions import generate_v3_pool_address
from degenbot.uniswap.v2.liquidity_pool import LiquidityPool
from degenbot.uniswap.v2.abi import UNISWAPV2_FACTORY_ABI
from degenbot.uniswap.v3.tick_lens import TickLens
from degenbot.uniswap.v3.v3_liquidity_pool import V3LiquidityPool
from degenbot.uniswap.v3.abi import UNISWAP_V3_FACTORY_ABI


class UniswapLiquidityPoolManager(Manager):
    """
    Single-concern base class to allow derived classes to share state
    """

    _state = {}


class UniswapV2LiquidityPoolManager(UniswapLiquidityPoolManager):
    """
    A class that generates and tracks Uniswap V2 liquidity pool helpers

    The state dictionary is held using the "Borg" singleton pattern, which
    ensures that all instances of the class have access to the same state data
    """

    def __init__(self, factory_address: str):

        # the internal state data for this object is held in the
        # class-level _state dictionary, keyed by the factory address
        if self._state.get(factory_address):
            self.__dict__ = self._state[factory_address]
        else:
            self._state[factory_address] = {}
            self.__dict__ = self._state[factory_address]

            # initialize internal attributes
            self._factory_contract = Contract.from_abi(
                name="Uniswap V2: Factory",
                address=factory_address,
                abi=UNISWAPV2_FACTORY_ABI,
            )
            self._lock = Lock()
            self._pools_by_address = {}
            self._pools_by_tokens = {}

            self._token_manager = Erc20TokenHelperManager(chain.id)

    def get_pool(
        self,
        pool_address: Optional[str] = None,
        token_addresses: Optional[Tuple[str]] = None,
        silent: bool = False,
        update_method: str = "polling",
    ) -> LiquidityPool:
        """
        Get the pool object from its address, or a tuple of token addresses
        """

        if pool_address is not None:

            pool_address = Web3.toChecksumAddress(pool_address)

            if pool_helper := self._pools_by_address.get(pool_address):
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

            return pool_helper

        elif token_addresses is not None:

            if len(token_addresses) != 2:
                raise ValueError(
                    f"Expected two tokens, found {len(token_addresses)}"
                )

            try:
                erc20token_helpers = tuple(
                    [
                        self._token_manager.get_erc20token(
                            address=token_address,
                            min_abi=True,
                            silent=silent,
                            unload_brownie_contract_after_init=True,
                        )
                        for token_address in token_addresses
                    ]
                )
            except Erc20TokenError:
                raise ManagerError(
                    f"Could not build Erc20Token helpers for pool {pool_address}"
                )

            # dictionary key pair is sorted by address
            erc20token_helpers = (
                min(erc20token_helpers),
                max(erc20token_helpers),
            )
            tokens_key = tuple([token.address for token in erc20token_helpers])

            if pool_helper := self._pools_by_tokens.get(tokens_key):
                return pool_helper

            if (
                pool_address := self._factory_contract.getPair(*tokens_key)
            ) == ZERO_ADDRESS:
                raise ManagerError("No V2 LP available")

            try:
                pool_helper = LiquidityPool(
                    address=pool_address,
                    tokens=erc20token_helpers,
                    silent=silent,
                    update_method=update_method,
                )
            except:
                raise ManagerError(f"Could not build V2 pool: {pool_address=}")

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

    def __init__(
        self,
        factory_address,
    ):

        # the internal state data for this object is held in the
        # class-level _state dictionary, keyed by the factory address
        if self._state.get(factory_address):
            self.__dict__ = self._state[factory_address]
        else:
            self._state[factory_address] = {}
            self.__dict__ = self._state[factory_address]

            # initialize internal attributes
            self._factory_contract = Contract.from_abi(
                name="Uniswap V3: Factory",
                address=factory_address,
                abi=UNISWAP_V3_FACTORY_ABI,
            )
            self._lens = TickLens()
            self._lock = Lock()
            self._pools_by_address = {}
            self._pools_by_tokens_and_fee = {}
            self._token_manager = Erc20TokenHelperManager(chain.id)

    def get_pool(
        self,
        pool_address: Optional[str] = None,
        token_addresses: Optional[Tuple[str]] = None,
        pool_fee: Optional[int] = None,
        silent: bool = False,
        update_method: str = "polling",
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

        if pool_address is not None:
            if token_addresses is not None or pool_fee is not None:
                raise ValueError(
                    f"Conflicting arguments provided. Pass address OR tokens+fee"
                )

            pool_address = Web3.toChecksumAddress(pool_address)

            if pool_helper := self._pools_by_address.get(pool_address):
                return pool_helper

            try:
                pool_helper = V3LiquidityPool(
                    address=pool_address,
                    lens=self._lens,
                    silent=silent,
                    update_method=update_method,
                )
            except Exception as e:
                raise ManagerError(
                    f"Could not build V3 pool: {pool_address=}: {e}"
                ) from e

            token_addresses = (
                pool_helper.token0.address,
                pool_helper.token1.address,
            )

            with self._lock:
                dict_key = *token_addresses, pool_fee
                self._pools_by_address[pool_address] = pool_helper
                self._pools_by_tokens_and_fee[dict_key] = pool_helper

            return pool_helper

        elif token_addresses is not None and pool_fee is not None:

            if len(token_addresses) != 2:
                raise ValueError(
                    f"Expected two tokens, found {len(token_addresses)}"
                )

            try:
                erc20token_helpers = tuple(
                    [
                        self._token_manager.get_erc20token(
                            address=token_address,
                            min_abi=True,
                            silent=silent,
                            unload_brownie_contract_after_init=True,
                        )
                        for token_address in token_addresses
                    ]
                )
            except Erc20TokenError:
                raise ManagerError("Could not build Erc20Token helpers")

            # dictionary key pair is sorted by address
            erc20token_helpers = (
                min(erc20token_helpers),
                max(erc20token_helpers),
            )
            tokens_key = tuple([token.address for token in erc20token_helpers])
            dict_key = *tokens_key, pool_fee

            if pool_helper := self._pools_by_tokens_and_fee.get(dict_key):
                return pool_helper

            pool_address = generate_v3_pool_address(
                token_addresses=tokens_key, fee=pool_fee
            )

            if pool_helper := self._pools_by_address.get(pool_address):
                return pool_helper

            try:
                pool_helper = V3LiquidityPool(
                    address=pool_address,
                    lens=self._lens,
                    tokens=erc20token_helpers,
                    silent=silent,
                )
            except:
                raise ManagerError(f"Could not build V3 pool: {pool_address=}")

            with self._lock:
                self._pools_by_address[pool_address] = pool_helper
                self._pools_by_tokens_and_fee[dict_key] = pool_helper

            return pool_helper
