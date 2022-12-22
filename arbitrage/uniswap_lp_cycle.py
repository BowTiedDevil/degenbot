from typing import List, Tuple, Union

from eth_abi import encode as abi_encode
from scipy import optimize
from web3 import Web3
from math import ceil, floor

from degenbot.arbitrage.base import Arbitrage
from degenbot.exceptions import (
    ArbCalculationError,
    DegenbotError,
    InvalidSwapPathError,
)
from degenbot.token import Erc20Token
from degenbot.uniswap.v2 import LiquidityPool
from degenbot.uniswap.v3 import V3LiquidityPool
from degenbot.uniswap.v3.libraries import TickMath


class UniswapLpCycle(Arbitrage):
    def __init__(
        self,
        input_token: Erc20Token,
        swap_pools: List[Union[LiquidityPool, V3LiquidityPool]],
        max_input: int,
        name: str = "",
        id: str = None,
    ):

        self.id = id if id else None

        if name:
            self.name = name

        self.input_token = input_token
        self.max_input = max_input
        self.gas_estimate = 0

        for pool in swap_pools:
            assert pool.uniswap_version in [2, 3], DegenbotError(
                f"Could not identify Uniswap version for pool {pool}!"
            )
        self.swap_pools = swap_pools
        self.swap_pool_addresses = [pool.address for pool in self.swap_pools]
        self.swap_pool_tokens = [
            [pool.token0, pool.token1] for pool in self.swap_pools
        ]

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

    @classmethod
    def from_addresses(
        cls,
        input_token_address: str,
        swap_pool_addresses: List[Tuple[str, str]],
        name: str = "",
        max_input: int = None,
        id: str = None,
    ) -> "UniswapLpCycle":
        """
        Create a new `V3LpSwap` object from token and pool addresses.

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

        name : str, optional
            The display name for the helper
        max_input: int, optional
            The maximum input for the cycle token in question
            (typically limited by the balance of the deployed contract or operating EOA)
        id: str, optional
            A unique identifier for bookkeeping purposes
            (not used internally, the attribute is provided for operator convenience)
        """

        # create the token object
        try:
            token = Erc20Token(input_token_address)
        except:
            raise

        # create the pool objects
        pool_objects = []
        for pool_address, pool_type in swap_pool_addresses:
            # determine if the pool is a V2 or V3 type
            if pool_type == "V2":
                pool_objects.append(LiquidityPool(address=pool_address))
            elif pool_type == "V3":
                pool_objects.append(V3LiquidityPool(address=pool_address))
            else:
                raise DegenbotError(
                    f"Pool type not understood! Expected 'V2' or 'V3', got {pool_type}"
                )

        return cls(
            input_token=token,
            swap_pools=pool_objects,
            name=name,
            max_input=max_input,
            id=id,
        )

    def _update_pool_states(self):
        """
        Internal method to update the `self.pool_states` state tracking dict
        """
        self.pool_states = {
            pool.address: pool.state for pool in self.swap_pools
        }

    def auto_update(self, silent=True, block_number=None) -> bool:

        # TODO: implement block_number check
        # TODO: check `self.pool_states`` to short-circuit updates if nothing has changed

        found_updates = False
        for pool in self.swap_pools:
            if pool._update_method == "polling":
                if pool.uniswap_version == 2:
                    # TODO: V2 pools can updated via polling, or via external updates. Need to implement a
                    # more robust check that gracefully handles externally-updated pools
                    if pool.update_reserves(silent=silent):
                        found_updates = True
                elif pool.uniswap_version == 3:
                    pool_updated, _ = pool.auto_update(silent=silent)
                    if pool_updated:
                        found_updates = True
            elif pool._update_method == "external":
                if pool.state != self.pool_states[pool.address]:
                    found_updates = True
            else:
                raise DegenbotError(
                    "auto_update: could not determine update method!"
                )

        if found_updates:
            self._update_pool_states
        return found_updates

    def calculate_arbitrage(self) -> Tuple[bool, Tuple[int, int]]:

        if self.best["init"] == True:
            # if the 'Init' flag is True, the calc has never been done and should be,
            # regardless of state
            pass
        elif self.pool_states == {
            pool.address: pool.state for pool in self.swap_pools
        }:
            # short-circuit to avoid arb recalc if pool states have not changed:
            return False, (0, 0)
        else:
            self._update_pool_states()

        # cap the amount to be swapped
        bounds = (1, self.max_input)

        if self.input_token == self.swap_pools[0].token0:
            forward_token = self.swap_pools[0].token1
        elif self.input_token == self.swap_pools[0].token1:
            forward_token = self.swap_pools[0].token0
        else:
            print("calculate_arbitrage: WTF? Could not identify input token")
            raise ArbCalculationError

        def arb_profit(x):
            x = ceil(x)  # round up on the input
            return -float(
                floor(  # round down on the output
                    self.swap_pools[1].calculate_tokens_out_from_tokens_in(
                        token_in=forward_token,
                        token_in_quantity=self.swap_pools[
                            0
                        ].calculate_tokens_out_from_tokens_in(
                            token_in=self.input_token,
                            token_in_quantity=x,
                        ),
                    )
                    - x
                )
            )

        try:
            opt = optimize.minimize_scalar(
                arb_profit,
                method="bounded",
                bounds=bounds,
                options={"xatol": 1.0},
            )
        except Exception as e:
            print(e)
            print(f"bounds: {bounds}")
            raise ArbCalculationError
        else:
            swap_amount = int(opt.x)
            best_profit = -int(opt.fun)

        profitable = True if best_profit > 0 else False
        self.best.update(
            {
                "swap_amount": swap_amount,
                "profit_amount": best_profit,
                "swap_pool_amounts": self._build_multipool_amounts_out(
                    token_in=self.input_token,
                    token_in_quantity=swap_amount,
                ),
            }
        )

        return profitable, (swap_amount, best_profit)

    def calculate_multipool_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
    ) -> int:
        """
        Calculates the expected token OUTPUT from the last pool for a given token INPUT to the first pool
        at current pool states. Uses the self.token0 and self.token1 pointers to determine which token
        is being swapped in and uses the appropriate formula
        """

        number_of_pools = len(self.swap_pools)

        for i in range(number_of_pools):

            # determine the output token for this pool
            if token_in == self.swap_pools[i].token0:
                token_out = self.swap_pools[i].token1
            elif token_in == self.swap_pools[i].token1:
                token_out = self.swap_pools[i].token0
            else:
                raise ArbCalculationError(
                    f"Could not identify token_in! Found {token_in}, pool holds {self.swap_pools[i].token0}, {self.swap_pools[i].token1} "
                )

            # calculate the swap output through pool[i]
            token_out_quantity = self.swap_pools[
                i
            ].calculate_tokens_out_from_tokens_in(
                token_in=token_in, token_in_quantity=token_in_quantity
            )

            if i == number_of_pools - 1:
                # if we've reached the last pool, return the output amount
                return token_out_quantity
            else:
                # otherwise, use the output as input on the next loop
                token_in = token_out
                token_in_quantity = token_out_quantity

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

        # web3py object without a provider, useful for offline transaction creation and signing
        w3 = Web3()

        payloads = []

        # generate the payload for the initial transfer if the first pool is type V2
        if self.swap_pools[0].uniswap_version == 2:
            print("\tPAYLOAD: building initial V2 transfer")
            try:
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
            except Exception as e:
                print(f"generate_payloads (transfer_payload): {e}")
                print(self.best)
                print(f"id: {self.id}")
            else:
                payloads.append(transfer_payload)

        # generate the swap payloads for each pool in the path
        try:
            last_pool = self.swap_pools[-1]
            print("\tPAYLOAD: identified last pool")
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
                    print(f"\tPAYLOAD: building V2 swap at pool {i}")
                    print(
                        f"\tPAYLOAD: pool address {swap_pool_object.address}"
                    )
                    print(
                        f'\tPAYLOAD: swap amounts {self.best["swap_pool_amounts"][i]["amounts"]}'
                    )
                    print(
                        f"\tPAYLOAD: destination address {swap_destination_address}"
                    )
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
                    print(f"\tPAYLOAD: building V3 swap at pool {i}")
                    print(
                        f"\tPAYLOAD: pool address {swap_pool_object.address}"
                    )
                    print(
                        f'\tPAYLOAD: swap amounts {self.best["swap_pool_amounts"][i]}'
                    )
                    print(
                        f"\tPAYLOAD: destination address {swap_destination_address}"
                    )
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
                    raise DegenbotError(
                        f"Could not determine Uniswap version for pool: {swap_pool_object}"
                    )
        except Exception as e:
            print(f"generate_payloads (swap_payload): {e}")
            print(self.best)
            print(f"id: {self.id}")

        return payloads

    def _build_multipool_amounts_out(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
    ) -> List[dict]:

        number_of_pools = len(self.swap_pools)

        pools_amounts_out = []

        for i in range(number_of_pools):

            # determine the uniswap version for the pool and format the output appropriately
            if self.swap_pools[i].uniswap_version == 2:
                # determine the output token for the pool
                if token_in == self.swap_pools[i].token0:
                    token_out = self.swap_pools[i].token1
                elif token_in == self.swap_pools[i].token1:
                    token_out = self.swap_pools[i].token0
                else:
                    raise InvalidSwapPathError(
                        f"Could not identify token_in! Found {token_in}, pool holds {self.swap_pools[i].token0}, {self.swap_pools[i].token1} "
                    )

                # calculate the swap output through pool[i]
                token_out_quantity = self.swap_pools[
                    i
                ].calculate_tokens_out_from_tokens_in(
                    token_in=token_in,
                    token_in_quantity=token_in_quantity,
                )

                if token_in == self.swap_pools[i].token0:
                    pools_amounts_out.append(
                        {
                            "uniswap_version": 2,
                            "amounts": [0, token_out_quantity],
                        }
                    )
                elif token_in == self.swap_pools[i].token1:
                    pools_amounts_out.append(
                        {
                            "uniswap_version": 2,
                            "amounts": [token_out_quantity, 0],
                        }
                    )
            elif self.swap_pools[i].uniswap_version == 3:
                # determine the output token for the pool
                if token_in == self.swap_pools[i].token0:
                    token_out = self.swap_pools[i].token1
                elif token_in == self.swap_pools[i].token1:
                    token_out = self.swap_pools[i].token0
                else:
                    raise InvalidSwapPathError(
                        f"Could not identify token_in! Found {token_in}, pool holds {self.swap_pools[i].token0}, {self.swap_pools[i].token1} "
                    )

                # calculate the swap output through pool[i]
                token_out_quantity = self.swap_pools[
                    i
                ].calculate_tokens_out_from_tokens_in(
                    token_in=token_in,
                    token_in_quantity=token_in_quantity,
                )

                if token_in == self.swap_pools[i].token0:
                    _zeroForOne = True
                    pools_amounts_out.append(
                        {
                            "uniswap_version": 3,
                            "amountSpecified": token_in_quantity,  # for an exactInput swap, always a positive number representing the input amount
                            "zeroForOne": _zeroForOne,
                            "sqrtPriceLimitX96": TickMath.MIN_SQRT_RATIO + 1
                            if _zeroForOne
                            else TickMath.MAX_SQRT_RATIO - 1,
                        }
                    )
                elif token_in == self.swap_pools[i].token1:
                    _zeroForOne = False
                    pools_amounts_out.append(
                        {
                            "uniswap_version": 3,
                            "amountSpecified": token_in_quantity,  # for an exactInput swap, always a positive number representing the input amount
                            "zeroForOne": _zeroForOne,
                            "sqrtPriceLimitX96": TickMath.MIN_SQRT_RATIO + 1
                            if _zeroForOne
                            else TickMath.MAX_SQRT_RATIO - 1,
                        }
                    )

            else:
                raise DegenbotError(
                    f"Could not identify Uniswap version for pool: {self.swap_pools[i]}"
                )

            if i == number_of_pools - 1:
                # if we've reached the last pool, return the pool_amounts_out list
                return pools_amounts_out
            else:
                # otherwise, feed the results back into the loop
                token_in = token_out
                token_in_quantity = token_out_quantity
