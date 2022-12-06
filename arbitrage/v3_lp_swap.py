from typing import List, Tuple, Union

from scipy import optimize

from degenbot.arbitrage.base import Arbitrage
from degenbot.exceptions import (
    ArbCalculationError,
    InvalidSwapPathError,
    DegenbotError,
)
from degenbot.token import Erc20Token
from degenbot.uniswap.v2 import LiquidityPool
from degenbot.uniswap.v3 import V3LiquidityPool


class V3LpSwap(Arbitrage):
    def __init__(
        self,
        input_token: Erc20Token,
        swap_pools: List[Union[LiquidityPool, V3LiquidityPool]],
        name: str = "",
        max_input: int = None,
        id: str = None,
    ):

        if id:
            self.id = id
        if name:
            self.name = name
        self.input_token = input_token
        self.swap_pools = swap_pools
        self.max_input = max_input
        self.gas_estimate = 0

    @classmethod
    def from_addresses(
        cls,
        input_token_address: str,
        swap_pool_addresses: List[Tuple[str, str]],
        name: str = "",
        max_input: int = None,
        id: str = None,
    ) -> "V3LpSwap":
        """
        Create a new `V3LpSwap` object from token and pool addresses.

        Arguments
        ---------
        input_token_address : str
            A address for the input_token
        swap_pool_addresses : List[str]
            An ordered list of tuples, representing the address for each pool in the swap path,
            and a string specifying the Uniswap version for that pool (either "V2" or "V3")

            e.g. swap_pool_addresses = [
                ("0xCBCdF9626bC03E24f779434178A73a0B4bad62eD","V3"),
                ("0xbb2b8038a1640196fbe3e38816f3e67cba72d940","V2")
            ]

        name : str, optional
            The display name for the helper
        max_input: int, optional
            The maximum input for the cycle token in question
            (typically limited by the balance of the deployed contract or operating EOA)
        id: str, optional
            A unique identifier for bookkeeping purposes
            (not used internally, the attribute is provided for operator convenience)
        """

        # create the token object
        try:
            token = Erc20Token(input_token_address)
        except:
            raise

        # create the pool objects
        pool_objects = []
        for pool_address, pool_type in swap_pool_addresses:
            # determine if the pool is a V2 or V3 type
            if pool_type == "V2":
                pool_objects.append(LiquidityPool(address=pool_address))
            elif pool_type == "V3":
                pool_objects.append(V3LiquidityPool(address=pool_address))
            else:
                raise DegenbotError(
                    f"Pool type not understood! Expected 'V2' or 'V3', got {pool_type}"
                )

        return cls(
            input_token=token,
            swap_pools=pool_objects,
            name=name,
            max_input=max_input,
            id=id,
        )

    def auto_update(self):
        for pool in self.swap_pools:
            if pool.uniswap_version == 2:
                # TODO: V2 pools can be polled or externally driven, need to implement a
                # more robust check that handles externally-updated pools gracefully
                if pool._update_method != "external":
                    pool.update_reserves()
            elif pool.uniswap_version == 3:
                pool.auto_update()
            else:
                raise DegenbotError(f"WTF? Could not identify version for pool {pool}")

    def calculate_arbitrage(self) -> Tuple[bool, Tuple[int,int]]:
        # cap the amount to be swapped
        bounds = (1, self.max_input)

        if self.input_token == self.swap_pools[0].token0:
            forward_token = self.swap_pools[0].token1
        elif self.input_token == self.swap_pools[0].token1:
            forward_token = self.swap_pools[0].token0
        else:
            print("calculate_arbitrage: WTF? Could not identify input token")
            raise ArbCalculationError

        try:

            def arb_profit(x):
                x = int(x)
                return -float(
                    self.swap_pools[1].calculate_tokens_out_from_tokens_in(
                        token_in=forward_token,
                        token_in_quantity=self.swap_pools[
                            0
                        ].calculate_tokens_out_from_tokens_in(
                            token_in=self.input_token,
                            token_in_quantity=x,
                        ),
                    )
                    - x
                )

            opt = optimize.minimize_scalar(
                arb_profit,
                method="bounded",
                bounds=bounds,
                options={"xatol": 1.0},
            )
        except Exception as e:
            print(e)
            print(f"bounds: {bounds}")
            raise ArbCalculationError
        else:
            swap_amount = int(opt.x)
            best_profit = -int(opt.fun)

        forward_amount = self.swap_pools[0].calculate_tokens_out_from_tokens_in(
            token_in=self.input_token, token_in_quantity=swap_amount
        )
        ending_amount = self.swap_pools[1].calculate_tokens_out_from_tokens_in(
            token_in=forward_token, token_in_quantity=forward_amount
        )

        if best_profit > 0:
            return True, (swap_amount, best_profit)
        else:
            return False, ()

        print()
        print(
            f"pool 0: swap {swap_amount} {self.input_token.symbol} for {forward_amount} {forward_token}"
        )
        print(
            f"pool 1: swap {forward_amount} {forward_token} for {ending_amount} {self.input_token.symbol}"
        )
        print(f"profit: {best_profit/10**self.input_token.decimals}")
