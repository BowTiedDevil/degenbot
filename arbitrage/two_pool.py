import random
from ..liquiditypool import LiquidityPool


class TwoPool:
    def __init__(
        self,
        pool_a: LiquidityPool,
        pool_b: LiquidityPool,
        name: str = "",
        calc_iterations: int = 256,
    ):
        assert pool_a is not pool_b, "Pools must be different!"
        assert (pool_a.token0 is pool_b.token0) and (
            pool_a.token1 is pool_b.token1
        ), "Tokens in both pools must match exactly"

        if name:
            self.name = name
        else:
            self.name = f"{pool_a} - {pool_b}"

        self.pool_a = pool_a
        self.pool_b = pool_b

        self.token0 = pool_a.token0
        self.token1 = pool_a.token1

        self.x0a = None
        self.y0a = None
        self.x0b = None
        self.y0b = None

        self.max_flash_borrow_pool_a_token0 = None
        self.max_flash_borrow_pool_a_token1 = None
        self.max_flash_borrow_pool_b_token0 = None
        self.max_flash_borrow_pool_b_token1 = None

        self.max_flash_borrow_pool_a_token0_profit = None
        self.max_flash_borrow_pool_a_token1_profit = None
        self.max_flash_borrow_pool_b_token0_profit = None
        self.max_flash_borrow_pool_b_token1_profit = None

        self._calc_iterations = calc_iterations

    def __str__(self):
        return self.name

    def _calculate_arbitrage(
        self,
        silent=True,
    ):
        for pool in [self.pool_a, self.pool_b]:
            if pool is self.pool_a:
                flash_pool = self.pool_a
                swap_pool = self.pool_b
            elif pool is self.pool_b:
                flash_pool = self.pool_b
                swap_pool = self.pool_a
            else:
                print("wtf?")
                raise

            for token in [self.token0, self.token1]:
                # set up pointers to flash_token and swap_token (in and out)
                if token is self.token0:
                    flash_borrow_token = self.token0
                    flash_repay_token = self.token1
                    swap_token_in = self.token0
                    swap_token_out = self.token1  # not used, delete?
                elif token is self.token1:
                    flash_borrow_token = self.token1
                    flash_repay_token = self.token0
                    swap_token_in = self.token1
                    swap_token_out = self.token0  # not used, delete?
                else:
                    print("wtf?")
                    raise

                # set up parameters for modified hill climb algorithm with initial borrow and seek step size
                best_borrow = 1

                if token is self.token0:
                    initial_step = int(0.01 * pool.reserves_token0)
                elif token is self.token1:
                    initial_step = int(0.01 * pool.reserves_token1)
                else:
                    print("wtf?")
                    raise

                best_profit = swap_pool.calculate_tokens_out_from_tokens_in(
                    token_in=swap_token_in,
                    token_in_quantity=best_borrow,
                ) - flash_pool.calculate_tokens_in_from_tokens_out(
                    token_in=flash_repay_token,
                    token_out_quantity=best_borrow,
                )

                for iteration in range(self._calc_iterations):
                    if iteration >= self._calc_iterations:
                        break

                    # shrink the delta on each loop, narrows search as we get closer
                    delta = initial_step * (1 - iteration / self._calc_iterations)

                    # calculate new borrow amounts by adding and subtracing the delta to the current best_borrow value
                    borrow_1 = best_borrow + delta
                    borrow_2 = best_borrow - delta

                    # calculate profit at the new borrow amounts
                    profit_1 = swap_pool.calculate_tokens_out_from_tokens_in(
                        token_in=swap_token_in,
                        token_in_quantity=borrow_1,
                    ) - flash_pool.calculate_tokens_in_from_tokens_out(
                        token_in=flash_repay_token,
                        token_out_quantity=borrow_1,
                    )
                    profit_2 = swap_pool.calculate_tokens_out_from_tokens_in(
                        token_in=swap_token_in,
                        token_in_quantity=borrow_2,
                    ) - flash_pool.calculate_tokens_in_from_tokens_out(
                        token_in=flash_repay_token,
                        token_out_quantity=borrow_2,
                    )

                    # select the highest profit among the two new profit results, and the current best profit
                    if profit_1 > best_profit and profit_1 > profit_2:
                        best_profit = profit_1
                        best_borrow = borrow_1
                    elif profit_2 > best_profit and profit_2 > profit_1:
                        best_profit = profit_2
                        best_borrow = borrow_2

                # only save rational opportunities with positive values
                if best_borrow > 0 and best_profit > 0:
                    if (flash_borrow_token is self.token0) and (
                        flash_pool is self.pool_a
                    ):
                        self.max_flash_borrow_pool_a_token0 = best_borrow
                        self.max_flash_borrow_pool_a_token0_profit = best_profit
                    elif (flash_borrow_token is self.token1) and (
                        flash_pool is self.pool_a
                    ):
                        self.max_flash_borrow_pool_a_token1 = best_borrow
                        self.max_flash_borrow_pool_a_token1_profit = best_profit
                    elif (flash_pool is self.pool_b) and (
                        flash_borrow_token is self.token0
                    ):
                        self.max_flash_borrow_pool_b_token0 = best_borrow
                        self.max_flash_borrow_pool_b_token0_profit = best_profit
                    elif (flash_pool is self.pool_b) and (
                        flash_borrow_token is self.token1
                    ):
                        self.max_flash_borrow_pool_b_token1 = best_borrow
                        self.max_flash_borrow_pool_b_token1_profit = best_profit
                    else:
                        print("wtf?")
                        raise
                    if not silent:
                        print()
                        print(
                            f"Optimal borrow: {best_borrow/(10**18)} {flash_borrow_token.symbol} on {flash_pool}"
                        )
                        print(
                            f"Profit: {best_profit/(10**18)} {flash_repay_token.symbol}"
                        )

    def update(self) -> bool:
        """
        Checks the current reserves of both associated pools (A & B) and calculates all profitable flash borrow amounts.
        The pool states are checked at the start so the update is only performed if the last-known state is stale.
        If force=True, the state check is skipped. Used by the constructor to calculate borrow amounts during instantiation.
        """

        # check pool state, if no changes since last call return False
        if (
            self.x0a == self.pool_a.reserves_token0
            and self.y0a == self.pool_a.reserves_token1
            and self.x0b == self.pool_b.reserves_token0
            and self.y0b == self.pool_b.reserves_token1
        ):
            return False
        # store the new reserve states for pools A & B, loop through both pools, calculate optimal borrow amounts, then return True
        else:
            self.x0a = self.pool_a.reserves_token0
            self.y0a = self.pool_a.reserves_token1
            self.x0b = self.pool_b.reserves_token0
            self.y0b = self.pool_b.reserves_token1

            self.max_flash_borrow_pool_a_token0 = 0
            self.max_flash_borrow_pool_a_token1 = 0
            self.max_flash_borrow_pool_b_token0 = 0
            self.max_flash_borrow_pool_b_token1 = 0

            self.max_flash_borrow_pool_a_token0_profit = 0
            self.max_flash_borrow_pool_a_token1_profit = 0
            self.max_flash_borrow_pool_b_token0_profit = 0
            self.max_flash_borrow_pool_b_token1_profit = 0

            # recalculate arbitrage amounts
            self._calculate_arbitrage()

            return True
