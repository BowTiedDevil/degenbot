from typing import List, Union

import web3
import itertools

from degenbot.transaction.base import Transaction
from degenbot.uniswap.v2 import LiquidityPool
from degenbot.uniswap.v2.abi import UNISWAPV2_ROUTER
from degenbot.uniswap.v3 import V3LiquidityPool
from degenbot.uniswap.v3.abi import (
    UNISWAP_V3_ROUTER_ABI,
    UNISWAP_V3_ROUTER2_ABI,
)


class UniswapTransaction(Transaction):
    def __init__(
        self,
        tx_hash: str,
        func_name: str,
        func_params: dict,
        pools: List[Union[LiquidityPool, V3LiquidityPool]],
    ):

        self.pools = pools
        self.tx_hash = tx_hash
        self.transaction_func = func_name
        self.transaction_params = func_params
        self.deadline = func_params.get("deadline")
        self.previousBlockhash = (
            hash.hex()
            if (hash := self.transaction_params.get("previousBlockhash"))
            else None
        )

        # print(f"Identified {transaction_func} transaction with params:")
        # # process the key-value pairs in the transaction dictionary
        # for key, value in transaction_params.items():
        #     print(f" • {key} = {value}")

        # print("Identified pools:")
        # for pool in pools:
        #     print(f" • {pool}")

    def simulate(self, transaction_func=None, transaction_params=None) -> dict:

        if transaction_func is None:
            transaction_func = self.transaction_func

        if transaction_params is None:
            transaction_params = self.transaction_params

        future_state = []

        # Start of UniswapV2 functions
        if transaction_func in (
            "swapExactTokensForETH",
            "swapExactTokensForETHSupportingFeeOnTransferTokens",
        ):
            print(transaction_func)
            mempool_tx_token_in_quantity = transaction_params.get("amountIn")
            # print(
            #     f"In: {mempool_tx_token_in_quantity/(10**mempool_tx_token_in.decimals):.4f} {mempool_tx_token_in}"
            # )
            # print(
            #     f"Out: {func_args.get('amountOutMin')/(10**mempool_tx_token_out.decimals):.4f} {mempool_tx_token_out}"
            # )
            # print(f"DEX: {ROUTERS[pending_tx.get('to')]['name']}")

        elif transaction_func in (
            "swapExactETHForTokens",
            "swapExactETHForTokensSupportingFeeOnTransferTokens",
        ):
            print(transaction_func)
            mempool_tx_token_in_quantity = transaction_params.get("value")
            # print(
            #     f"In: {mempool_tx_token_in_quantity/(10**mempool_tx_token_in.decimals):.4f} {mempool_tx_token_in}"
            # )
            # print(
            #     f"Out: {func_args.get('amountOutMin')/(10**mempool_tx_token_out.decimals):.4f} {mempool_tx_token_out}"
            # )
            # print(f"DEX: {ROUTERS[pending_tx.get('to')]['name']}")

        elif transaction_func in [
            "swapExactTokensForTokens",
            "swapExactTokensForTokensSupportingFeeOnTransferTokens",
        ]:
            print(transaction_func)
            mempool_tx_token_in_quantity = transaction_params.get("amountIn")
            # print(
            #     f"In: {mempool_tx_token_in_quantity/(10**mempool_tx_token_in.decimals):.4f} {mempool_tx_token_in}"
            # )
            # print(
            #     f"Out: {func_args.get('amountOutMin')/(10**mempool_tx_token_out.decimals):.4f} {mempool_tx_token_out}"
            # )
            # print(f"DEX: {ROUTERS[pending_tx.get('to')]['name']}")

        elif transaction_func in ("swapTokensForExactETH"):
            print(transaction_func)

            # # an index used for finding token addresses in the TX path
            # token_out_position = -1

            # # work backward from the end (using a negative step list copy), calculating token inputs required to receive amountOut from final pool
            # for pool in mempool_tx_lp_objects[::-1]:
            #     token_out = degenbot_tokens.get(
            #         func_args.get("path")[token_out_position]
            #     )
            #     token_in = degenbot_tokens.get(
            #         func_args.get("path")[token_out_position - 1]
            #     )

            #     # use the transaction amountOut parameter for the first calculation
            #     if token_out_position == -1:
            #         token_out_quantity = func_args.get("amountOut")

            #     # check if the requested amount out exceeds the available pool reserves. If so, set valid_swap to False and break
            #     _lp = mempool_tx_lp_objects[token_out_position]

            #     if token_out == _lp.token0:
            #         if token_out_quantity > _lp.reserves_token0:
            #             valid_swap = False
            #             break
            #     elif token_out == _lp.token1:
            #         if token_out_quantity > _lp.reserves_token1:
            #             valid_swap = False
            #             break

            #     # print(f"Calculating input for pool {pool}")

            #     token_in_quantity = mempool_tx_lp_objects[
            #         token_out_position
            #     ].calculate_tokens_in_from_tokens_out(
            #         token_in=token_in,
            #         token_out_quantity=token_out_quantity,
            #     )

            #     # feed the result into the next loop, unless we're at the beginning of the path
            #     if token_out_position == -len(mempool_tx_lp_objects):
            #         mempool_tx_token_in_quantity = token_in_quantity
            #         if mempool_tx_token_in_quantity > func_args.get(
            #             "amountInMax"
            #         ):
            #             valid_swap = False
            #         break
            #     else:
            #         # move the index back
            #         token_out_position -= 1
            #         # set the output for the next pool equal to the input of this pool
            #         token_out_quantity = token_in_quantity

            # if not valid_swap:
            #     continue

            # print(
            #     f"In: {func_args.get('amountInMax')/(10**mempool_tx_token_in.decimals):.4f} {mempool_tx_token_in}"
            # )
            # print(
            #     f"Min. In: {mempool_tx_token_in_quantity/(10**mempool_tx_token_in.decimals):.4f} {mempool_tx_token_in}"
            # )
            # print(
            #     f"Out: {func_args.get('amountOut')/(10**mempool_tx_token_out.decimals):.4f} {mempool_tx_token_out}"
            # )
            # print(f"DEX: {ROUTERS[pending_tx.get('to')].get('name')}")

        elif transaction_func in ("swapETHForExactTokens"):
            print(transaction_func)
            # # an index used for finding token addresses in the TX path
            # token_out_position = -1

            # # work backward (using a negative step list copy), calculating token inputs required to receive amountOut from final pool
            # for pool in mempool_tx_lp_objects[::-1]:
            #     token_out = degenbot_tokens.get(
            #         func_args.get("path")[token_out_position]
            #     )
            #     token_in = degenbot_tokens.get(
            #         func_args.get("path")[token_out_position - 1]
            #     )

            #     # use the quantity from the mempool TX
            #     if token_out_position == -1:
            #         token_out_quantity = func_args.get("amountOut")

            #     # check if the requested amount out exceeds the available pool reserves. If so, set valid_swap to False and break
            #     _lp = mempool_tx_lp_objects[token_out_position]

            #     if token_out == _lp.token0:
            #         if token_out_quantity > _lp.reserves_token0:
            #             valid_swap = False
            #             break
            #     elif token_out == _lp.token1:
            #         if token_out_quantity > _lp.reserves_token1:
            #             valid_swap = False
            #             break

            #     token_in_quantity = mempool_tx_lp_objects[
            #         token_out_position
            #     ].calculate_tokens_in_from_tokens_out(
            #         token_in=token_in,
            #         token_out_quantity=token_out_quantity,
            #     )

            #     # Feed the result into the next loop, unless we've reached the beginning of the path.
            #     # If we're at the beginning, set the required min input and break the loop
            #     if token_out_position == -len(mempool_tx_lp_objects):
            #         mempool_tx_token_in_quantity = token_in_quantity
            #         if mempool_tx_token_in_quantity > pending_tx.get("value"):
            #             valid_swap = False
            #         break
            #     else:
            #         # move the index back
            #         token_out_position -= 1
            #         # set the output for the next pool equal to the input of this pool
            #         token_out_quantity = token_in_quantity

            # if not valid_swap:
            #     continue

            # print(
            #     f"In: {pending_tx.get('value')/(10**mempool_tx_token_in.decimals):.4f} {mempool_tx_token_in}"
            # )
            # print(
            #     f"Min. In: {mempool_tx_token_in_quantity/(10**mempool_tx_token_in.decimals):.4f} {mempool_tx_token_in}"
            # )
            # print(
            #     f"Out: {func_args.get('amountOut')/(10**mempool_tx_token_out.decimals):.4f} {mempool_tx_token_out}"
            # )
            # print(f"DEX: {ROUTERS[pending_tx.get('to')].get('name')}")

        elif transaction_func == "swapExactTokensForTokens":
            print(transaction_func)

            swap_input_amount = transaction_params["amountIn"]
            swap_input_token = transaction_params["path"][0]

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

        # Start of UniswapV3 functions
        elif transaction_func == "multicall":
            future_state = self.simulate_multicall()
        elif transaction_func == "exactInputSingle":
            print(transaction_func)

            # decode with Router ABI - https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
            # struct ExactInputSingleParams {
            #   address tokenIn;
            #   address tokenOut;
            #   uint24 fee;
            #   address recipient;
            #   uint256 deadline;
            #   uint256 amountIn;
            #   uint256 amountOutMinimum;
            #   uint160 sqrtPriceLimitX96;
            # }
            try:
                (
                    tokenIn,
                    tokenOut,
                    fee,
                    recipient,
                    deadline,
                    amountIn,
                    amountOutMinimum,
                    sqrtPriceLimitX96,
                ) = transaction_params.get("params")
            except:
                pass

            # decode with Router2 ABI - https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
            # struct ExactInputSingleParams {
            #   address tokenIn;
            #   address tokenOut;
            #   uint24 fee;
            #   address recipient;
            #   uint256 amountIn;
            #   uint256 amountOutMinimum;
            #   uint160 sqrtPriceLimitX96;
            # }
            try:
                tokenIn,
                tokenOut,
                fee,
                recipient,
                amountIn,
                amountOutMinimum,
                sqrtPriceLimitX96 = transaction_params.get("params")
            except:
                pass

            for pool in self.pools:
                # find the pool helper that holds tokenIn and tokenOut
                if set(pool.token0, pool.token1) == set(tokenIn, tokenOut):
                    print(f"pool located: {pool}")
                    lp_helper = pool
                    break
            print(f"Predicting output of swap through pool: {pool}")

        elif transaction_func == "exactInput":
            print(transaction_func)
            try:
                (
                    exactInputParams_path,
                    exactInputParams_recipient,
                    exactInputParams_deadline,
                    exactInputParams_amountIn,
                    exactInputParams_amountOutMinimum,
                ) = transaction_params.get("params")
            except:
                pass

            try:
                (
                    exactInputParams_path,
                    exactInputParams_recipient,
                    exactInputParams_amountIn,
                    exactInputParams_amountOutMinimum,
                ) = transaction_params.get("params")
            except:
                pass

            # decode the path
            path_pos = 0
            exactInputParams_path_decoded = []
            # read alternating 20 and 3 byte chunks from the encoded path,
            # store each address (hex) and fee (int)
            for byte_length in itertools.cycle((20, 3)):
                # stop at the end
                if path_pos == len(exactInputParams_path):
                    break
                elif (
                    byte_length == 20
                    and len(exactInputParams_path) >= path_pos + byte_length
                ):
                    address = exactInputParams_path[
                        path_pos : path_pos + byte_length
                    ].hex()
                    exactInputParams_path_decoded.append(address)
                elif (
                    byte_length == 3
                    and len(exactInputParams_path) >= path_pos + byte_length
                ):
                    fee = int(
                        exactInputParams_path[
                            path_pos : path_pos + byte_length
                        ].hex(),
                        16,
                    )
                    exactInputParams_path_decoded.append(fee)
                path_pos += byte_length

            # print(f"\tpath = {exactInputParams_path_decoded}")
            # print(f"\trecipient = {exactInputParams_recipient}")
            # if exactInputParams_deadline:
            #     print(f"\tdeadline = {exactInputParams_deadline}")
            # print(f"\tamountIn = {exactInputParams_amountIn}")
            # print(f"\tamountOutMinimum = {exactInputParams_amountOutMinimum}")
        elif transaction_func == "exactOutputSingle":
            print(transaction_func)
            # print(transaction_params.get("params"))
        elif transaction_func == "exactOutput":
            print(transaction_func)
            # print(transaction_params.get("params"))

            # Router ABI
            try:
                (
                    exactOutputParams_path,
                    exactOutputParams_recipient,
                    exactOutputParams_deadline,
                    exactOutputParams_amountOut,
                    exactOutputParams_amountInMaximum,
                ) = transaction_params.get("params")
            except Exception as e:
                pass

            # Router2 ABI
            try:
                (
                    exactOutputParams_path,
                    exactOutputParams_recipient,
                    exactOutputParams_amountOut,
                    exactOutputParams_amountInMaximum,
                ) = transaction_params.get("params")
            except Exception as e:
                pass

            # decode the path
            path_pos = 0
            exactOutputParams_path_decoded = []
            # read alternating 20 and 3 byte chunks from the encoded path,
            # store each address (hex) and fee (int)
            for byte_length in itertools.cycle((20, 3)):
                # stop at the end
                if path_pos == len(exactOutputParams_path):
                    break
                elif (
                    byte_length == 20
                    and len(exactOutputParams_path) >= path_pos + byte_length
                ):
                    address = exactOutputParams_path[
                        path_pos : path_pos + byte_length
                    ].hex()
                    exactOutputParams_path_decoded.append(address)
                elif (
                    byte_length == 3
                    and len(exactOutputParams_path) >= path_pos + byte_length
                ):
                    fee = int(
                        exactOutputParams_path[
                            path_pos : path_pos + byte_length
                        ].hex(),
                        16,
                    )
                    exactOutputParams_path_decoded.append(fee)
                path_pos += byte_length

            # print(f" • path = {exactOutputParams_path_decoded}")
            # print(f" • recipient = {exactOutputParams_recipient}")
            # if exactOutputParams_deadline:
            #     print(f" • deadline = {exactOutputParams_deadline}")
            # print(f" • amountOut = {exactOutputParams_amountOut}")
            # print(
            #     f" • amountamountInMaximum = {exactOutputParams_amountInMaximum}"
            # )
        elif transaction_func == "swapExactTokensForTokens":
            print(transaction_func)
            # print(transaction_params)
        elif transaction_func == "swapTokensForExactTokens":
            print(transaction_func)
            # print(transaction_params)
        elif transaction_func in (
            "addLiquidity",
            "addLiquidityETH",
            "removeLiquidity",
            "removeLiquidityETH",
            "removeLiquidityETHWithPermit",
            "removeLiquidityETHSupportingFeeOnTransferTokens",
            "removeLiquidityETHWithPermitSupportingFeeOnTransferTokens",
            "removeLiquidityWithPermit",
            "swapExactTokensForTokensSupportingFeeOnTransferTokens",
            "swapExactETHForTokensSupportingFeeOnTransferTokens",
            "swapExactTokensForETHSupportingFeeOnTransferTokens",
        ):
            # TODO: add prediction for these functions
            print(f"TODO: {transaction_func}")
        elif transaction_func in (
            "refundETH",
            "selfPermit",
            "selfPermitAllowed",
            "unwrapWETH9",
        ):
            # ignore, these functions do not affect future pool states
            pass
        else:
            print(f"\tUNHANDLED function: {transaction_func}")

        return future_state

    def simulate_multicall(self):

        future_state = []

        if multicall_data := self.transaction_params.get("data"):
            for i, payload in enumerate(multicall_data):
                try:
                    # decode with Router ABI
                    payload_func, payload_args = (
                        web3.Web3()
                        .eth.contract(abi=UNISWAP_V3_ROUTER_ABI)
                        .decode_function_input(payload)
                    )
                except Exception as e:
                    pass
                else:
                    # simulate each payload individually and append the future_state dict of that payload
                    future_state.extend(
                        self.simulate(
                            transaction_func=payload_func.fn_name,
                            transaction_params=payload_args,
                        )
                    )

                try:
                    # decode with Router2 ABI
                    payload_func, payload_args = (
                        web3.Web3()
                        .eth.contract(abi=UNISWAP_V3_ROUTER2_ABI)
                        .decode_function_input(payload)
                    )
                except Exception as e:
                    pass
                else:
                    # simulate each payload individually and append the future_state dict of that payload
                    future_state.extend(
                        self.simulate(
                            transaction_func=payload_func.fn_name,
                            transaction_params=payload_args,
                        )
                    )

        return future_state
