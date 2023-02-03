from typing import Tuple, Union


from degenbot.base import Manager
from degenbot.token import Erc20Token
from degenbot.uniswap.functions import generate_v3_pool_address
from degenbot.uniswap.v2 import LiquidityPool
from degenbot.uniswap.v3 import TickLens, V3LiquidityPool


class UniswapLiquidityPoolManager(Manager):
    """
    A class that generates and tracks Uniswap V2 & V3 liquidity pool helpers
    """

    def __init__(self):

        self.lens = TickLens()

        self.v2_pools_by_address = {}
        self.v2_pools_by_tokens = {}

        self.v3_pools_by_address = {}
        self.v3_pools_by_tokens_and_fee = {}

    def get_v2_pool(
        self,
        address: str = None,
        tokens: Tuple[Union[LiquidityPool, V3LiquidityPool], str] = None,
    ) -> Union[LiquidityPool, V3LiquidityPool]:
        """
        Get the pool object from its address, or a tuple of token objects or addresses
        """
        pass

    def get_v3_pool(
        self,
        address: str = None,
        tokens: Tuple[Erc20Token, str] = None,
        fee: int = None,
    ) -> Union[LiquidityPool, V3LiquidityPool]:
        """
        Get the pool object from its address, or a tuple of token objects or addresses and its fee
        """

        if address is not None:
            if tokens is not None or fee is not None:
                raise ValueError(
                    f"Conflicting arguments provided. Pass address OR tokens+fee"
                )

            if pool_helper := self.v3_pools_by_address.get(address):
                return pool_helper
            else:
                pool_helper = V3LiquidityPool(address=address, lens=self.lens)
                self.v3_pools_by_address[address] = pool_helper
                self.v3_pools_by_tokens_and_fee[
                    tuple(
                        set(
                            pool_helper.token0.address,
                            pool_helper.token1.address,
                        ),
                        fee,
                    )
                ] = pool_helper
                return pool_helper

        elif tokens is not None and fee is not None:

            if len(tokens) != 2:
                raise ValueError(f"Expected two tokens, found {len(tokens)}")

            token_addresses = []
            for token in tokens:
                if not isinstance(token, (Erc20Token, str)):
                    raise ValueError(f"Expected str or Erc20Token, found {repr(token)}")
                if isinstance(token, Erc20Token):
                    token_addresses.append(token.address)
                else:
                    token_addresses.append(token)

            # sort the token addresses
            token_addresses = (min(token_addresses), max(token_addresses))

            if pool_helper := self.v3_pools_by_tokens_and_fee.get((*token_addresses,fee)):
                return pool_helper
            else:
                pool_address = generate_v3_pool_address(
                    token_addresses=token_addresses, fee=fee
                )
                if pool_helper := self.v3_pools_by_address.get(pool_address):
                    return pool_helper
                else:
                    pool_helper = V3LiquidityPool(address=pool_address, lens=self.lens)
                    self.v3_pools_by_address[pool_address] = pool_helper
                    self.v3_pools_by_tokens_and_fee[token_addresses,fee] = pool_helper
                    return pool_helper
        else:
            raise ValueError(
                f"Insufficient arguments provided. Pass address OR tokens+fee"
            )
