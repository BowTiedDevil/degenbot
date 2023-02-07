from brownie import Contract
from typing import Tuple, Union
from web3 import Web3

from degenbot.manager import Manager
from degenbot.constants import ZERO_ADDRESS
from degenbot.manager import Erc20TokenHelperManager
from degenbot.exceptions import (
    LiquidityPoolError,
    Erc20TokenError,
    ManagerError,
)
from degenbot.token import Erc20Token
from degenbot.uniswap.functions import generate_v3_pool_address
from degenbot.uniswap.v2.abi import UNISWAPV2_FACTORY_ABI
from degenbot.uniswap.v2 import LiquidityPool
from degenbot.uniswap.v3 import TickLens, V3LiquidityPool
from degenbot.uniswap.v3.abi import UNISWAP_V3_FACTORY_ABI


class UniswapLiquidityPoolManager(Manager):
    """
    Single-concern class to allow V2 and V3 managers to share a token manager
    """

    _token_manager = Erc20TokenHelperManager()


class UniswapV2LiquidityPoolManager(UniswapLiquidityPoolManager):
    """
    A class that generates and tracks Uniswap V2 liquidity pool helpers

    The dictionaries of pool helpers are held as a class attribute, so all manager
    objects reference the same state data
    """

    # _token_manager = Erc20TokenHelperManager()
    _state = {}

    def __init__(
        self,
        factory_address,
        erc20token_manager: Erc20TokenHelperManager = None,
    ):

        if self._state.get(factory_address):
            self.__dict__ = self._state[factory_address]
        else:
            self._state[factory_address] = {}
            self.__dict__ = self._state[factory_address]

        if erc20token_manager is not None:
            self.erc20token_manager = erc20token_manager
        else:
            self.erc20token_manager = Erc20TokenHelperManager()

        self.factory_contract = Contract.from_abi(
            name="Uniswap V2: Factory",
            address=factory_address,
            abi=UNISWAPV2_FACTORY_ABI,
        )

        self.pools_by_address = {}
        self.pools_by_tokens = {}

    def get_pool(
        self,
        pool_address: str = None,
        token_addresses: Tuple[str] = None,
    ) -> LiquidityPool:
        """
        Get the pool object from its address, or a tuple of token addresses
        """

        if pool_address is not None:
            pool_address = Web3.toChecksumAddress(pool_address)

            if pool_helper := self.pools_by_address.get(pool_address):
                return pool_helper

            try:
                pool_helper = LiquidityPool(address=pool_address)
            except:
                raise
            else:
                self.pools_by_address[pool_address] = pool_helper
                self.pools_by_tokens[
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
                            address=token_address
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

            if pool_helper := self.pools_by_tokens.get(tokens_key):
                return pool_helper

            if (
                pool_address := self.factory_contract.getPair(*tokens_key)
            ) == ZERO_ADDRESS:
                raise LiquidityPoolError("No V2 LP deployed")

            try:
                pool_helper = LiquidityPool(
                    address=pool_address,
                    tokens=erc20token_helpers,
                )
            except:
                raise
            else:
                self.pools_by_address[pool_address] = pool_helper
                self.pools_by_tokens[tokens_key] = pool_helper
                return pool_helper


class UniswapV3LiquidityPoolManager(UniswapLiquidityPoolManager):
    """
    A class that generates and tracks Uniswap V3 liquidity pool helpers

    The dictionaries of pool helpers are held as a class attribute, so all manager
    objects reference the same state data
    """

    # _token_manager = Erc20TokenHelperManager()
    _state = {}

    def __init__(
        self,
        factory_address,
        erc20token_manager: Erc20TokenHelperManager = None,
    ):

        if self._state.get(factory_address):
            self.__dict__ = self._state[factory_address]
        else:
            self._state[factory_address] = {}
            self.__dict__ = self._state[factory_address]

        if erc20token_manager is not None:
            self.erc20token_manager = erc20token_manager
        else:
            self.erc20token_manager = Erc20TokenHelperManager()

        self.factory_contract = Contract.from_abi(
            name="Uniswap V3: Factory",
            address=factory_address,
            abi=UNISWAP_V3_FACTORY_ABI,
        )

        self.lens = TickLens()
        self.pools_by_address = {}
        self.pools_by_tokens_and_fee = {}

    def get_pool(
        self,
        pool_address: str = None,
        token_addresses: Tuple[str] = None,
        pool_fee: int = None,
    ) -> V3LiquidityPool:
        """
        Get the pool object from its address, or a tuple of token addresses and fee
        """

        if pool_address is not None:
            if token_addresses is not None or pool_fee is not None:
                raise ValueError(
                    f"Conflicting arguments provided. Pass address OR tokens+fee"
                )

            pool_address = Web3.toChecksumAddress(pool_address)

            if pool_helper := self.pools_by_address.get(pool_address):
                return pool_helper
            else:

                pool_helper = V3LiquidityPool(
                    address=pool_address, lens=self.lens
                )
                self.pools_by_address[pool_address] = pool_helper
                dict_key = *token_addresses, pool_fee
                self.pools_by_tokens_and_fee[dict_key] = pool_helper
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
                            address=token_address, silent=True
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

            if pool_helper := self.pools_by_tokens_and_fee.get(dict_key):
                return pool_helper
            else:
                pool_address = generate_v3_pool_address(
                    token_addresses=tokens_key, fee=pool_fee
                )
                if pool_helper := self.pools_by_address.get(pool_address):
                    return pool_helper
                else:
                    pool_helper = V3LiquidityPool(
                        address=pool_address,
                        lens=self.lens,
                        tokens=erc20token_helpers,
                    )
                    self.pools_by_address[pool_address] = pool_helper
                    self.pools_by_tokens_and_fee[dict_key] = pool_helper
                    return pool_helper
        else:
            raise ValueError(
                f"Insufficient arguments provided. Pass address OR tokens+fee"
            )
