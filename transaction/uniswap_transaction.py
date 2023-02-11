import web3
import itertools

from typing import List, Optional
from degenbot.exceptions import (
    LiquidityPoolError,
    Erc20TokenError,
    EVMRevertError,
    ManagerError,
    TransactionError,
)
from degenbot.transaction.base import Transaction
from degenbot.uniswap.manager import (
    UniswapV2LiquidityPoolManager,
    UniswapV3LiquidityPoolManager,
)
from degenbot.uniswap.v2 import LiquidityPool
from degenbot.uniswap.v3.abi import (
    UNISWAP_V3_ROUTER_ABI,
    UNISWAP_V3_ROUTER2_ABI,
)
from degenbot.manager import Erc20TokenHelperManager


# maintain an internal dict of known mainnet routers
_routers = {
    "0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F": {
        "name": "Sushiswap: Router",
        "uniswap_version": 2,
        "factory_address": {2: "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"},
    },
    "0xf164fC0Ec4E93095b804a4795bBe1e041497b92a": {
        "name": "UniswapV2: Router",
        "uniswap_version": 2,
        "factory_address": {2: "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"},
    },
    "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D": {
        "name": "UniswapV2: Router 2",
        "uniswap_version": 2,
        "factory_address": {2: "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"},
    },
    "0xE592427A0AEce92De3Edee1F18E0157C05861564": {
        "name": "UniswapV3: Router",
        "uniswap_version": 3,
        "factory_address": {3: "0x1F98431c8aD98523631AE4a59f267346ea31F984"},
    },
    "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45": {
        "name": "UniswapV3: Router 2",
        "uniswap_version": 3,
        "factory_address": {
            2: "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            3: "0x1F98431c8aD98523631AE4a59f267346ea31F984",
        },
    },
}


