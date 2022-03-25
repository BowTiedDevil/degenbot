from brownie import Contract
from scipy import optimize
from fractions import Fraction
from . import LiquidityPool
from ..token import Erc20Token


class MultiLiquidityPool:
    def __init__(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        pool_addresses: list[str],
        pool_tokens: list[list[Erc20Token]],
        update_method="polling",
    ):

        self.token_in = token_in
        self.token_out = token_out
        self.token_in_quantity = 0
        self.token_out_quantity = 0

        assert len(pool_addresses) == len(
            pool_tokens
        ), "Number of pool addresses and token pairs must match!"

        assert (
            len(pool_addresses) > 1
        ), "Only one LP submitted, use LiquidityPool() instead"

        number_of_pools = len(pool_addresses)

        # build the list of pool objects for the given addresses
        self.pools = []
        for i in range(number_of_pools):
            self.pools.append(
                LiquidityPool(
                    address=pool_addresses[i],
                    update_method=update_method,
                    tokens=pool_tokens[i],
                )
            )
        self.pool_addresses = pool_addresses

        assert (token_in == self.pools[0].token0) or (
            token_in == self.pools[0].token1
        ), f"First LP does not contain the submitted token_in ({token_in})"

        assert (token_out == self.pools[-1].token0) or (
            token_out == self.pools[-1].token1
        ), f"Last LP does not contain the submitted token_out ({token_out})"

        # check that pools have a valid token path
        for i in range(number_of_pools - 1):
            assert (self.pools[i].token0 == self.pools[i + 1].token0) or (
                self.pools[i].token1 == self.pools[i + 1].token1
            ), f"LPs {self.pools[i]} and {self.pools[i+1]} do not share a common token!"

    def update_reserves(
        self,
        silent: bool = False,
        print_reserves: bool = True,
        print_ratios: bool = True,
    ) -> bool:
        """
        Checks each liquidity pool for updates by passing a call to .update_reserves(), which returns False if there are no updates.
        Will calculate arbitrage amounts only after checking all pools and finding an update, or on startup (via the 'init' dictionary key)
        """
        recalculate = False

        # flag for recalculation if any of the pools along the swap path have been updated
        for pool in self.pools:
            if pool.update_reserves(
                silent=silent,
                print_reserves=print_reserves,
                print_ratios=print_ratios,
            ):
                self.calculate_multipool_tokens_out_from_tokens_in(
                    token_in=self.token_in,
                    token_in_quantity=self.token_in_quantity,
                )
                return True
            else:
                return False

    def calculate_multipool_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
    ) -> int:
        """
        Calculates the expected token OUTPUT from the last pool for a given token INPUT to the first pool at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        and uses the appropriate formula
        """

        number_of_pools = len(self.pools)

        for i in range(number_of_pools):
            # determine the output token for this pool
            if token_in.address == self.pools[i].token0.address:
                token_out = self.pools[i].token1
            elif token_in.address == self.pools[i].token1.address:
                token_out = self.pools[i].token0
            else:
                print("wtf?")
                raise Exception

            # calculate the swap output from this pool
            token_out_quantity = self.pools[i].calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
            )

            if i == number_of_pools - 1:
                # if we've reached the last pool, build the amounts_out list and then
                # store the output quantity
                self.token_out_quantity = token_out_quantity
                self._build_multipool_amounts_out(
                    token_in=self.token_in,
                    token_in_quantity=self.token_in_quantity,
                )
            else:
                # otherwise, use the output as input on the next loop
                token_in = token_out
                token_in_quantity = token_out_quantity

    def update_balance(
        self,
        token_in_quantity: int,
    ):
        self.token_in_quantity = token_in_quantity
        self.calculate_multipool_tokens_out_from_tokens_in(
            token_in=self.token_in,
            token_in_quantity=self.token_in_quantity,
        )

    def _build_multipool_amounts_out(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
    ) -> list[list]:

        number_of_pools = len(self.pools)

        self.pools_amounts_out = []

        for i in range(number_of_pools):

            # determine the output token for pool0
            if token_in.address == self.pools[i].token0.address:
                token_out = self.pools[i].token1
            elif token_in.address == self.pools[i].token1.address:
                token_out = self.pools[i].token0
            else:
                print("wtf?")
                raise Exception

            # calculate the swap output through pool[i]
            token_out_quantity = self.pools[i].calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
            )

            if token_in.address == self.pools[i].token0.address:
                self.pools_amounts_out.append([0, token_out_quantity])
            elif token_in.address == self.pools[i].token1.address:
                self.pools_amounts_out.append([token_out_quantity, 0])

            if i == number_of_pools:
                print("breaking...")
                # if this is the last pool, break
                break
            else:
                # otherwise, use the output as input on the next loop
                token_in = token_out
                token_in_quantity = token_out_quantity
