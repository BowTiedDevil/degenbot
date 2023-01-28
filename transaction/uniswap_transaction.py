from .base import Transaction

from typing import Union, List
from degenbot.uniswap.v2 import LiquidityPool
from degenbot.uniswap.v3 import V3LiquidityPool


class UniswapTransaction(Transaction):
    def __init__(
        self,
        hash: str,
        transaction_func: str,
        transaction_params: dict,
        pools: List[Union[LiquidityPool, V3LiquidityPool]],
    ):
        self.pools = pools
        self.hash = hash
        self.transaction_func = transaction_func
        self.transaction_params = transaction_params

        print(f"Identified {transaction_func} transaction with params:")
        # process the key-value pairs in the transaction dictionary
        for key, value in transaction_params.items():
            print(f" • {key} = {value}")

        print("Identified pools:")
        for pool in pools:
            print(f" • {pool}")

    def simulate(self) -> dict:

        future_state = []

        if self.transaction_func == "swapExactTokensForTokens":

            swap_input_amount = self.transaction_params["amountIn"]
            swap_input_token = self.transaction_params["path"][0]

            # iterate through the pools and simulate the output of the transaction using the pool's
            # `calculate_tokens_out_from_tokens_in` method

            for i, pool in enumerate(self.pools):
                if i == 0:
                    pool_amount_in = swap_input_amount
                    pool_token_in = (
                        pool.token0
                        if swap_input_token == pool.token0.address
                        else pool.token1
                    )
                else:
                    pool_amount_in = pool_amount_out
                    # output of the last swap becomes input to this swap
                    pool_token_in = pool_token_out

                pool_amount_out = pool.calculate_tokens_out_from_tokens_in(
                    token_in=pool_token_in,
                    token_in_quantity=pool_amount_in,
                )
                pool_token_out = (
                    pool.token1
                    if pool_token_in == pool.token0
                    else pool.token0
                )

                # predict the change in reserves for this pool
                # add to token0 reserves if the input is in token0 position
                if pool_token_in == pool.token0:
                    token0_delta = pool_amount_in
                    token1_delta = -pool_amount_out
                elif pool_token_in == pool.token1:
                    token0_delta = -pool_amount_out
                    token1_delta = pool_amount_in
                pool_state = {
                    "pool": pool.name,
                    "reserves0": pool.reserves_token0 + token0_delta,
                    "reserves1": pool.reserves_token1 + token1_delta,
                }
                future_state.append(pool_state)
        else:
            print("unsupported swap (for now)")

        print(future_state)
        return future_state
