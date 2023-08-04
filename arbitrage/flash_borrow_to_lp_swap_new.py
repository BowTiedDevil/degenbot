from typing import List, Optional

from scipy.optimize import minimize_scalar  # type: ignore

from degenbot.token import Erc20Token
from degenbot.types import ArbitrageHelper
from degenbot.uniswap.v2.liquidity_pool import LiquidityPool

# TODO: improve arbitrage calculation for repaying with same token, instead of borrow A -> repay B


class FlashBorrowToLpSwapNew(ArbitrageHelper):
    def __init__(
        self,
        borrow_pool: LiquidityPool,
        borrow_token: Erc20Token,
        repay_token: Erc20Token,
        swap_pool_addresses: Optional[List[str]] = None,
        swap_pools: Optional[List[LiquidityPool]] = None,
        name: str = "",
        update_method="polling",
    ):
        if swap_pools is None and swap_pool_addresses is None:
            raise TypeError(
                "Provide a list of LiquidityPool objects or a list of pool addresses."
            )

        if not (swap_pools is not None) ^ (swap_pool_addresses is not None):
            # NOTE: ^ is the exclusive-or operator, which allows us to check that only one condition is True, but not both and not neither
            raise TypeError(
                "Provide a list of LiquidityPool objects or a list of pool addresses, but not both."
            )

        if update_method not in [
            "polling",
            "external",
        ]:
            raise ValueError("update_method must be 'polling' or 'external'")

        if update_method == "external" and swap_pool_addresses:
            raise ValueError(
                "swap pools passed by address must be updated with the 'polling' method"
            )

        if swap_pool_addresses is None:
            swap_pool_addresses = []

        if swap_pools is None:
            swap_pools = []

        self.borrow_pool = borrow_pool
        self.borrow_token = borrow_token
        self.repay_token = repay_token
        self._update_method = update_method

        # if the object was initialized with pool objects directly, use these directly
        if swap_pools:
            self.swap_pools = swap_pools

        # otherwise, create internal objects
        else:
            self.swap_pools = []
            for address in swap_pool_addresses:
                self.swap_pools.append(
                    LiquidityPool(
                        address=address,
                        update_method=update_method,
                    )
                )

        self.swap_pool_addresses = [pool.address for pool in self.swap_pools]
        self.swap_pool_tokens = [
            [pool.token0, pool.token1] for pool in self.swap_pools
        ]

        self.all_pool_addresses = [
            pool.address for pool in self.swap_pools + [self.borrow_pool]
        ]

        if name:
            self.name = name
        else:
            self.name = (
                self.borrow_pool.name
                + " -> "
                + " -> ".join([pool.name for pool in self.swap_pools])
            )

        if not self.borrow_token.address in [
            self.swap_pools[0].token0.address,
            self.swap_pools[0].token1.address,
        ]:
            raise ValueError("Borrowed token not found in the first swap pool")

        if not self.repay_token.address in [
            self.swap_pools[-1].token0.address,
            self.swap_pools[-1].token1.address,
        ]:
            raise ValueError("Repay token not found in the last swap pool")

        if self.swap_pools[0].token0.address == borrow_token.address:
            forward_token_address = self.swap_pools[0].token1.address
        else:
            forward_token_address = self.swap_pools[0].token0.address

        token_in_address = forward_token_address
        for pool in self.swap_pools:
            if pool.token0.address == token_in_address:
                forward_token_address = pool.token1.address
            elif pool.token1.address == token_in_address:
                forward_token_address = pool.token0.address
            else:
                raise Exception(
                    "Swap pools are invalid, no swap route possible!"
                )

        self.best = {
            "init": True,
            "strategy": "flash borrow swap",
            "borrow_amount": 0,
            "borrow_token": self.borrow_token,
            "borrow_pool": self.borrow_pool,
            "borrow_pool_amounts": [],
            "repay_amount": 0,
            "profit_amount": 0,
            "profit_token": self.repay_token,
            "swap_pools": self.swap_pools,
            "swap_pool_addresses": self.swap_pool_addresses,
            "swap_pool_amounts": [],
            "swap_pool_tokens": self.swap_pool_tokens,
        }

        self.best_future = {
            "strategy": "flash borrow swap",
            "borrow_amount": 0,
            "borrow_token": self.borrow_token,
            "borrow_pool": self.borrow_pool,
            "borrow_pool_amounts": [],
            "repay_amount": 0,
            "profit_amount": 0,
            "profit_token": self.repay_token,
            "swap_pools": self.swap_pools,
            "swap_pool_addresses": self.swap_pool_addresses,
            "swap_pool_amounts": [],
            "swap_pool_tokens": self.swap_pool_tokens,
        }

        self.update_reserves()

    def _build_multipool_amounts_out(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
    ) -> List[List[int]]:
        number_of_pools = len(self.swap_pools)

        pools_amounts_out = []

        for i in range(number_of_pools):
            # determine the output token for pool0
            if token_in.address == self.swap_pools[i].token0.address:
                token_out = self.swap_pools[i].token1
            elif token_in.address == self.swap_pools[i].token1.address:
                token_out = self.swap_pools[i].token0
            else:
                raise ValueError(f"Could not identify token_in {token_in}")

            # calculate the swap output through pool[i]
            token_out_quantity = self.swap_pools[
                i
            ].calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
            )

            if token_in.address == self.swap_pools[i].token0.address:
                pools_amounts_out.append([0, token_out_quantity])
            elif token_in.address == self.swap_pools[i].token1.address:
                pools_amounts_out.append([token_out_quantity, 0])

            if i == number_of_pools - 1:
                break
            else:
                # otherwise, feed the results back into the loop
                token_in = token_out
                token_in_quantity = token_out_quantity

        return pools_amounts_out

    def _calculate_arbitrage(
        self,
        override_future: bool = False,
        override_future_borrow_pool_reserves_token0: Optional[int] = None,
        override_future_borrow_pool_reserves_token1: Optional[int] = None,
    ):
        if override_future:
            if (
                override_future_borrow_pool_reserves_token0 is None
                or override_future_borrow_pool_reserves_token1 is None
            ):
                raise ValueError(
                    "Must override reserves for token0 and token1"
                )
            reserves_token0 = override_future_borrow_pool_reserves_token0
            reserves_token1 = override_future_borrow_pool_reserves_token1
        else:
            if (
                override_future_borrow_pool_reserves_token0 is not None
                or override_future_borrow_pool_reserves_token1 is not None
            ):
                raise ValueError(
                    "Do not provide override reserves without setting override_future = True"
                )
            reserves_token0 = self.borrow_pool.reserves_token0
            reserves_token1 = self.borrow_pool.reserves_token1

        # set up the boundaries for the Brent optimizer based on which token is being borrowed
        if self.borrow_token == self.borrow_pool.token0:
            bounds = (
                1,
                float(reserves_token0),
            )
            bracket = (
                0.001 * reserves_token0,
                0.01 * reserves_token0,
            )
        elif self.borrow_token == self.borrow_pool.token1:
            bounds = (
                1,
                float(reserves_token1),
            )
            bracket = (
                0.001 * reserves_token1,
                0.01 * reserves_token1,
            )
        else:
            raise ValueError(
                f"Could not identify borrow token {self.borrow_token}"
            )

        # TODO: extend calculate_multipool_tokens_out_from_tokens_in() to support overriding token reserves for an arbitrary pool,
        # currently only supports overriding the borrow pool reserves
        opt = minimize_scalar(
            lambda x: -float(
                self.calculate_multipool_tokens_out_from_tokens_in(
                    token_in=self.borrow_token,
                    token_in_quantity=x,
                )
                - self.borrow_pool.calculate_tokens_in_from_tokens_out(
                    token_in=self.repay_token,
                    token_out_quantity=x,
                    override_reserves_token0=override_future_borrow_pool_reserves_token0,
                    override_reserves_token1=override_future_borrow_pool_reserves_token1,
                )
            ),
            method="bounded",
            bounds=bounds,
            bracket=bracket,
            options={"xatol": 1.0},
        )

        best_borrow = int(opt.x)

        if self.borrow_token == self.borrow_pool.token0:
            borrow_amounts = [best_borrow, 0]
        elif self.borrow_token == self.borrow_pool.token1:
            borrow_amounts = [0, best_borrow]
        else:
            raise ValueError(
                f"Could not identify borrow token {self.borrow_token}"
            )

        best_repay = self.borrow_pool.calculate_tokens_in_from_tokens_out(
            token_in=self.repay_token,
            token_out_quantity=best_borrow,
            override_reserves_token0=override_future_borrow_pool_reserves_token0,
            override_reserves_token1=override_future_borrow_pool_reserves_token1,
        )
        best_profit = -int(opt.fun)

        if override_future:
            if best_borrow > 0 and best_profit > 0:
                self.best_future.update(
                    {
                        "borrow_amount": best_borrow,
                        "borrow_pool_amounts": borrow_amounts,
                        "repay_amount": best_repay,
                        "profit_amount": best_profit,
                        "swap_pool_amounts": self._build_multipool_amounts_out(
                            token_in=self.borrow_token,
                            token_in_quantity=best_borrow,
                        ),
                    }
                )
            else:
                self.best_future.update(
                    {
                        "borrow_amount": 0,
                        "borrow_pool_amounts": [],
                        "repay_amount": 0,
                        "profit_amount": 0,
                        "swap_pool_amounts": [],
                    }
                )
        else:
            # only save opportunities with rational, positive values
            if best_borrow > 0 and best_profit > 0:
                self.best.update(
                    {
                        "borrow_amount": best_borrow,
                        "borrow_pool_amounts": borrow_amounts,
                        "repay_amount": best_repay,
                        "profit_amount": best_profit,
                        "swap_pool_amounts": self._build_multipool_amounts_out(
                            token_in=self.borrow_token,
                            token_in_quantity=best_borrow,
                        ),
                    }
                )
            else:
                self.best.update(
                    {
                        "borrow_amount": 0,
                        "borrow_pool_amounts": [],
                        "repay_amount": 0,
                        "profit_amount": 0,
                        "swap_pool_amounts": [],
                    }
                )

    def __str__(self) -> str:
        return self.name

    def calculate_multipool_tokens_out_from_tokens_in(
        self, token_in: Erc20Token, token_in_quantity: int
    ) -> int:
        """
        Calculates the expected token OUTPUT from the last pool for a given token INPUT to the first pool at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        and uses the appropriate formula
        """

        number_of_pools = len(self.swap_pools)

        for i in range(number_of_pools):
            # determine the output token for pool0
            if token_in.address == self.swap_pools[i].token0.address:
                token_out = self.swap_pools[i].token1
            elif token_in.address == self.swap_pools[i].token1.address:
                token_out = self.swap_pools[i].token0
            else:
                raise ValueError(f"Could not identify token_in {token_in}")

            # calculate the swap output through pool[i]
            token_out_quantity = self.swap_pools[
                i
            ].calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
            )

            if i == number_of_pools - 1:
                break
            else:
                # otherwise, use the output as input on the next loop
                token_in = token_out
                token_in_quantity = token_out_quantity

        return token_out_quantity

    def update_reserves(
        self,
        silent: bool = False,
        print_reserves: bool = True,
        print_ratios: bool = True,
        override_future: bool = False,
        override_future_borrow_pool_reserves_token0: Optional[int] = None,
        override_future_borrow_pool_reserves_token1: Optional[int] = None,
    ) -> bool:
        """
        Checks each liquidity pool for updates by passing a call to .update_reserves(), which returns False if there are no updates.
        Will calculate arbitrage amounts only after checking all pools and finding a reason to update, or on startup (via the 'init' dictionary key)
        """
        recalculate = False

        if self._update_method != "external":
            # calculate initial arbitrage after the object is instantiated, otherwise proceed with normal checks
            if self.best["init"] == True:
                self.best["init"] = False
                recalculate = True

            # flag for recalculation if the borrowing pool has been updated
            if self.borrow_pool.update_reserves(
                silent=silent,
                print_reserves=print_reserves,
                print_ratios=print_ratios,
            ):
                recalculate = True

            # flag for recalculation if any of the pools along the swap path have been updated
            for pool in self.swap_pools:
                if pool.update_reserves(
                    silent=silent,
                    print_reserves=print_reserves,
                    print_ratios=print_ratios,
                ):
                    recalculate = True

        if override_future:
            recalculate = True
            if (
                override_future_borrow_pool_reserves_token0 is None
                or override_future_borrow_pool_reserves_token1 is None
            ):
                raise ValueError(
                    "Must override reserves for token0 and token1"
                )
        else:
            if (
                override_future_borrow_pool_reserves_token0 is not None
                or override_future_borrow_pool_reserves_token1 is not None
            ):
                raise ValueError(
                    "Do not provide override reserves without setting override_future = True"
                )

        if self.borrow_pool.new_reserves:
            recalculate = True
        for pool in self.swap_pools:
            if pool.new_reserves:
                recalculate = True
                break

        if recalculate:
            self._calculate_arbitrage(
                override_future=override_future,
                override_future_borrow_pool_reserves_token0=override_future_borrow_pool_reserves_token0,
                override_future_borrow_pool_reserves_token1=override_future_borrow_pool_reserves_token1,
            )
            return True
        else:
            return False