class UniswapTransaction(Transaction):
    def __init__(
        self,
        tx_hash: str,
        tx_nonce: int,
        tx_value: int,
        func_name: str,
        func_params: dict,
        router_address: str,
    ):

        self.routers = _routers

        if router_address not in self.routers.keys():
            raise ValueError(f"Router address {router_address} unknown!")

        try:
            self.v2_pool_manager = UniswapV2LiquidityPoolManager(
                factory_address=self.routers[router_address][
                    "factory_address"
                ][2]
            )
        except:
            pass

        try:
            self.v3_pool_manager = UniswapV3LiquidityPoolManager(
                factory_address=self.routers[router_address][
                    "factory_address"
                ][3]
            )
        except:
            pass

        self.hash = tx_hash
        self.nonce = tx_nonce
        self.value = tx_value
        self.func_name = func_name
        self.func_params = func_params
        self.func_deadline = func_params.get("deadline")
        self.func_previous_block_hash = (
            hash.hex()
            if (hash := self.func_params.get("previousBlockhash"))
            else None
        )

    @classmethod
    def add_router(cls, router_address: str, router_dict: dict):

        print(f"adding router {router_address}")

        router_address = web3.Web3.toChecksumAddress(router_address)
        if router_address in _routers.keys():
            raise ValueError("Router address already known!")

        _routers[router_address] = router_dict

    def simulate(
        self,
        func_name: Optional[str] = None,
        func_params: Optional[dict] = None,
    ) -> dict:
        """
        Take a Uniswap V2 / V3 transaction (specified by name and a dictionary of parameters to that function and return the state dictionary associated with a
        """

        def v2_swap_exact_in(
            params: dict,
            unwrapped_input: Optional[bool] = False,
        ) -> list:
            pool_objects: List[LiquidityPool] = []
            for token_addresses in itertools.pairwise(params.get("path")):
                try:
                    pool_helper: LiquidityPool = self.v2_pool_manager.get_pool(
                        token_addresses=token_addresses
                    )

                except LiquidityPoolError:
                    raise TransactionError(
                        f"Liquidity pool could not be build for token pair {token_addresses[0]} - {token_addresses[1]}"
                    )
                else:
                    pool_objects.append(pool_helper)

            # the pool manager creates Erc20Token objects as it works,
            # so calls to `get_erc20token` will return the previously-created helper
            token_in = Erc20TokenHelperManager().get_erc20token(
                address=params["path"][0],
                silent=True,
                min_abi=True,
                unload_brownie_contract_after_init=True,
            )
            token_out = Erc20TokenHelperManager().get_erc20token(
                address=params["path"][-1],
                silent=True,
                min_abi=True,
                unload_brownie_contract_after_init=True,
            )

            if unwrapped_input:
                swap_in_quantity = self.value
            else:
                swap_in_quantity = params.get("amountIn")

            # predict future pool states assuming the swap executes in isolation
            future_pool_states = []
            for i, pool in enumerate(pool_objects):
                token_in_quantity = (
                    swap_in_quantity if i == 0 else token_out_quantity
                )

                # i == 0 for first pool in path, take from 'path' in func_params
                # otherwise, set token_in equal to token_out from previous iteration
                # and token_out equal to the other token held by the pool
                token_in = token_in if i == 0 else token_out
                token_out = (
                    pool.token0 if token_in is pool.token1 else pool.token1
                )

                current_state = pool.state
                future_state = pool.simulate_swap(
                    token_in=token_in,
                    token_in_quantity=token_in_quantity,
                )

                if (
                    future_state["reserves_token0"]
                    < current_state["reserves_token0"]
                ):
                    token_out_quantity = (
                        current_state["reserves_token0"]
                        - future_state["reserves_token0"]
                    )
                elif (
                    future_state["reserves_token1"]
                    < current_state["reserves_token1"]
                ):
                    token_out_quantity = (
                        current_state["reserves_token1"]
                        - future_state["reserves_token1"]
                    )
                else:
                    raise ValueError("Swap direction could not be identified")

                future_pool_states.append(
                    [
                        pool,
                        future_state,
                    ]
                )

                print(f"Simulating swap through pool: {pool}")
                print(
                    f"\t{token_in_quantity} {token_in} -> {token_out_quantity} {token_out}"
                )
                print("\t(CURRENT)")
                print(f"\t{pool.token0}: {current_state['reserves_token0']}")
                print(f"\t{pool.token1}: {current_state['reserves_token1']}")
                print(f"\t(FUTURE)")
                print(f"\t{pool.token0}: {future_state['reserves_token0']}")
                print(f"\t{pool.token1}: {future_state['reserves_token1']}")

            return future_pool_states

        if func_name is None:
            func_name = self.func_name

        if func_params is None:
            func_params = self.func_params

        future_state = []

        if False:
            pass

        # Start of UniswapV2 functions

        # if func_name in (
        #     "swapExactTokensForETH",
        #     "swapExactTokensForETHSupportingFeeOnTransferTokens",
        # ):
        #     print()
        #     print(func_name)
        #     future_state.extend(v2_swap_exact_in(func_params))

        elif func_name in (
            "swapExactETHForTokens",
            "swapExactETHForTokensSupportingFeeOnTransferTokens",
        ):
            print()
            print(func_name)
            future_state.extend(
                v2_swap_exact_in(func_params, unwrapped_input=True)
            )

        # short-circuit until all functions have been defined
        elif True:
            pass

        elif func_name in [
            "swapExactTokensForTokens",
            "swapExactTokensForTokensSupportingFeeOnTransferTokens",
        ]:

            print(func_name)

            token_in_quantity = func_params["amountIn"]
            token_out_min_quantity = func_params["amountOutMin"]

            # get the V2 pool helpers
            try:
                token_objects = [
                    Erc20TokenHelperManager().get_erc20token(
                        address=token_address,
                        silent=True,
                        min_abi=True,
                        unload_brownie_contract_after_init=True,
                    )
                    for token_address in func_params["path"]
                ]
            except Erc20TokenError:
                raise TransactionError(
                    "Could not load Erc20Token objects for complete swap path"
                )

            token_in = token_objects[0]
            token_out = token_objects[-1]

            print(
                f"In: {token_in_quantity/(10**token_in.decimals):.4f} {token_in}"
            )
            print(
                f"Min. out: {token_out_min_quantity/(10**token_out.decimals):.4f} {token_out}"
            )

            v2_pools = []
            for token_pair in itertools.pairwise(token_objects):
                try:
                    v2_pools.append(
                        self.v2_pool_manager.get_pool(
                            token_addresses=[
                                token.address for token in token_pair
                            ]
                        )
                    )
                except LiquidityPoolError:
                    raise TransactionError(
                        "Could not load LiquidityPool objects for complete swap path"
                    )

            print(f"{' -> '.join([pool.name for pool in v2_pools])}")

        elif func_name in ("swapTokensForExactETH"):
            pass

        elif func_name in ("swapETHForExactTokens"):

            pass

        elif func_name == "swapExactTokensForTokens":

            print()
            print(transaction_func)

            swap_input_amount = func_params["amountIn"]
            swap_input_token = func_params["path"][0]

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
        elif func_name == "multicall":
            # print(transaction_func)
            future_state = self.simulate_multicall()
        elif func_name == "exactInputSingle":
            print(func_name)

            # decode with Router ABI
            # https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
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
                ) = func_params.get("params")
            except:
                pass

            # decode with Router2 ABI
            # https://github.com/Uniswap/swap-router-contracts/blob/main/contracts/interfaces/IV3SwapRouter.sol
            try:
                (
                    tokenIn,
                    tokenOut,
                    fee,
                    recipient,
                    amountIn,
                    amountOutMinimum,
                    sqrtPriceLimitX96,
                ) = func_params.get("params")
            except:
                pass

            try:
                # get the V3 pool involved in the swap
                v3_pool = self.v3_pool_manager.get_pool(
                    token_addresses=(tokenIn, tokenOut),
                    pool_fee=fee,
                )
            except (ManagerError, LiquidityPoolError) as e:
                raise TransactionError(f"Could not get pool (via tokens): {e}")
            except:
                raise

            print(f"Predicting output of swap through pool: {v3_pool}")

            try:
                token_in_object = Erc20TokenHelperManager().get_erc20token(
                    address=tokenIn,
                    silent=True,
                    min_abi=True,
                    unload_brownie_contract_after_init=True,
                )
            except Exception as e:
                print(e)
                print(type(e))
                raise

            starting_state = v3_pool.state
            try:
                final_state = v3_pool.simulate_swap(
                    token_in=token_in_object,
                    token_in_quantity=amountIn,
                )
            except EVMRevertError as e:
                print(f"TRANSACTION CANNOT BE SIMULATED!: {e}")
                return

            print(f"{starting_state=}")
            print(f"{final_state=}")

        elif func_name == "exactInput":
            # print(transaction_func)
            # TODO: remove once this function is fully implemented
            return
            try:
                (
                    exactInputParams_path,
                    exactInputParams_recipient,
                    exactInputParams_deadline,
                    exactInputParams_amountIn,
                    exactInputParams_amountOutMinimum,
                ) = func_params.get("params")
            except:
                pass

            try:
                (
                    exactInputParams_path,
                    exactInputParams_recipient,
                    exactInputParams_amountIn,
                    exactInputParams_amountOutMinimum,
                ) = func_params.get("params")
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
        elif func_name == "exactOutputSingle":
            pass

        elif func_name == "exactOutput":
            # Router ABI
            try:
                (
                    exactOutputParams_path,
                    exactOutputParams_recipient,
                    exactOutputParams_deadline,
                    exactOutputParams_amountOut,
                    exactOutputParams_amountInMaximum,
                ) = func_params.get("params")
            except Exception as e:
                pass

            # Router2 ABI
            try:
                (
                    exactOutputParams_path,
                    exactOutputParams_recipient,
                    exactOutputParams_amountOut,
                    exactOutputParams_amountInMaximum,
                ) = func_params.get("params")
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
        elif func_name == "swapExactTokensForTokens":
            # print(transaction_func)
            # TODO: remove once this function is fully implemented
            return
        elif func_name == "swapTokensForExactTokens":
            # print(transaction_func)
            # TODO: remove once this function is fully implemented
            return
        elif func_name in (
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
            print(f"TODO: {func_name}")
        elif func_name in (
            "refundETH",
            "selfPermit",
            "selfPermitAllowed",
            "unwrapWETH9",
        ):
            # ignore, these functions do not affect future pool states
            pass
        else:
            print(f"\tUNHANDLED function: {func_name}")

        return future_state

    def simulate_multicall(self):

        future_state = []

        if multicall_data := self.func_params.get("data"):
            for payload in multicall_data:
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
                    if payload_state_delta := self.simulate(
                        func_name=payload_func.fn_name,
                        func_params=payload_args,
                    ):
                        future_state.extend(payload_state_delta)
                    continue

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
                    if payload_state_delta := self.simulate(
                        func_name=payload_func.fn_name,
                        func_params=payload_args,
                    ):
                        future_state.extend(payload_state_delta)
                    continue

        return future_state
