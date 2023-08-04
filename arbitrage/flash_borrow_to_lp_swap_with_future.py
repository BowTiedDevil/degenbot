from typing import List, Optional, Tuple

from brownie.convert.datatypes import Wei  # type: ignore
from scipy import optimize  # type: ignore

from degenbot.token import Erc20Token
from degenbot.types import ArbitrageHelper
from degenbot.uniswap.v2.liquidity_pool import LiquidityPool


class FlashBorrowToLpSwapWithFuture(ArbitrageHelper):
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
        if swap_pools is None:
            swap_pools = []

        if swap_pool_addresses is None:
            swap_pool_addresses = []

        if not (swap_pools or swap_pool_addresses):
            raise ValueError(
                "At least one pool address or LiquidityPool object must be provided"
            )

        if swap_pool_addresses and swap_pools:
            raise ValueError(
                "Choose pool addresses or LiquidityPool objects, not both"
            )

        if not update_method in [
            "polling",
            "external",
        ]:
            raise ValueError("update_method must be 'polling' or 'external'")

        if update_method == "external" and swap_pool_addresses:
            raise ValueError(
                "swap pools by address must be updated with the 'polling' method"
            )

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

        # bugfix: maintain a record of the reserve state for associated LPs, used to avoid arb recalculation
        self.reserves: dict = {}
        if self._update_method != "external":
            self.update_reserves()

        # track the gas estimate to execute this arb
        self.gas_estimate = 0

    def _build_multipool_amounts_out(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        pool_overrides: Optional[
            List[Tuple[LiquidityPool, Tuple[int, int]]]
        ] = None,
    ) -> List[List[int]]:
        number_of_pools = len(self.swap_pools)

        if pool_overrides is None:
            pool_overrides = []

        pools_amounts_out = []

        for i in range(number_of_pools):
            # determine the output token for pool0
            if token_in.address == self.swap_pools[i].token0.address:
                token_out = self.swap_pools[i].token1
            elif token_in.address == self.swap_pools[i].token1.address:
                token_out = self.swap_pools[i].token0
            else:
                print("wtf?")
                raise Exception

            # value of 0 will result in the default behavior (no reserve overrides)
            override_reserves_token0 = 0
            override_reserves_token1 = 0

            # override the reserves if found in pool_overrides
            for override in pool_overrides:
                if override[0] == self.swap_pools[i]:
                    (
                        override_reserves_token0,
                        override_reserves_token1,
                    ) = override[1]

            # calculate the swap output through pool[i]
            token_out_quantity = self.swap_pools[
                i
            ].calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
                override_reserves_token0=override_reserves_token0,
                override_reserves_token1=override_reserves_token1,
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
        pool_overrides: Optional[
            List[Tuple[LiquidityPool, Tuple[int, int]]]
        ] = None,
    ):
        if pool_overrides is None:
            pool_overrides = []

        borrow_pool_reserves_token0 = self.borrow_pool.reserves_token0
        borrow_pool_reserves_token1 = self.borrow_pool.reserves_token1

        # override the borrowing pool reserves if any of the pools in pool_overrides
        # reference this pool
        if override_future:
            for override in pool_overrides:
                if override[0] == self.borrow_pool:
                    borrow_pool_reserves_token0 = override[1][0]
                    borrow_pool_reserves_token1 = override[1][1]
                    break

        # set up the boundaries for the Brent optimizer based on which token
        # is being borrowed
        if self.borrow_token.address == self.borrow_pool.token0.address:
            bounds = (
                0,
                float(borrow_pool_reserves_token0),
            )
            bracket = (
                0.001 * borrow_pool_reserves_token0,
                0.01 * borrow_pool_reserves_token0,
            )
        elif self.borrow_token.address == self.borrow_pool.token1.address:
            bounds = (
                0,
                float(borrow_pool_reserves_token1),
            )
            bracket = (
                0.001 * borrow_pool_reserves_token1,
                0.01 * borrow_pool_reserves_token1,
            )
        else:
            print("_calculate_arbitrage: WTF? Could not identify borrow token")
            raise Exception

        try:
            opt = optimize.minimize_scalar(
                lambda x: -float(
                    self.calculate_multipool_tokens_out_from_tokens_in(
                        token_in=self.borrow_token,
                        token_in_quantity=x,
                        pool_overrides=pool_overrides,
                    )
                    - self.borrow_pool.calculate_tokens_in_from_tokens_out(
                        token_in=self.repay_token,
                        token_out_quantity=x,
                        override_reserves_token0=borrow_pool_reserves_token0,
                        override_reserves_token1=borrow_pool_reserves_token1,
                    )
                ),
                method="bounded",
                bounds=bounds,
                bracket=bracket,
                options={
                    "xatol": 1.0,
                    # "disp": 3,
                },
            )
        except Exception as e:
            print(e)
            print(f"bounds: {bounds}")
            print(f"bracket: {bracket}")
            raise
        else:
            best_borrow = int(opt.x)

        if self.borrow_token.address == self.borrow_pool.token0.address:
            borrow_amounts = [best_borrow, 0]
        elif self.borrow_token.address == self.borrow_pool.token1.address:
            borrow_amounts = [0, best_borrow]
        else:
            print("_calculate_arbitrage: WTF? Could not identify borrow token")
            raise Exception

        best_repay = self.borrow_pool.calculate_tokens_in_from_tokens_out(
            token_in=self.repay_token,
            token_out_quantity=best_borrow,
            override_reserves_token0=borrow_pool_reserves_token0,
            override_reserves_token1=borrow_pool_reserves_token1,
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
                            pool_overrides=pool_overrides,
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
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        pool_overrides: Optional[
            List[Tuple[LiquidityPool, Tuple[int, int]]]
        ] = None,
    ) -> int:
        """
        Calculates the expected token OUTPUT from the last pool for a given token INPUT to the first pool
        at current pool reserves. Uses the self.token0 and self.token1 pointers to determine which token
        is being swapped in and uses the appropriate formula

        2022-06-14 update: add support for overriding pool reserves
        """

        if pool_overrides is None:
            pool_overrides = []

        number_of_pools = len(self.swap_pools)

        for i in range(number_of_pools):
            # determine the output token for pool0
            if token_in.address == self.swap_pools[i].token0.address:
                token_out = self.swap_pools[i].token1
            elif token_in.address == self.swap_pools[i].token1.address:
                token_out = self.swap_pools[i].token0
            else:
                print("wtf?")
                raise Exception

            override_reserves_token0 = 0
            override_reserves_token1 = 0

            for override_pool, override_reserves in pool_overrides:
                # each override is a tuple of form (LiquidityPool, (reserve0,reserve1))
                if override_pool == self.swap_pools[i]:
                    override_reserves_token0 = override_reserves[0]
                    override_reserves_token1 = override_reserves[1]
                    break

            # calculate the swap output through pool[i]
            token_out_quantity = self.swap_pools[
                i
            ].calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
                override_reserves_token0=override_reserves_token0,
                override_reserves_token1=override_reserves_token1,
            )

            if i == number_of_pools - 1:
                break
            else:
                # otherwise, use the output as input on the next loop
                token_in = token_out
                token_in_quantity = token_out_quantity

        return token_out_quantity

    def clear_best(self):
        self.best.update(
            {
                "borrow_amount": 0,
                "borrow_pool_amounts": [],
                "repay_amount": 0,
                "profit_amount": 0,
                "swap_pool_amounts": [],
            }
        )

    def clear_best_future(self):
        self.best_future.update(
            {
                "borrow_amount": 0,
                "borrow_pool_amounts": [],
                "repay_amount": 0,
                "profit_amount": 0,
                "swap_pool_amounts": [],
            }
        )

    def update_reserves(
        self,
        silent: bool = False,
        print_reserves: bool = True,
        print_ratios: bool = True,
        override_future: bool = False,
        pool_overrides: Optional[
            List[Tuple[LiquidityPool, Tuple[int, int]]]
        ] = None,
    ) -> bool:
        """
        Updates reserve values for one or more liquidity pools by calling update_reserves(), which returns False if the reserves have not changed.
        Will calculate arbitrage amounts only after checking all pools and finding a reason to update, or on startup (via the 'init' dictionary key)
        """

        if pool_overrides is None:
            pool_overrides = []

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
            if not pool_overrides:
                raise ValueError("Overrides must be provided!")

            recalculate = True

            for override_pool, override_reserves in pool_overrides:
                if type(override_pool) is not LiquidityPool:
                    raise TypeError(
                        "Override does not include a LiquidityPool object!"
                    )

                if type(override_reserves) is not tuple:
                    raise TypeError("Overrides not formatted as a tuple")

                if len(override_reserves) != 2:
                    raise ValueError("Override length must be 2")

                if type(override_reserves[0]) not in (
                    int,
                    Wei,
                ):
                    raise TypeError(
                        f"override for token0 must be int/Wei, is {type(override_reserves[0])}"
                    )

                if type(override_reserves[1]) not in (
                    int,
                    Wei,
                ):
                    raise TypeError(
                        f"override for token1 must be int/Wei, is {type(override_reserves[1])}"
                    )

        # update the reserves tracked in self.reserves and flag the arb for recalculation if they did not match
        for pool in [self.borrow_pool] + self.swap_pools:
            current_lp_reserves = (pool.reserves_token0, pool.reserves_token1)
            if self.reserves.get(pool.address) != current_lp_reserves:
                self.reserves[pool.address] = current_lp_reserves
                recalculate = True

        if recalculate:
            self._calculate_arbitrage(
                override_future=override_future,
                pool_overrides=pool_overrides,
            )
            return True
        else:
            return False
