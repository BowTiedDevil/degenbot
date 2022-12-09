from typing import List, Tuple, Union

from scipy import optimize

from degenbot.arbitrage.base import Arbitrage
from degenbot.exceptions import (
    ArbCalculationError,
    DegenbotError,
    InvalidSwapPathError,
)
from degenbot.token import Erc20Token
from degenbot.uniswap.v2 import LiquidityPool
from degenbot.uniswap.v3 import V3LiquidityPool
from degenbot.uniswap.v3.libraries import TickMath


class UniswapLpCycle(Arbitrage):
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
        self.max_input = max_input
        self.gas_estimate = 0

        for pool in swap_pools:
            assert pool.uniswap_version in [2, 3], DegenbotError(
                f"WTF? Could not identify version for pool {pool}"
            )
        self.swap_pools = swap_pools

        self.swap_pool_addresses = [pool.address for pool in self.swap_pools]
        self.swap_pool_tokens = [
            [pool.token0, pool.token1] for pool in self.swap_pools
        ]

        # WIP: add state tracking for all pools. Populated from relevant state data from each pool, retrieved directly from pool.state attribute
        self.pool_states = {}

        self.best = {
            "init": True,
            "strategy": "cycle",
            "swap_amount": 0,
            "input_token": self.input_token,
            "profit_amount": 0,
            "profit_token": self.input_token,
            "swap_pools": self.swap_pools,
            "swap_pool_addresses": self.swap_pool_addresses,
            "swap_pool_amounts": [],
            "swap_pool_tokens": self.swap_pool_tokens,
        }

    @classmethod
    def from_addresses(
        cls,
        input_token_address: str,
        swap_pool_addresses: List[Tuple[str, str]],
        name: str = "",
        max_input: int = None,
        id: str = None,
    ) -> "UniswapLpCycle":
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

    def _update_pool_states(self):
        """
        Internal method to update the `self.pool_states` state tracking dict
        """
        self.pool_states = {
            pool.address: pool.state for pool in self.swap_pools
        }

    def auto_update(
        self,
        silent=True,
    ):
        for pool in self.swap_pools:
            if pool.uniswap_version == 2 and pool._update_method != "external":
                # TODO: V2 pools can updated via polling, or via external updates. Need to implement a
                # more robust check that gracefully handles externally-updated pools
                pool.update_reserves(silent=silent)
            elif (
                pool.uniswap_version == 3 and pool._update_method != "external"
            ):
                pool.auto_update(silent=silent)

    def calculate_arbitrage(
        self, verbose=False
    ) -> Tuple[bool, Tuple[int, int]]:

        # short-circuit to avoid arb recalc if pool states have not changed:
        if self.pool_states == {
            pool.address: pool.state for pool in self.swap_pools
        }:
            return False, ()
        else:
            self._update_pool_states()

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

        # forward_amount = self.swap_pools[
        #     0
        # ].calculate_tokens_out_from_tokens_in(
        #     token_in=self.input_token, token_in_quantity=swap_amount
        # )
        # ending_amount = self.swap_pools[1].calculate_tokens_out_from_tokens_in(
        #     token_in=forward_token, token_in_quantity=forward_amount
        # )

        # print()
        # print(
        #     f"pool 0: swap {swap_amount} {self.input_token.symbol} for {forward_amount} {forward_token}"
        # )
        # print(
        #     f"pool 1: swap {forward_amount} {forward_token} for {ending_amount} {self.input_token.symbol}"
        # )
        # print(f"profit: {best_profit/10**self.input_token.decimals}")

        if best_profit > 0:
            self.best.update(
                {
                    "swap_amount": swap_amount,
                    "profit_amount": best_profit,
                    "swap_pool_amounts": [],
                }
            )
            return True, (swap_amount, best_profit)
        else:
            return False, (swap_amount, best_profit)

    def calculate_multipool_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
    ) -> int:
        """
        Calculates the expected token OUTPUT from the last pool for a given token INPUT to the first pool
        at current pool states. Uses the self.token0 and self.token1 pointers to determine which token
        is being swapped in and uses the appropriate formula
        """

        number_of_pools = len(self.swap_pools)

        for i in range(number_of_pools):

            # determine the output token for this pool
            if token_in == self.swap_pools[i].token0:
                token_out = self.swap_pools[i].token1
            elif token_in == self.swap_pools[i].token1:
                token_out = self.swap_pools[i].token0
            else:
                raise ArbCalculationError(
                    f"Could not identify token_in! Found {token_in}, pool holds {self.swap_pools[i].token0}, {self.swap_pools[i].token1} "
                )

            # calculate the swap output through pool[i]
            token_out_quantity = self.swap_pools[
                i
            ].calculate_tokens_out_from_tokens_in(
                token_in=token_in, token_in_quantity=token_in_quantity
            )

            if i == number_of_pools - 1:
                # if we've reached the last pool, return the output amount
                return token_out_quantity
            else:
                # otherwise, use the output as input on the next loop
                token_in = token_out
                token_in_quantity = token_out_quantity

    def _build_multipool_amounts_out(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
    ) -> List[dict]:

        number_of_pools = len(self.swap_pools)

        pools_amounts_out = []

        for i in range(number_of_pools):

            # determine the uniswap version for the pool and format the output appropriately
            if self.swap_pools[i].uniswap_version == 2:
                # determine the output token for the pool
                if token_in == self.swap_pools[i].token0:
                    token_out = self.swap_pools[i].token1
                elif token_in == self.swap_pools[i].token1:
                    token_out = self.swap_pools[i].token0
                else:
                    raise InvalidSwapPathError(
                        f"Could not identify token_in! Found {token_in}, pool holds {self.swap_pools[i].token0}, {self.swap_pools[i].token1} "
                    )

                # calculate the swap output through pool[i]
                token_out_quantity = self.swap_pools[
                    i
                ].calculate_tokens_out_from_tokens_in(
                    token_in=token_in,
                    token_in_quantity=token_in_quantity,
                )

                if token_in == self.swap_pools[i].token0:
                    pools_amounts_out.append(
                        {
                            "uniswap_version": 2,
                            "amounts": [0, token_out_quantity],
                        }
                    )
                elif token_in == self.swap_pools[i].token1:
                    pools_amounts_out.append(
                        {
                            "uniswap_version": 2,
                            "amounts": [token_out_quantity, 0],
                        }
                    )
            elif self.swap_pools[i].uniswap_version == 3:
                # determine the output token for the pool
                if token_in == self.swap_pools[i].token0:
                    token_out = self.swap_pools[i].token1
                elif token_in == self.swap_pools[i].token1:
                    token_out = self.swap_pools[i].token0
                else:
                    raise InvalidSwapPathError(
                        f"Could not identify token_in! Found {token_in}, pool holds {self.swap_pools[i].token0}, {self.swap_pools[i].token1} "
                    )

                # calculate the swap output through pool[i]
                token_out_quantity = self.swap_pools[
                    i
                ].calculate_tokens_out_from_tokens_in(
                    token_in=token_in,
                    token_in_quantity=token_in_quantity,
                )

                if token_in == self.swap_pools[i].token0:
                    _zeroForOne = True
                    pools_amounts_out.append(
                        {
                            "uniswap_version": 3,
                            "amountSpecified": token_in_quantity,  # for an exactInput swap, always a positive number representing the input amount
                            "zeroForOne": _zeroForOne,
                            "sqrtPriceLimitX96": TickMath.MIN_SQRT_RATIO + 1
                            if _zeroForOne
                            else TickMath.MAX_SQRT_RATIO - 1,
                        }
                    )
                elif token_in == self.swap_pools[i].token1:
                    _zeroForOne = False
                    pools_amounts_out.append(
                        {
                            "uniswap_version": 3,
                            "amountSpecified": token_in_quantity,  # for an exactInput swap, always a positive number representing the input amount
                            "zeroForOne": _zeroForOne,
                            "sqrtPriceLimitX96": TickMath.MIN_SQRT_RATIO + 1
                            if _zeroForOne
                            else TickMath.MAX_SQRT_RATIO - 1,
                        }
                    )

            else:
                raise DegenbotError(
                    f"Could not identify Uniswap version for pool: {self.swap_pools[i]}"
                )

            if i == number_of_pools - 1:
                # if we've reached the last pool, return the pool_amounts_out list
                return pools_amounts_out
            else:
                # otherwise, feed the results back into the loop
                token_in = token_out
                token_in_quantity = token_out_quantity
