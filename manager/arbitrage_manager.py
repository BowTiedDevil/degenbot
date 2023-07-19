from threading import Lock
from typing import Dict, List, Optional, Union

from web3 import Web3

from degenbot import Erc20TokenHelperManager
from degenbot.arbitrage.uniswap_lp_cycle import UniswapLpCycle
from degenbot.constants import WRAPPED_NATIVE_TOKENS
from degenbot.exceptions import ManagerError
from degenbot.token import Erc20Token
from degenbot.types import ArbitrageHelper, HelperManager
from degenbot.uniswap.uniswap_managers import (
    UniswapV2LiquidityPoolManager,
    UniswapV3LiquidityPoolManager,
)
from degenbot.uniswap.v2 import LiquidityPool
from degenbot.uniswap.v3 import V3LiquidityPool


class ArbitrageHelperManager(HelperManager):
    """
    A class that generates and tracks Arbitrage helpers

    The dictionary of arbitrage helpers is held as a class attribute, so all manager
    objects reference the same state data
    """

    _state: Dict = {}

    def __init__(self, chain_id: int):
        # the internal state data for this object is held in the
        # class-level _state dictionary, keyed by the chain ID
        if self._state.get(chain_id):
            self.__dict__ = self._state[chain_id]
        else:
            self._state[chain_id] = {}
            self.__dict__ = self._state[chain_id]

            # initialize internal attributes
            self._arbs: Dict = {}  # all known arbs, keyed by id
            self._blacklisted_ids: set = set()
            self._chain_id = chain_id
            self._erc20tokenmanager = Erc20TokenHelperManager(chain_id)
            self._lock = Lock()
            self._v2_pool_managers: Dict[
                str, UniswapV2LiquidityPoolManager
            ] = {}  # all V2 pool managers, keyed by factory address
            self._v3_pool_managers: Dict[
                str, UniswapV3LiquidityPoolManager
            ] = {}  # all V3 pool managers, keyed by factory address

    def add_pool_manager(self, factory_address: str, uniswap_version: int):
        """
        Create a Uniswap pool manager from the factory contract address and version, store in the internal dictionary of pool managers
        """

        if (
            factory_address in self._v2_pool_managers
            or factory_address in self._v3_pool_managers
        ):
            raise ValueError(
                f"Pool manager for factory={factory_address} already exists"
            )

        if uniswap_version == 2:
            self._v2_pool_managers[
                factory_address
            ] = UniswapV2LiquidityPoolManager(factory_address)
        elif uniswap_version == 3:
            self._v3_pool_managers[
                factory_address
            ] = UniswapV3LiquidityPoolManager(factory_address)
        else:
            raise ValueError

    def build(
        self,
        arb_type: str,
        swap_pools: Union[
            List[Union[LiquidityPool, V3LiquidityPool]],
            List[str],
        ],
        update_method: str = "polling",
        input_token: Optional[Union[str, Erc20Token]] = None,
    ) -> ArbitrageHelper:
        """
        Returns the arb helper
        """

        native_wrapped_token_address = WRAPPED_NATIVE_TOKENS[self._chain_id]

        print(native_wrapped_token_address)

        input_token = self._erc20tokenmanager.get_erc20token(
            native_wrapped_token_address
        )

        _swap_pools: List[Union[LiquidityPool, V3LiquidityPool]] = []

        # replace all pool addresses with helper objects
        for i, pool in enumerate(swap_pools):
            if isinstance(pool, str):
                # if an address was provided, get the pool helper object
                pool_helper: Union[LiquidityPool, V3LiquidityPool]

                # iterate through the pool managers (may be multiple compatible DEX on one chain)
                for v2_pool_manager in self._v2_pool_managers.values():
                    try:
                        pool_helper = v2_pool_manager.get_pool(pool)
                    except:
                        pass
                for v3_pool_manager in self._v3_pool_managers.values():
                    try:
                        pool_helper = v3_pool_manager.get_pool(pool)
                    except:
                        pass
                try:
                    pool_helper
                except:
                    # will throw if the pool helper could not be found
                    raise ValueError(
                        f"Could not generate Uniswap LP helper for pool {pool}"
                    )
                else:
                    print(pool_helper)
            elif isinstance(pool, (LiquidityPool, V3LiquidityPool)):
                # otherwise, use the helper directly
                pool_helper = pool
            else:
                raise TypeError(
                    f"Pool {pool} is {type(pool)}! Expected LiquidityPool, V3LiquidityPool, or string"
                )

            _swap_pools[i] = pool_helper

        arb_id = Web3.keccak(
            hexstr="".join([pool.address[2:] for pool in _swap_pools])
        ).hex()

        # check if the helper is already known, throw exception if so
        try:
            arb_helper = self._arbs[arb_id]
        except KeyError:
            pass
        else:
            raise ValueError(f"Arbitrage helper already exists")

        arb_helper = UniswapLpCycle(
            input_token=input_token,
            swap_pools=_swap_pools,
            max_input=None,
            id=arb_id,
        )
        return arb_helper

    def get(
        self,
        arb_id: str,
        arb_type: str,
        chain_id: int,
        input_token: Optional[Union[str, Erc20Token]] = None,
        update_method: str = "polling",
    ) -> ArbitrageHelper:
        """
        Get an arbitrage path object from its ID and the type. An ID is a keccak address of all pool addresses, in order.

        Type can only be "cycle", but additional types will be added later
        """

        # attempt to retrieve the arb (might already exist)
        try:
            arb_helper = self._arbs[arb_id][arb_type]
        except KeyError:
            pass
        else:
            return arb_helper

        # otherwise create a new arb helper
        try:
            if arb_type == "cycle":
                if input_token is None:
                    native_wrapped_token_address = WRAPPED_NATIVE_TOKENS[
                        chain_id
                    ]
                    input_token = self._erc20tokenmanager.get_erc20token(
                        native_wrapped_token_address
                    )
                elif not isinstance(input_token, Erc20Token):
                    input_token = self._erc20tokenmanager.get_erc20token(
                        input_token
                    )
                # arb_helper = UniswapLpCycle(input_token=input_token)
                arb_helper = self.build(
                    arb_type=arb_type,
                    # TODO: read pools from a file,
                    # swap_pools=XXX,
                    update_method=update_method,
                    input_token=input_token,
                )
        except:
            raise ManagerError(f"Could not create Arbitrage helper: {arb_id=}")
        else:
            return arb_helper
