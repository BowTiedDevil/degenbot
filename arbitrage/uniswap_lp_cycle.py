from typing import List, Optional, Tuple, Union
from warnings import warn

from eth_abi import encode as abi_encode
from scipy import optimize
from web3 import Web3

from degenbot.arbitrage.base import Arbitrage
from degenbot.exceptions import (
    ArbitrageError,
    EVMRevertError,
    LiquidityPoolError,
    ZeroLiquidityError,
    ZeroSwapError,
)
from degenbot.token import Erc20Token
from degenbot.uniswap.v2.liquidity_pool import LiquidityPool
from degenbot.uniswap.v3.v3_liquidity_pool import V3LiquidityPool
from degenbot.uniswap.v3.libraries import TickMath


class UniswapLpCycle(Arbitrage):
    def __init__(
        self,
        input_token: Erc20Token,
        swap_pools: List[Union[LiquidityPool, V3LiquidityPool]],
        max_input: Optional[int] = None,
        id: Optional[str] = None,
    ):

        self.id = id
        self.input_token = input_token

        if max_input is None:
            warn("No maximum input provided, setting to 100 WETH")
            max_input = 100 * 10**18
        self.max_input = max_input

        self.gas_estimate = 0

        for pool in swap_pools:
            if pool.uniswap_version not in [2, 3]:
                raise ArbitrageError(
                    f"Could not identify Uniswap version for pool {pool}!"
                )
        self.swap_pools = swap_pools
        self.swap_pool_addresses = [pool.address for pool in self.swap_pools]
        self.swap_pool_tokens = [
            [pool.token0, pool.token1] for pool in self.swap_pools
        ]
        self.swap_vectors = []
        for i, pool in enumerate(self.swap_pools):
            if i == 0:
                if self.input_token == pool.token0:
                    zeroForOne = True
                    token_in = pool.token0
                    token_out = pool.token1
                elif self.input_token == pool.token1:
                    zeroForOne = False
                    token_in = pool.token1
                    token_out = pool.token0
                else:
                    raise ArbitrageError("Token could not be identified!")
            else:
                # token_out references the output from the previous pool
                if token_out == pool.token0:
                    zeroForOne = True
                    token_in = pool.token0
                    token_out = pool.token1
                elif token_out == pool.token1:
                    zeroForOne = False
                    token_in = pool.token1
                    token_out = pool.token0
                else:
                    raise ArbitrageError("Token could not be identified!")
            self.swap_vectors.append(
                {
                    "token_in": token_in,
                    "token_out": token_out,
                    "zeroForOne": zeroForOne,
                }
            )

        # print(f"swap_token array built:")
        # for i, swap_pair in enumerate(self.swap_tokens):
        #     print(
        #         f"Pool {i} swap {swap_pair.get('token_in')} -> {swap_pair.get('token_out')}"
        #     )

        self.name = " -> ".join([pool.name for pool in self.swap_pools])

        # WIP: add state tracking for all pools. Populated from relevant state data from each pool,
        # retrieved directly from pool.state attribute
        self.pool_states = {}
        self._update_pool_states()

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

    def __str__(self) -> str:
        return self.name

    def _build_amounts_out(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: Optional[
            List[
                Tuple[
                    Union[LiquidityPool, V3LiquidityPool],
                    dict,
                ]
            ]
        ] = None,
    ) -> List[dict]:

        pools_amounts_out = []

        # sort the override_state values into a dictionary for fast lookup inside the calculation loop
        _overrides = (
            {pool.address: state for pool, state in override_state}
            if override_state is not None
            else {}
        )

        for i, pool in enumerate(self.swap_pools):

            token0 = pool.token0
            token1 = pool.token1

            if token_in == token0:
                token_out = token1
                _zeroForOne = True
            elif token_in == token1:
                token_out = token0
                _zeroForOne = False
            else:
                raise ValueError(
                    f"Could not identify token_in! Found {token_in}, pool holds {token0}, {token1} "
                )

            try:
                # calculate the swap output through pool
                token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                    token_in=token_in,
                    token_in_quantity=token_in_quantity,
                    override_state=_overrides.get(pool.address),
                )
            except LiquidityPoolError as e:
                raise ArbitrageError(
                    f"(calculate_tokens_out_from_tokens_in): {e}"
                )
            else:
                if token_out_quantity == 0:
                    raise ZeroSwapError(
                        f"Zero-output swap through pool {pool}"
                    )

            # determine the uniswap version for the pool and format the output appropriately
            if pool.uniswap_version == 2:
                pools_amounts_out.append(
                    {
                        "uniswap_version": 2,
                        "amounts": [0, token_out_quantity]
                        if _zeroForOne
                        else [token_out_quantity, 0],
                    }
                )
            elif pool.uniswap_version == 3:
                pools_amounts_out.append(
                    {
                        "uniswap_version": 3,
                        # for an exactInput swap, amountSpecified is a positive number representing the INPUT amount
                        # for an exactOutput swap, amountSpecified is a negative number representing the OUTPUT amount
                        # specify exactInput for first leg (i==0), exactOutput for others
                        "amountSpecified": token_in_quantity
                        if i == 0
                        else -token_out_quantity,
                        "zeroForOne": _zeroForOne,
                        "sqrtPriceLimitX96": TickMath.MIN_SQRT_RATIO + 1
                        if _zeroForOne
                        else TickMath.MAX_SQRT_RATIO - 1,
                    }
                )
            else:
                raise ValueError(
                    f"Could not identify Uniswap version for pool: {self.swap_pools[i]}"
                )

            # feed the output into input, continue the loop
            token_in = token_out
            token_in_quantity = token_out_quantity

        return pools_amounts_out

    def _update_pool_states(self):
        """
        Internal method to update the `self.pool_states` state tracking dict
        """
        self.pool_states = {
            pool.address: pool.state for pool in self.swap_pools
        }

    def auto_update(
        self,
        silent: bool = True,
        block_number: Optional[int] = None,
        override_update_method: Optional[str] = None,
    ) -> bool:

        found_updates = False

        if override_update_method:
            print(f"OVERRIDDEN UPDATE METHOD: {override_update_method}")

        for pool in self.swap_pools:
            if (
                pool._update_method == "polling"
                or override_update_method == "polling"
            ):
                if pool.uniswap_version == 2:
                    pool: LiquidityPool
                    pool_updated = pool.update_reserves(
                        silent=silent,
                        override_update_method=override_update_method,
                        update_block=block_number,
                    )
                    if pool_updated:
                        if not silent:
                            print(
                                f"(UniswapLpCycle) found update for pool {pool}"
                            )
                        found_updates = True
                elif pool.uniswap_version == 3:
                    pool: V3LiquidityPool
                    pool_updated, _ = pool.auto_update(
                        silent=silent,
                        block_number=block_number,
                    )
                    if pool_updated:
                        if not silent:
                            print(
                                f"(UniswapLpCycle) found update for pool {pool}"
                            )
                        found_updates = True
                else:
                    print("could not determine Uniswap pool version!")
            elif pool._update_method == "external":
                if pool.state != self.pool_states[pool.address]:
                    found_updates = True
            else:
                raise ValueError(
                    "auto_update: could not determine update method!"
                )

        if found_updates:
            self._update_pool_states()
            self.clear_best()

        return found_updates

    def calculate_arbitrage(
        self,
        override_state: List[
            Tuple[
                Union[LiquidityPool, V3LiquidityPool],
                dict,
            ]
        ] = None,
    ) -> Tuple[bool, Tuple[int, int]]:

        if self.best["init"] == True:
            # if the 'init' flag is True, the `best` dict is empty so the calc should be done for the
            # first time, regardless of state
            self.auto_update()
            self.best["init"] = False
        elif self.pool_states == {
            pool.address: pool.state for pool in self.swap_pools
        }:
            # short-circuit if pool state has not changed,
            # return previously-calculated swap and profit values
            return False, (
                self.best["swap_amount"],
                self.best["profit_amount"],
            )
        else:
            self._update_pool_states()

        # sort the override_state values into a dictionary for fast lookup
        # inside the calculation loop
        _overrides = {}
        if override_state is not None:
            for pool, state in override_state:
                _overrides[pool.address] = state

        # check the pools for zero liquidity in the direction of the trade
        for i, pool in enumerate(self.swap_pools):

            if pool.uniswap_version == 2 and (
                pool.reserves_token0 <= 1 or pool.reserves_token1 <= 1
            ):
                raise ZeroLiquidityError(
                    f"V2 pool {pool.address} has no liquidity"
                )

            if pool.uniswap_version == 3 and pool.state["liquidity"] == 0:

                # check if the swap is 0 -> 1 and cannot swap any more token0 for token1
                if (
                    self.swap_vectors[i]["zeroForOne"]
                    and pool.state["sqrt_price_x96"]
                    == TickMath.MIN_SQRT_RATIO + 1
                ):
                    raise ZeroLiquidityError(
                        f"V3 pool {pool.address} has no liquidity for a 0 -> 1 swap"
                    )
                # check if the swap is 1 -> 0 (zeroForOne=False) and cannot swap any more token1 for token0
                elif (
                    not self.swap_vectors[i]["zeroForOne"]
                    and pool.state["sqrt_price_x96"]
                    == TickMath.MAX_SQRT_RATIO - 1
                ):
                    raise ZeroLiquidityError(
                        f"V3 pool {pool.address} has no liquidity for a 1 -> 0 swap"
                    )

        # bound the amount to be swapped
        bounds = (
            1,
            float(self.max_input),
        )

        # bracket the initial guess for the algo
        bracket = (
            0.1,
            0.2 * self.max_input,
        )

        def arb_profit(x):

            token_in_quantity = int(x)  # round the input down

            for i, pool in enumerate(self.swap_pools):
                try:
                    token_in = self.swap_vectors[i]["token_in"]
                    token_out_quantity = (
                        pool.calculate_tokens_out_from_tokens_in(
                            token_in=token_in,
                            token_in_quantity=token_in_quantity
                            if i == 0
                            else token_out_quantity,
                            override_state=_overrides.get(pool.address),
                        )
                    )
                except (EVMRevertError, LiquidityPoolError) as e:
                    # The optimizer might send invalid data into the swap calculation during
                    # iteration. We don't want it to stop, so catch the exception and pretend
                    # the swap results in token_out_quantity = 0.
                    token_out_quantity = 0

            return -float(token_out_quantity - token_in_quantity)

            # try:
            #     for i, pool in enumerate(self.swap_pools):
            #         token_in = self.swap_vectors[i]["token_in"]
            #         token_out_quantity = (
            #             pool.calculate_tokens_out_from_tokens_in(
            #                 token_in=token_in,
            #                 token_in_quantity=x
            #                 if i == 0
            #                 else token_out_quantity,
            #                 override_state=_overrides.get(pool.address),
            #             )
            #         )
            # except (EVMRevertError, LiquidityPoolError):
            #     # The optimizer might send invalid data into the swap calculation during
            #     # iteration. We don't want it to stop, so catch the exception and pretend
            #     # the swap results in token_out_quantity = 0.
            #     token_out_quantity = 0

            # return -float(token_out_quantity - x)

        opt = optimize.minimize_scalar(
            fun=arb_profit,
            method="bounded",
            bounds=bounds,
            bracket=bracket,
            # Optimizer will run until the consecutive input values
            # are within `xatol`. ERC-20 tokens can have different decimal precision,
            # so set the tolerance to 0.1% of the 'nominal' decimal digits
            #
            # Examples:
            #   WETH (18 decimal places) calculated within 1*10**14 Wei,
            #   USDC (6 decimal places) calculated within 1*10**2 Wei
            options={"xatol": 10 ** (self.input_token.decimals - 4)},
        )

        # The arb_profit function converts the value to a negative number so the minimize_scalar
        # correctly finds the optimum input. However we need a practical positive profit,
        # so we negate the result afterwards
        swap_amount = int(opt.x)
        best_profit = -int(opt.fun)

        try:
            best_amounts = self._build_amounts_out(
                token_in=self.input_token,
                token_in_quantity=swap_amount,
                override_state=override_state,
            )
        # except (EVMRevertError, LiquidityPoolError) as e:
        except ArbitrageError as e:
            # Simulated EVM reverts inside the ported `swap` function were ignored to execute the optimizer
            # through to completion, but now we want to raise a real error to avoid generating bad payloads
            # that will revert
            raise ArbitrageError(f"No possible arbitrage: {e}") from None
        except Exception as e:
            raise ArbitrageError(f"No possible arbitrage: {e}") from e
        else:
            # print(f"built multipool amounts! {self.name}")
            self.best.update(
                {
                    "swap_amount": swap_amount,
                    "profit_amount": best_profit,
                    "swap_pool_amounts": best_amounts,
                }
            )

        profitable = True if best_profit > 0 else False
        return profitable, (swap_amount, best_profit)

    def clear_best(self):
        self.best.update(
            {
                "swap_amount": 0,
                "profit_amount": 0,
                "swap_pool_amounts": [],
            }
        )

    @classmethod
    def from_addresses(
        cls,
        input_token_address: str,
        swap_pool_addresses: List[Tuple[str, str]],
        max_input: Optional[int] = None,
        id: Optional[str] = None,
    ) -> "UniswapLpCycle":
        """
        Create a new `UniswapLpCycle` object from token and pool addresses.

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

        max_input: int, optional
            The maximum input for the cycle token in question
            (typically limited by the balance of the deployed contract or operating EOA)
        id: str, optional
            A unique identifier for bookkeeping purposes
            (not used internally, the attribute is provided for operator convenience)
        """

        # create the token object
        token = Erc20Token(
            input_token_address,
            min_abi=True,
        )

        # create the pool objects
        pool_objects = []
        for pool_address, pool_type in swap_pool_addresses:
            # determine if the pool is a V2 or V3 type
            if pool_type == "V2":
                pool_objects.append(LiquidityPool(address=pool_address))
            elif pool_type == "V3":
                pool_objects.append(V3LiquidityPool(address=pool_address))
            else:
                raise ArbitrageError(
                    f"Pool type not understood! Expected 'V2' or 'V3', got {pool_type}"
                )

        return cls(
            input_token=token,
            swap_pools=pool_objects,
            max_input=max_input,
            id=id,
        )

    def generate_payloads(
        self,
        from_address: str,
    ) -> List[Tuple[str, bytes, int]]:
        """
        Generates a list of calldata payloads for each step in the swap path, with calldata built using the eth_abi.encode method
        and the `swap` function of either the V2 or V3 pool

        Arguments
        ---------
        from_address: str
            The smart contract address that will receive all token swap transfers and implement the necessary V3 callbacks

        Returns
        ---------
        payloads: List[Tuple[str, bytes, int]]
            A list of payloads, formatted as a tuple: (address, calldata, msg.value)
        """

        # # check all amounts for zero-amount swaps, which will execute but are undesirable
        # # print('checking pool amounts')
        # # print(self.best['swap_pool_amounts'])
        # if not self.best["swap_pool_amounts"]:
        #     raise ArbitrageError("No arbitrage results available")

        # web3py object without a provider, useful for offline transaction creation and signing
        w3 = Web3()

        payloads = []

        try:

            # generate the payload for the initial transfer if the first pool is type V2
            if self.swap_pools[0].uniswap_version == 2:

                # transfer the input token to the first swap pool
                transfer_payload = (
                    # address
                    self.input_token.address,
                    # bytes calldata
                    w3.keccak(text="transfer(address,uint256)")[0:4]
                    + abi_encode(
                        [
                            "address",
                            "uint256",
                        ],
                        [
                            self.swap_pool_addresses[0],
                            self.best.get("swap_amount"),
                        ],
                    ),
                    0,  # msg.value
                )

                payloads.append(transfer_payload)

            # generate the swap payloads for each pool in the path

            last_pool = self.swap_pools[-1]
            # print("\tPAYLOAD: identified last pool")
            for i, swap_pool_object in enumerate(self.swap_pools):

                if swap_pool_object is last_pool:
                    next_pool = None
                else:
                    next_pool = self.swap_pools[i + 1]

                if next_pool is not None:
                    # if it is a V2 pool, set swap destination to its address
                    if next_pool.uniswap_version == 2:
                        swap_destination_address = next_pool.address
                    # if it is a V3 pool, set swap destination to `from_address`
                    elif next_pool.uniswap_version == 3:
                        swap_destination_address = from_address
                else:
                    # we have reached the last pool, so set the destination to `from_address` regardless of type
                    swap_destination_address = from_address

                if swap_pool_object.uniswap_version == 2:
                    # print(f"\tPAYLOAD: building V2 swap at pool {i}")
                    # print(f"\tPAYLOAD: pool address {swap_pool_object.address}")
                    # print(
                    #     f'\tPAYLOAD: swap amounts {self.best["swap_pool_amounts"][i]["amounts"]}'
                    # )
                    # print(f"\tPAYLOAD: destination address {swap_destination_address}")
                    payloads.append(
                        (
                            # address
                            swap_pool_object.address,
                            # bytes calldata
                            w3.keccak(
                                text="swap(uint256,uint256,address,bytes)"
                            )[0:4]
                            + abi_encode(
                                [
                                    "uint256",
                                    "uint256",
                                    "address",
                                    "bytes",
                                ],
                                [
                                    *self.best["swap_pool_amounts"][i][
                                        "amounts"
                                    ],
                                    swap_destination_address,
                                    b"",
                                ],
                            ),
                            0,  # msg.value
                        )
                    )
                elif swap_pool_object.uniswap_version == 3:
                    # print(f"\tPAYLOAD: building V3 swap at pool {i}")
                    # print(f"\tPAYLOAD: pool address {swap_pool_object.address}")
                    # print(
                    #     f'\tPAYLOAD: swap amounts {self.best["swap_pool_amounts"][i]}'
                    # )
                    # print(f"\tPAYLOAD: destination address {swap_destination_address}")
                    payloads.append(
                        (
                            # address
                            swap_pool_object.address,
                            # bytes calldata
                            w3.keccak(
                                text="swap(address,bool,int256,uint160,bytes)"
                            )[0:4]
                            + abi_encode(
                                [
                                    "address",
                                    "bool",
                                    "int256",
                                    "uint160",
                                    "bytes",
                                ],
                                [
                                    swap_destination_address,
                                    self.best.get("swap_pool_amounts")[i][
                                        "zeroForOne"
                                    ],
                                    self.best.get("swap_pool_amounts")[i][
                                        "amountSpecified"
                                    ],
                                    self.best.get("swap_pool_amounts")[i][
                                        "sqrtPriceLimitX96"
                                    ],
                                    b"",
                                ],
                            ),
                            0,  # msg.value
                        )
                    )
                else:
                    raise ValueError(
                        f"Could not determine Uniswap version for pool: {swap_pool_object}"
                    )
        except Exception as e:
            print(self.best)
            print(f"id: {self.id}")
            raise ArbitrageError(f"generate_payloads (catch-all)): {e}") from e

        return payloads
