from typing import List

from ..erc20_token import Erc20Token
from .v2_liquidity_pool import LiquidityPool


class MultiLiquidityPool:
    def __init__(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        pool_addresses: List[str],
        pool_tokens: List[List[Erc20Token]],
        name: str = "",
        update_method: str = "polling",
        silent: bool = False,
    ):
        self.token_in = token_in
        self.token_out = token_out
        self.token_in_quantity = 0
        self.token_out_quantity = 0
        self.init = True

        if len(pool_addresses) != len(pool_tokens):
            raise ValueError(
                "Number of pool addresses and token pairs must match!"
            )

        if not (len(pool_addresses) > 1):
            raise ValueError(
                f"Expected 2 pool addresses, found {len(pool_addresses)}"
            )

        number_of_pools = len(pool_addresses)

        # build the list of pool objects for the given addresses
        self._pools = []
        for i in range(number_of_pools):
            self._pools.append(
                LiquidityPool(
                    address=pool_addresses[i],
                    update_method=update_method,
                    tokens=pool_tokens[i],
                    silent=silent,
                )
            )
        self.pool_addresses = pool_addresses

        if not (
            (token_in == self._pools[0].token0)
            or (token_in == self._pools[0].token1)
        ):
            raise ValueError(
                f"First LP does not contain the submitted token_in ({token_in})"
            )

        if not (
            (token_out == self._pools[-1].token0)
            or (token_out == self._pools[-1].token1)
        ):
            raise ValueError(
                f"Last LP does not contain the submitted token_out ({token_out})"
            )

        # check that pools have a valid token path
        for i in range(number_of_pools - 1):
            if not (
                (self._pools[i].token0 == self._pools[i + 1].token0)
                or (self._pools[i].token1 == self._pools[i + 1].token1)
            ):
                raise ValueError(
                    f"LPs {self._pools[i]} and {self._pools[i+1]} do not share a common token!"
                )

        if name:
            self.name = name

    def update_reserves(
        self,
        silent: bool = False,
        print_reserves: bool = True,
        print_ratios: bool = True,
    ) -> bool:
        """
        Checks each liquidity pool for updates by passing a call to .update_reserves(), which returns False if there are no updates.
        Will calculate arbitrage amounts only after checking all pools and finding an update, or on startup (via the self._init variable)
        """

        recalculate = False

        if self.init is True:
            self.init = False
            recalculate = True

        for pool in self._pools:
            if pool.update_reserves(
                silent=silent,
                print_reserves=print_reserves,
                print_ratios=print_ratios,
            ):
                recalculate = True

        if recalculate:
            self.calculate_multipool_tokens_out_from_tokens_in(
                token_in=self.token_in,
                token_in_quantity=self.token_in_quantity,
                silent=silent,
            )
            return True
        else:
            return False

    def calculate_multipool_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        silent: bool = False,
    ) -> None:
        """
        Calculates the expected token OUTPUT from the last pool for a given token INPUT to the first pool at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        and uses the appropriate formula
        """

        number_of_pools = len(self._pools)

        for i in range(number_of_pools):
            # determine the output token for this pool
            if token_in.address == self._pools[i].token0.address:
                token_out = self._pools[i].token1
            elif token_in.address == self._pools[i].token1.address:
                token_out = self._pools[i].token0
            else:
                print("wtf?")
                raise Exception

            # calculate the swap output from this pool
            token_out_quantity = self._pools[
                i
            ].calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
            )

            if i == number_of_pools - 1:
                # if we've reached the last pool, build amounts_out and store the output quantity
                self.token_out_quantity = token_out_quantity
            else:
                # otherwise, use the output as input on the next loop
                token_in = token_out
                token_in_quantity = token_out_quantity

        self._build_multipool_amounts_out(
            token_in=self.token_in,
            token_in_quantity=self.token_in_quantity,
            silent=silent,
        )

    def update_balance(
        self,
        token_in_quantity: int,
        silent: bool = False,
    ):
        self.token_in_quantity = token_in_quantity
        self.calculate_multipool_tokens_out_from_tokens_in(
            token_in=self.token_in,
            token_in_quantity=self.token_in_quantity,
            silent=silent,
        )

    def __str__(self):
        """
        Return the pool name when the object is included in a print statement, or cast as a string
        """
        return self.name

    def _build_multipool_amounts_out(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        silent: bool = True,
    ) -> None:
        number_of_pools = len(self._pools)

        self.pools_amounts_out = []

        for i in range(number_of_pools):
            # determine the output token for pool0
            if token_in.address == self._pools[i].token0.address:
                token_out = self._pools[i].token1
            elif token_in.address == self._pools[i].token1.address:
                token_out = self._pools[i].token0
            else:
                print("wtf?")
                raise Exception

            # calculate the swap output through pool[i]
            token_out_quantity = self._pools[
                i
            ].calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
            )

            if not silent:
                print(
                    f"Swap {token_in_quantity} {token_in} for {token_out_quantity} {token_out} via {self._pools[i]}"
                )

            if token_in.address == self._pools[i].token0.address:
                self.pools_amounts_out.append([0, token_out_quantity])
            elif token_in.address == self._pools[i].token1.address:
                self.pools_amounts_out.append([token_out_quantity, 0])

            # use the swap output as the swap input through the next pool
            token_in = token_out
            token_in_quantity = token_out_quantity
