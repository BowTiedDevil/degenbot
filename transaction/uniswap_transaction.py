import eth_abi
import itertools
import web3

from typing import List, Optional, Tuple, Union
from degenbot.exceptions import (
    DegenbotError,
    LiquidityPoolError,
    EVMRevertError,
    ManagerError,
    TransactionError,
)
from degenbot.transaction.base import Transaction
from degenbot.uniswap.manager.uniswap_managers import (
    UniswapV2LiquidityPoolManager,
    UniswapV3LiquidityPoolManager,
)
from degenbot.uniswap.v2.liquidity_pool import LiquidityPool
from degenbot.uniswap.v3.abi import (
    UNISWAP_V3_ROUTER_ABI,
    UNISWAP_V3_ROUTER2_ABI,
)
from degenbot.uniswap.v3.v3_liquidity_pool import V3LiquidityPool
from degenbot.manager.token_manager import Erc20TokenHelperManager


# Internal dict of known router contracts, pre-populated with mainnet addresses
# Routers can be added via class method `add_router`
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
    "0xEf1c6E67703c7BD7107eed8303Fbe6EC2554BF6B": {
        "name": "Uniswap Universal Router",
        # TODO: determine if 'uniswap_version' is necessary,
        # or convert to tuple (2,3) so routers that support
        # two or more versions can be handled correctly
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

        router_address = web3.Web3.toChecksumAddress(router_address)
        if router_address in _routers.keys():
            raise ValueError("Router address already known!")

        _routers[router_address] = router_dict

    def simulate(
        self,
        func_name: Optional[str] = None,
        func_params: Optional[dict] = None,
        silent: bool = False,
    ) -> List[Tuple[Union[LiquidityPool, V3LiquidityPool], dict]]:
        """
        Take a Uniswap V2 / V3 transaction (specified by name and a dictionary of arguments
        to that function) and return a list of pools and state dictionaries for all hops
        associated with the transaction
        """

        def decode_v3_path(path: bytes) -> list:
            path_pos = 0
            exactInputParams_path_decoded = []
            # read alternating 20 and 3 byte chunks from the encoded path,
            # store each address (hex) and fee (int)
            for byte_length in itertools.cycle((20, 3)):
                # stop at the end
                if path_pos == len(path):
                    break
                elif byte_length == 20 and len(path) >= path_pos + byte_length:
                    address = path[path_pos : path_pos + byte_length].hex()
                    exactInputParams_path_decoded.append(address)
                elif byte_length == 3 and len(path) >= path_pos + byte_length:
                    fee = int(
                        path[path_pos : path_pos + byte_length].hex(),
                        16,
                    )
                    exactInputParams_path_decoded.append(fee)
                path_pos += byte_length

            return exactInputParams_path_decoded

        def v2_swap_exact_in(
            params: dict,
            unwrapped_input: Optional[bool] = False,
            silent: bool = False,
        ) -> List[Tuple[LiquidityPool, dict]]:

            v2_pool_objects = []
            for token_addresses in itertools.pairwise(params.get("path")):
                try:
                    pool_helper: LiquidityPool = self.v2_pool_manager.get_pool(
                        token_addresses=token_addresses,
                        silent=silent,
                    )
                except LiquidityPoolError:
                    raise TransactionError(
                        f"LiquidityPool could not be build for token pair {token_addresses[0]} - {token_addresses[1]}"
                    )
                else:
                    v2_pool_objects.append(pool_helper)

            # print("pools:")
            # for pool in v2_pool_objects:
            #     print(f"{pool}: {pool.address}")
            #     print(f"{pool.reserves_token0=}")
            #     print(f"{pool.reserves_token1=}")

            # the pool manager created Erc20Token objects in the code block above,
            # so calls to `get_erc20token` will return the previously-created helper
            token_in = Erc20TokenHelperManager().get_erc20token(
                address=params["path"][0],
                silent=silent,
                min_abi=True,
                unload_brownie_contract_after_init=True,
            )
            token_out = Erc20TokenHelperManager().get_erc20token(
                address=params["path"][-1],
                silent=silent,
                min_abi=True,
                unload_brownie_contract_after_init=True,
            )

            if unwrapped_input:
                swap_in_quantity = self.value
            else:
                swap_in_quantity = params.get("amountIn")

            # predict future pool states assuming the swap executes in isolation
            future_pool_states = []
            for i, v2_pool in enumerate(v2_pool_objects):
                token_in_quantity = (
                    swap_in_quantity if i == 0 else token_out_quantity
                )

                # i == 0 for first pool in path, take from 'path' in func_params
                # otherwise, set token_in equal to token_out from previous iteration
                # and token_out equal to the other token held by the pool
                token_in = token_in if i == 0 else token_out
                token_out = (
                    v2_pool.token0
                    if token_in is v2_pool.token1
                    else v2_pool.token1
                )

                current_state = v2_pool.state
                future_state = v2_pool.simulate_swap(
                    token_in=token_in,
                    token_in_quantity=token_in_quantity,
                )

                token_out_quantity = -min(
                    future_state["amount0_delta"],
                    future_state["amount1_delta"],
                )

                # if (
                #     future_state["reserves_token0"]
                #     < current_state["reserves_token0"]
                # ):
                #     token_out_quantity = (
                #         current_state["reserves_token0"]
                #         - future_state["reserves_token0"]
                #     )
                # elif (
                #     future_state["reserves_token1"]
                #     < current_state["reserves_token1"]
                # ):
                #     token_out_quantity = (
                #         current_state["reserves_token1"]
                #         - future_state["reserves_token1"]
                #     )
                # else:
                #     raise ValueError(
                #         "Swap direction could not be identified"
                #         "\n"
                #         f'{current_state["reserves_token0"]=}'
                #         "\n"
                #         f'{current_state["reserves_token1"]=}'
                #         "\n"
                #         f'{future_state["reserves_token0"]=}'
                #         "\n"
                #         f'{future_state["reserves_token1"]=}'
                #     )

                future_pool_states.append(
                    (
                        v2_pool,
                        future_state,
                    )
                )

                if not silent:
                    print(f"Simulating swap through pool: {v2_pool}")
                    print(
                        f"\t{token_in_quantity} {token_in} -> {token_out_quantity} {token_out}"
                    )
                    print("\t(CURRENT)")
                    print(
                        f"\t{v2_pool.token0}: {current_state['reserves_token0']}"
                    )
                    print(
                        f"\t{v2_pool.token1}: {current_state['reserves_token1']}"
                    )
                    print(f"\t(FUTURE)")
                    print(
                        f"\t{v2_pool.token0}: {future_state['reserves_token0']}"
                    )
                    print(
                        f"\t{v2_pool.token1}: {future_state['reserves_token1']}"
                    )

            return future_pool_states

        def v2_swap_exact_out(
            params: dict,
            unwrapped_input: Optional[bool] = False,
            silent: bool = False,
        ) -> List[Tuple[LiquidityPool, dict]]:

            pool_objects: List[LiquidityPool] = []
            for token_addresses in itertools.pairwise(params.get("path")):
                try:
                    pool_helper: LiquidityPool = self.v2_pool_manager.get_pool(
                        token_addresses=token_addresses,
                        silent=silent,
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
                silent=silent,
                min_abi=True,
                unload_brownie_contract_after_init=True,
            )
            token_out = Erc20TokenHelperManager().get_erc20token(
                address=params["path"][-1],
                silent=silent,
                min_abi=True,
                unload_brownie_contract_after_init=True,
            )

            swap_out_quantity = params.get("amountOut")

            if unwrapped_input:
                swap_in_quantity = self.value

            # predict future pool states assuming the swap executes in isolation
            # work through the pools backwards, since the swap will execute at a defined output, with input floating
            future_pool_states = []
            for i, v2_pool in enumerate(pool_objects[::-1]):
                token_out_quantity = (
                    swap_out_quantity if i == 0 else token_out_quantity
                )

                # i == 0 for last pool in path, take from 'path' in func_params
                # otherwise, set token_out equal to token_in from previous iteration
                # and token_in equal to the other token held by the pool
                token_out = token_out if i == 0 else token_in
                token_in = (
                    v2_pool.token0
                    if token_out is v2_pool.token1
                    else v2_pool.token1
                )

                current_state = v2_pool.state
                future_state = v2_pool.simulate_swap(
                    token_out=token_out,
                    token_out_quantity=token_out_quantity,
                )

                # print(f"{i}: {token_in} -> {token_out}")
                # print(f"{current_state=}")
                # print(f"{future_state=}")

                token_in_quantity = max(
                    future_state["amount0_delta"],
                    future_state["amount1_delta"],
                )

                future_pool_states.append(
                    (
                        v2_pool,
                        future_state,
                    )
                )

                if not silent:
                    print(f"Simulating swap through pool: {v2_pool}")
                    print(
                        f"\t{token_in_quantity} {token_in} -> {token_out_quantity} {token_out}"
                    )
                    print("\t(CURRENT)")
                    print(
                        f"\t{v2_pool.token0}: {current_state['reserves_token0']}"
                    )
                    print(
                        f"\t{v2_pool.token1}: {current_state['reserves_token1']}"
                    )
                    print(f"\t(FUTURE)")
                    print(
                        f"\t{v2_pool.token0}: {future_state['reserves_token0']}"
                    )
                    print(
                        f"\t{v2_pool.token1}: {future_state['reserves_token1']}"
                    )

            # if swap_in_quantity < token_in_quantity:
            #     raise TransactionError("msg.value too low for swap")

            return future_pool_states

        def v3_swap_exact_in(
            params: dict,
            silent: bool = False,
        ) -> List[Tuple[V3LiquidityPool, dict]]:

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
                ) = params.get("params")
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
                ) = params.get("params")
            except:
                pass

            # decode values from exactInput (hand-crafted)
            try:
                (
                    tokenIn,
                    tokenOut,
                    fee,
                    amountIn,
                ) = params.get("params")
            except:
                pass

            # decode a direct pool swap encoded by the Universal Router
            try:
                (
                    recipient,
                    zeroForOne,
                    amount,
                    zeroForOne,
                    path,
                    payer,
                ) = params.get("params")
            except:
                pass

            try:
                # get the V3 pool involved in the swap
                v3_pool = self.v3_pool_manager.get_pool(
                    token_addresses=(tokenIn, tokenOut),
                    pool_fee=fee,
                    silent=silent,
                )
            except (ManagerError, LiquidityPoolError) as e:
                raise TransactionError(f"Could not get pool (via tokens): {e}")
            except:
                raise

            if not silent:
                print(f"Predicting output of swap through pool: {v3_pool}")

            try:
                token_in_object = Erc20TokenHelperManager().get_erc20token(
                    address=tokenIn,
                    silent=silent,
                    min_abi=True,
                    unload_brownie_contract_after_init=True,
                )
            except Exception as e:
                print(e)
                print(type(e))
                raise

            try:
                final_state = v3_pool.simulate_swap(
                    token_in=token_in_object,
                    token_in_quantity=amountIn,
                )
            except EVMRevertError as e:
                raise TransactionError(
                    f"V3 operation could not be simulated: {e}"
                )

            return [
                (
                    v3_pool,
                    final_state,
                )
            ]

        def v3_swap_exact_out(
            params: dict,
            silent: bool = False,
        ) -> List[Tuple[V3LiquidityPool, dict]]:

            sqrtPriceLimitX96 = None
            amountInMaximum = None

            # decode with Router ABI
            # https://github.com/Uniswap/v3-periphery/blob/main/contracts/interfaces/ISwapRouter.sol
            try:
                (
                    tokenIn,
                    tokenOut,
                    fee,
                    recipient,
                    deadline,
                    amountOut,
                    amountInMaximum,
                    sqrtPriceLimitX96,
                ) = params.get("params")
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
                    amountOut,
                    amountInMaximum,
                    sqrtPriceLimitX96,
                ) = params.get("params")
            except:
                pass

            # decode values from exactOutput (hand-crafted)
            try:
                (
                    tokenIn,
                    tokenOut,
                    fee,
                    amountOut,
                ) = params.get("params")
            except:
                pass

            try:
                # get the V3 pool involved in the swap
                v3_pool = self.v3_pool_manager.get_pool(
                    token_addresses=(tokenIn, tokenOut),
                    pool_fee=fee,
                    silent=silent,
                )
            except (ManagerError, LiquidityPoolError) as e:
                raise TransactionError(f"Could not get pool (via tokens): {e}")
            except:
                raise

            if not silent:
                print(f"Predicting output of swap through pool: {v3_pool}")

            try:
                token_out_object = Erc20TokenHelperManager().get_erc20token(
                    address=tokenOut,
                    silent=silent,
                    min_abi=True,
                    unload_brownie_contract_after_init=True,
                )
            except Exception as e:
                print(e)
                print(type(e))
                raise

            try:
                final_state = v3_pool.simulate_swap(
                    token_out=token_out_object,
                    token_out_quantity=amountOut,
                    sqrt_price_limit=sqrtPriceLimitX96,
                )
            except EVMRevertError as e:
                raise TransactionError(
                    f"V3 operation could not be simulated: {e}"
                )

            # swap input is positive from the POV of the pool
            amountIn = max(
                final_state["amount0_delta"],
                final_state["amount1_delta"],
            )

            if amountInMaximum and amountIn < amountInMaximum:
                raise TransactionError(
                    f"amountIn ({amountIn}) < amountOutMin ({amountInMaximum})"
                )

            return [
                (
                    v3_pool,
                    final_state,
                )
            ]

        if func_name is None:
            func_name = self.func_name

        if func_params is None:
            func_params = self.func_params

        future_state = []

        try:

            # -----------------------------------------------------
            # UniswapV2 functions
            # -----------------------------------------------------
            if func_name in (
                "swapExactTokensForETH",
                "swapExactTokensForETHSupportingFeeOnTransferTokens",
            ):
                if not silent:
                    print(f"{func_name}: {self.hash}")
                future_state.extend(
                    v2_swap_exact_in(func_params, silent=silent)
                )

            elif func_name in (
                "swapExactETHForTokens",
                "swapExactETHForTokensSupportingFeeOnTransferTokens",
            ):
                if not silent:
                    print(f"{func_name}: {self.hash}")
                future_state.extend(
                    v2_swap_exact_in(
                        func_params, unwrapped_input=True, silent=silent
                    )
                )

            elif func_name in [
                "swapExactTokensForTokens",
                "swapExactTokensForTokensSupportingFeeOnTransferTokens",
            ]:
                if not silent:
                    print(f"{func_name}: {self.hash}")
                future_state.extend(
                    v2_swap_exact_in(func_params, silent=silent)
                )

            elif func_name in ("swapTokensForExactETH"):
                if not silent:
                    print(f"{func_name}: {self.hash}")
                future_state.extend(
                    v2_swap_exact_out(params=func_params, silent=silent)
                )

            elif func_name in ("swapTokensForExactTokens"):
                if not silent:
                    print(f"{func_name}: {self.hash}")
                future_state.extend(
                    v2_swap_exact_out(params=func_params, silent=silent)
                )

            elif func_name in ("swapETHForExactTokens"):
                if not silent:
                    print(f"{func_name}: {self.hash}")
                future_state.extend(
                    v2_swap_exact_out(
                        params=func_params, unwrapped_input=True, silent=silent
                    )
                )

            # -----------------------------------------------------
            # UniswapV3 functions
            # -----------------------------------------------------
            elif func_name == "multicall":
                if not silent:
                    print(f"{func_name}: {self.hash}")
                future_state = self.simulate_multicall(silent=silent)
            elif func_name == "exactInputSingle":
                if not silent:
                    print(f"{func_name}: {self.hash}")
                # v3_pool, swap_info, pool_state = v3_swap_exact_in(
                #     params=func_params
                # )
                # future_state.append([v3_pool, pool_state])
                future_state.extend(
                    v3_swap_exact_in(params=func_params, silent=silent)
                )
            elif func_name == "exactInput":
                if not silent:
                    print(f"{func_name}: {self.hash}")

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
                        and len(exactInputParams_path)
                        >= path_pos + byte_length
                    ):
                        address = exactInputParams_path[
                            path_pos : path_pos + byte_length
                        ].hex()
                        exactInputParams_path_decoded.append(address)
                    elif (
                        byte_length == 3
                        and len(exactInputParams_path)
                        >= path_pos + byte_length
                    ):
                        fee = int(
                            exactInputParams_path[
                                path_pos : path_pos + byte_length
                            ].hex(),
                            16,
                        )
                        exactInputParams_path_decoded.append(fee)
                    path_pos += byte_length

                if not silent:
                    print(f" • path = {exactInputParams_path_decoded}")
                    print(f" • recipient = {exactInputParams_recipient}")
                    try:
                        exactInputParams_deadline
                    except:
                        pass
                    else:
                        print(f" • deadline = {exactInputParams_deadline}")
                    print(f" • amountIn = {exactInputParams_amountIn}")
                    print(
                        f" • amountOutMinimum = {exactInputParams_amountOutMinimum}"
                    )

                # decode the path - tokenIn is the first position, tokenOut is the second position
                # e.g. tokenIn, fee, tokenOut
                for token_pos in range(
                    0,
                    len(exactInputParams_path_decoded) - 2,
                    2,
                ):
                    tokenIn = exactInputParams_path_decoded[token_pos]
                    fee = exactInputParams_path_decoded[token_pos + 1]
                    tokenOut = exactInputParams_path_decoded[token_pos + 2]

                    v3_pool, pool_state = v3_swap_exact_in(
                        params={
                            "params": (
                                tokenIn,
                                tokenOut,
                                fee,
                                # use amountIn for the first swap, otherwise take the output
                                # amount of the last swap (always negative so we can check
                                # for the min without knowing the token positions)
                                exactInputParams_amountIn
                                if token_pos == 0
                                else min(
                                    pool_state["amount0_delta"],
                                    pool_state["amount1_delta"],
                                ),
                            )
                        },
                        silent=silent,
                    )[0]
                    future_state.append([v3_pool, pool_state])
            elif func_name == "exactOutputSingle":
                if not silent:
                    print(f"{func_name}: {self.hash}")
                # v3_pool, swap_info, pool_state = v3_swap_exact_out(
                #     params=func_params
                # )
                future_state.extend(
                    v3_swap_exact_out(params=func_params, silent=silent)
                )
            elif func_name == "exactOutput":
                if not silent:
                    print(f"{func_name}: {self.hash}")

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
                        and len(exactOutputParams_path)
                        >= path_pos + byte_length
                    ):
                        address = exactOutputParams_path[
                            path_pos : path_pos + byte_length
                        ].hex()
                        exactOutputParams_path_decoded.append(address)
                    elif (
                        byte_length == 3
                        and len(exactOutputParams_path)
                        >= path_pos + byte_length
                    ):
                        fee = int(
                            exactOutputParams_path[
                                path_pos : path_pos + byte_length
                            ].hex(),
                            16,
                        )
                        exactOutputParams_path_decoded.append(fee)
                    path_pos += byte_length

                if not silent:
                    print(f" • path = {exactOutputParams_path_decoded}")
                    print(f" • recipient = {exactOutputParams_recipient}")
                    if exactOutputParams_deadline:
                        print(f" • deadline = {exactOutputParams_deadline}")
                    print(f" • amountOut = {exactOutputParams_amountOut}")
                    print(
                        f" • amountInMaximum = {exactOutputParams_amountInMaximum}"
                    )

                # the path is encoded in REVERSE order, so we decode from start to finish
                # tokenOut is the first position, tokenIn is the second position
                # e.g. tokenOut, fee, tokenIn
                for token_pos in range(
                    0,
                    len(exactOutputParams_path_decoded) - 2,
                    2,
                ):
                    tokenOut = exactOutputParams_path_decoded[token_pos]
                    fee = exactOutputParams_path_decoded[token_pos + 1]
                    tokenIn = exactOutputParams_path_decoded[token_pos + 2]

                    v3_pool, pool_state = v3_swap_exact_out(
                        params={
                            "params": (
                                tokenIn,
                                tokenOut,
                                fee,
                                # use amountOut for the last swap (token_pos == 0),
                                # otherwise take the input amount of the previous swap
                                # (always positive so we can check for the max without
                                # knowing the token positions)
                                exactOutputParams_amountOut
                                if token_pos == 0
                                else max(
                                    pool_state["amount0_delta"],
                                    pool_state["amount1_delta"],
                                )
                                # else max(swap_info.values()),
                            )
                        },
                        silent=silent,
                    )[0]

                    future_state.append([v3_pool, pool_state])

            # -----------------------------------------------------
            # Universal Router functions
            # -----------------------------------------------------
            elif func_name == "execute":

                COMMANDS = {
                    0x00: "V3_SWAP_EXACT_IN",
                    0x01: "V3_SWAP_EXACT_OUT",
                    0x02: "PERMIT2_TRANSFER_FROM",
                    0x03: "PERMIT2_PERMIT_BATCH",
                    0x04: "SWEEP",
                    0x05: "TRANSFER",
                    0x06: "PAY_PORTION",
                    0x07: None,  # COMMAND_PLACEHOLDER
                    0x08: "V2_SWAP_EXACT_IN",
                    0x09: "V2_SWAP_EXACT_OUT",
                    0x0A: "PERMIT2_PERMIT",
                    0x0B: "WRAP_ETH",
                    0x0C: "UNWRAP_WETH",
                    0x0D: "ERMIT2_TRANSFER_FROM_BATCH",
                    0x0E: "BALANCE_CHECK_ERC20",
                    0x0F: None,  # COMMAND_PLACEHOLDER
                    0x10: "SEAPORT",
                    0x11: "LOOKS_RARE_721",
                    0x12: "NFTX",
                    0x13: "CRYPTOPUNKS",
                    0x14: "LOOKS_RARE_1155",
                    0x15: "OWNER_CHECK_721",
                    0x16: "OWNER_CHECK_1155",
                    0x17: "SWEEP_ERC721",
                    0x18: "X2Y2_721",
                    0x19: "SUDOSWAP",
                    0x1A: "NFT20",
                    0x1B: "X2Y2_1155",
                    0x1C: "FOUNDATION",
                    0x1D: "SWEEP_ERC1155",
                    0x1E: "ELEMENT_MARKET",
                    0x1F: None,  # COMMAND_PLACEHOLDER
                    0x20: "EXECUTE_SUB_PLAN",
                    0x21: "SEAPORT_V2",
                }

                def simulate_dispatch(command_type: bytes, inputs: bytes):

                    COMMAND_TYPE_MASK = 0x3F
                    command = COMMANDS[command_type & COMMAND_TYPE_MASK]

                    print(command)

                    result = []

                    if command == "V3_SWAP_EXACT_IN":

                        if not silent:
                            print(f"{func_name}: {self.hash}")

                        # equivalent: abi.decode(inputs, (address, uint256, uint256, bytes, bool))
                        (
                            recipient,
                            amountIn,
                            amountOutMin,
                            path,
                            payerIsUser,
                        ) = eth_abi.decode(
                            ["address", "uint256", "uint256", "bytes", "bool"],
                            inputs,
                        )

                        exactInputParams_path_decoded = decode_v3_path(path)

                        # decode the path - tokenIn is the first position, tokenOut is the second position
                        # e.g. tokenIn, fee, tokenOut
                        for token_pos in range(
                            0,
                            len(exactInputParams_path_decoded) - 2,
                            2,
                        ):
                            tokenIn = exactInputParams_path_decoded[token_pos]
                            fee = exactInputParams_path_decoded[token_pos + 1]
                            tokenOut = exactInputParams_path_decoded[
                                token_pos + 2
                            ]

                            v3_pool, pool_state = v3_swap_exact_in(
                                params={
                                    "params": (
                                        tokenIn,
                                        tokenOut,
                                        fee,
                                        # use amountIn for the first swap, otherwise take the output
                                        # amount of the last swap (always negative so we can check
                                        # for the min without knowing the token positions)
                                        amountIn
                                        if token_pos == 0
                                        else min(
                                            pool_state["amount0_delta"],
                                            pool_state["amount1_delta"],
                                        ),
                                    )
                                },
                                silent=silent,
                            )[0]
                            result.append([v3_pool, pool_state])

                        return result

                    elif command == "V3_SWAP_EXACT_OUT":

                        if not silent:
                            print(f"{func_name}: {self.hash}")

                        # equivalent: abi.decode(inputs, (address, uint256, uint256, bytes, bool))
                        (
                            recipient,
                            amountOut,
                            amountInMax,
                            path,
                            payerIsUser,
                        ) = eth_abi.decode(
                            ["address", "uint256", "uint256", "bytes", "bool"],
                            inputs,
                        )

                        exactOutputParams_path_decoded = decode_v3_path(path)

                        # the path is encoded in REVERSE order, so we decode from start to finish
                        # tokenOut is the first position, tokenIn is the second position
                        # e.g. tokenOut, fee, tokenIn
                        for token_pos in range(
                            0,
                            len(exactOutputParams_path_decoded) - 2,
                            2,
                        ):
                            tokenOut = exactOutputParams_path_decoded[
                                token_pos
                            ]
                            fee = exactOutputParams_path_decoded[token_pos + 1]
                            tokenIn = exactOutputParams_path_decoded[
                                token_pos + 2
                            ]

                            v3_pool, pool_state = v3_swap_exact_out(
                                params={
                                    "params": (
                                        tokenIn,
                                        tokenOut,
                                        fee,
                                        # use amountOut for the last swap (token_pos == 0),
                                        # otherwise take the input amount of the previous swap
                                        # (always positive so we can check for the max without
                                        # knowing the token positions)
                                        amountOut
                                        if token_pos == 0
                                        else max(
                                            pool_state["amount0_delta"],
                                            pool_state["amount1_delta"],
                                        )
                                        # else max(swap_info.values()),
                                    )
                                },
                                silent=silent,
                            )[0]

                            future_state.append([v3_pool, pool_state])

                        return result

                    elif command == "PERMIT2_TRANSFER_FROM":
                        pass
                    elif command == "PERMIT2_PERMIT_BATCH":
                        pass
                    elif command == "SWEEP":
                        pass
                    elif command == "TRANSFER":
                        pass
                    elif command == "PAY_PORTION":
                        pass
                    elif command == "V2_SWAP_EXACT_IN":

                        if not silent:
                            print(f"{func_name}: {self.hash}")

                        # equivalent: abi.decode(inputs, (address, uint256, uint256, bytes, bool))
                        (
                            recipient,
                            amountIn,
                            amountOutMin,
                            path,
                            payerIsUser,
                        ) = eth_abi.decode(
                            [
                                "address",
                                "uint256",
                                "uint256",
                                "address[]",
                                "bool",
                            ],
                            inputs,
                        )

                        func_params = {
                            "amountIn": amountIn,
                            "amountOutMin": amountOutMin,
                            "path": path,
                            "to": recipient,
                        }

                        result.extend(
                            v2_swap_exact_in(func_params, silent=silent)
                        )

                        return result

                    elif command == "V2_SWAP_EXACT_OUT":

                        if not silent:
                            print(f"{func_name}: {self.hash}")

                        # equivalent: abi.decode(inputs, (address, uint256, uint256, bytes, bool))
                        (
                            recipient,
                            amountOut,
                            amountInMax,
                            path,
                            payerIsUser,
                        ) = eth_abi.decode(
                            [
                                "address",
                                "uint256",
                                "uint256",
                                "address[]",
                                "bool",
                            ],
                            inputs,
                        )

                        func_params = {
                            "amountOut": amountOut,
                            "amountInMax": amountInMax,
                            "path": path,
                            "to": recipient,
                        }

                        result.extend(
                            v2_swap_exact_out(func_params, silent=silent)
                        )

                        return result
                    elif command == "PERMIT2_PERMIT":
                        pass
                    elif command == "WRAP_ETH":
                        pass
                    elif command == "UNWRAP_WETH":
                        pass
                    elif command == "PERMIT2_TRANSFER_FROM_BATCH":
                        pass
                    elif command == "BALANCE_CHECK_ERC20":
                        pass
                    elif command == "SEAPORT":
                        pass
                    elif command == "LOOKS_RARE_721":
                        pass
                    elif command == "NFTX":
                        pass
                    elif command == "CRYPTOPUNKS":
                        pass
                    elif command == "LOOKS_RARE_1155":
                        pass
                    elif command == "OWNER_CHECK_721":
                        pass
                    elif command == "OWNER_CHECK_1155":
                        pass
                    elif command == "SWEEP_ERC721":
                        pass
                    elif command == "X2Y2_721":
                        pass
                    elif command == "SUDOSWAP":
                        pass
                    elif command == "NFT20":
                        pass
                    elif command == "X2Y2_1155":
                        pass
                    elif command == "FOUNDATION":
                        pass
                    elif command == "SWEEP_ERC1155":
                        pass
                    elif command == "ELEMENT_MARKET":
                        pass
                    elif command == "EXECUTE_SUB_PLAN":
                        pass
                    elif command == "SEAPORT_V2":
                        pass
                    else:
                        raise TransactionError(f"Invalid command {command}")

                if not silent:
                    print(f"{func_name}: {self.hash}")

                commands = func_params.get("commands")
                inputs = func_params.get("inputs")
                deadline = func_params.get("deadline")

                future_state = []

                for idx in range(len(commands)):
                    command = commands[idx]
                    input = inputs[idx]
                    if result := simulate_dispatch(command, input):
                        future_state.extend(result)

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
                if not silent:
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

        # WIP: catch generic DegenbotError (non-fatal) and ValueError (bad inputs),
        # allow the rest to escape
        except (DegenbotError, ValueError) as e:
            raise TransactionError(f"Transaction could not be calculated: {e}")
        else:
            return future_state

    def simulate_multicall(self, silent: bool = False):

        future_state = []

        for payload in self.func_params.get("data"):
            try:
                # decode with Router ABI
                payload_func, payload_args = (
                    web3.Web3()
                    .eth.contract(abi=UNISWAP_V3_ROUTER_ABI)
                    .decode_function_input(payload)
                )
            except:
                pass

            try:
                # decode with Router2 ABI
                payload_func, payload_args = (
                    web3.Web3()
                    .eth.contract(abi=UNISWAP_V3_ROUTER2_ABI)
                    .decode_function_input(payload)
                )
            except:
                pass

            if payload_func.fn_name == "multicall":

                if not silent:
                    print("Unwrapping nested multicall")

                for payload in payload_args["data"]:
                    try:
                        _func, _params = (
                            web3.Web3()
                            .eth.contract(abi=UNISWAP_V3_ROUTER_ABI)
                            .decode_function_input(payload)
                        )
                    except:
                        pass

                    try:
                        _func, _params = (
                            web3.Web3()
                            .eth.contract(abi=UNISWAP_V3_ROUTER2_ABI)
                            .decode_function_input(payload)
                        )
                    except:
                        pass

                    try:
                        # simulate each payload individually and append its result to
                        # the future_state tuple
                        future_state.extend(
                            self.simulate(
                                func_name=_func.fn_name,
                                func_params=_params,
                                silent=silent,
                            )
                        )
                    except Exception as e:
                        raise TransactionError(
                            f"Could not decode nested multicall: {e}"
                        )

            else:
                try:
                    # simulate each payload individually and append its result to
                    # the future_state tuple
                    future_state.extend(
                        self.simulate(
                            func_name=payload_func.fn_name,
                            func_params=payload_args,
                            silent=silent,
                        )
                    )
                except Exception as e:
                    raise TransactionError(f"Could not decode multicall: {e}")

        return future_state
