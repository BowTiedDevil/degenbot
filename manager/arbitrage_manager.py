from degenbot.arbitrage import Arbitrage, UniswapLpCycle
from degenbot.exceptions import ManagerError
from .token_manager import Erc20TokenHelperManager
from .base import Manager
from degenbot.token import Erc20Token
from degenbot.uniswap.v2 import LiquidityPool
from degenbot.uniswap.v3 import V3LiquidityPool
from degenbot.uniswap.manager import (
    UniswapV2LiquidityPoolManager,
    UniswapV3LiquidityPoolManager,
)


from threading import Lock
from typing import Optional, Union, List
from web3 import Web3


class ArbitrageHelperManager(Manager):
    """
    A class that generates and tracks Arbitrage helpers

    The dictionary of arbitrage helpers is held as a class attribute, so all manager
    objects reference the same state data
    """

    _state = {}

    # a dictionary of contract addresses for the native blockchain token,
    # keyed by chain ID
    WRAPPED_NATIVE_TOKENS = {
        # Ethereum (ETH)
        1: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        # Arbitrum (AETH)
        42161: "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
    }

    def __init__(
        self,
        chain_id: int,
    ):
        if self._state.get(chain_id):
            self.__dict__ = self._state[chain_id]
        else:
            self._state[chain_id] = {}
            self.__dict__ = self._state[chain_id]

        self.chain_id = chain_id

        # a reference to all known arbs, keyed by id
        self.arbs = {}

        # a reference to all V2 pool managers, keyed by factory address
        self.v2_pool_managers = {}

        # a reference to all V3 pool managers, keyed by factory address
        self.v3_pool_managers = {}

        self.erc20tokenmanager = Erc20TokenHelperManager()

        self.blacklisted_ids = set()

        self.lock = Lock()

    def add_factory(self, factory_address: str, uniswap_version: int):
        if uniswap_version == 2:
            self.v2_pool_managers[
                factory_address
            ] = UniswapV2LiquidityPoolManager(factory_address)
        elif uniswap_version == 3:
            self.v3_pool_managers[
                factory_address
            ] = UniswapV3LiquidityPoolManager(factory_address)
        else:
            raise ValueError

    def build(
        self,
        arb_type: str,
        update_method: str = "polling",
        chain_id: Optional[int] = None,
        input_token: Optional[Union[str, Erc20Token]] = None,
        swap_pools: Union[
            List[Union[LiquidityPool, V3LiquidityPool]],
            List[str],
        ] = None,
    ):

        native_wrapped_token_address = self.WRAPPED_NATIVE_TOKENS[
            self.chain_id
        ]

        print(native_wrapped_token_address)

        input_token = self.erc20tokenmanager.get_erc20token(
            native_wrapped_token_address
        )

        for i, pool in enumerate(swap_pools):

            if type(pool) not in (str, LiquidityPool, V3LiquidityPool):
                raise TypeError(
                    f"Pool {pool} is {type(pool)}! Expected LiquidityPool, V3LiquidityPool, or string"
                )

            if type(pool) == str:
                pool_address = pool
                pool_helper = None
                for v2_pool_manager in self.v2_pool_managers.values():
                    try:
                        _pool_helper = v2_pool_manager.get_pool(pool_address)
                    except:
                        pass
                    else:
                        pool_helper = _pool_helper
                for v3_pool_manager in self.v3_pool_managers.values():
                    try:
                        _pool_helper = v3_pool_manager.get_pool(pool_address)
                    except:
                        pass
                    else:
                        pool_helper = _pool_helper

                print(pool_helper)

                if pool_helper is None:
                    raise ValueError(
                        f"Could not generate Uniswap LP helper for pool {pool}"
                    )
                else:
                    swap_pools[i] = pool_helper

        arb_id = Web3.keccak(
            hexstr="".join([pool.address[2:] for pool in swap_pools])
        ).hex()

        try:
            arb_helper = self.arbs[arb_id]
        except KeyError:
            arb_helper = UniswapLpCycle(
                input_token=input_token,
                swap_pools=swap_pools,
                max_input=None,
                id=arb_id,
            )

            with self.lock:
                self.arbs[arb_id] = arb_helper

        return arb_helper

    def get(
        self,
        arb_id: str,
        arb_type: str,
        chain_id: Optional[int] = None,
        input_token: Optional[Union[str, Erc20Token]] = None,
        update_method: str = "polling",
    ) -> "Arbitrage":
        """
        Get an arbitrage path object from its ID. An ID is a keccak address of all pool addresses, in order.
        """

        if arb_helper := self.arbs.get(arb_id):
            return arb_helper

        # # identify the arb from the dict of known IDs
        # try:
        #     if arb_type == "cycle":
        #         if input_token is None:
        #             native_wrapped_token_address = self.WRAPPED_NATIVE_TOKENS[
        #                 chain_id
        #             ]
        #             input_token = self.erc20tokenmanager.get_erc20token(
        #                 native_wrapped_token_address
        #             )
        #         elif not isinstance(input_token, Erc20Token):
        #             input_token = self.erc20tokenmanager.get_erc20token(
        #                 input_token
        #             )
        #         arb_helper = UniswapLpCycle(input_token=input_token)
        # except:
        #     raise ManagerError(f"Could not create Arbitrage helper: {arb_id=}")

        return arb_helper
