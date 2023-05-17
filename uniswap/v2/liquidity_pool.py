from decimal import Decimal
from fractions import Fraction
from typing import List, Optional, Tuple, Union

from brownie import Contract, Wei, chain  # type: ignore
from eth_typing import ChecksumAddress
from web3 import Web3

from degenbot.exceptions import (
    DeprecationError,
    ExternalUpdateError,
    LiquidityPoolError,
)
from degenbot.manager.token_manager import Erc20TokenHelperManager
from degenbot.token import Erc20Token
from degenbot.uniswap.v2.abi import UNISWAPV2_LP_ABI
from degenbot.uniswap.v2.router import Router


class LiquidityPool:
    def __init__(
        self,
        address: str,
        tokens: Optional[List[Erc20Token]] = None,
        name: Optional[str] = None,
        update_method: str = "polling",
        router: Optional[Router] = None,
        abi: Optional[list] = None,
        # default fee for most UniswapV2 AMMs is 0.3%
        fee: Fraction = Fraction(3, 1000),
        fee_token0: Optional[Fraction] = None,
        fee_token1: Optional[Fraction] = None,
        silent: bool = False,
        update_reserves_on_start: bool = True,
        unload_brownie_contract_after_init: bool = False,
    ) -> None:
        """
        Create a new `LiquidityPool` object for interaction with a Uniswap V2 pool.

        Arguments
        ---------
        address : str
            Address for the deployed pool contract.
        tokens : List[Erc20Token], optional
            Erc20Token objects for the tokens held by the deployed pool.
        name : str, optional
            Name of the contract, e.g. "DAI-WETH".
        update_method : str
            A string that sets the method used to fetch updates to the pool. Can be "polling", which fetches updates from the chain object using the contract object, or "external" which relies on updates being provided from outside the object.
        router : Router, optional
            A reference to a Router object, which can be used to execute swaps using the attributes held within this object.
        abi : list
            Contract ABI.
        fee : Fraction
            The swap fee imposed by the pool. Defaults to `Fraction(3,1000)` which is equivalent to 0.3%.
        fee_token0 : Fraction, optional
            Swap fee for token0. Same purpose as `fee` except useful for pools with different fees for each token.
        fee_token1 : Fraction, optional
            Swap fee for token1. Same purpose as `fee` except useful for pools with different fees for each token.
        silent : bool
            Suppress status output.
        update_reserves_on_start : bool
            Update the reserves during instantiation.
        unload_brownie_contract_after_init : bool
            Remove the Brownie contract helper before completion. Saves memory for objects that are externally-updated, and do not need to perform calls to the chain after creation.
        """
        self.uniswap_version = 2

        self.address: Union[str, ChecksumAddress]

        # transforms to checksummed address
        try:
            self.address = Web3.toChecksumAddress(address)
        except ValueError:
            print(
                "Could not checksum address, storing non-checksummed version"
            )
            self.address = address

        if router:
            self.router = router

        if type(fee) == Decimal:
            print("***")
            print(
                f"WARNING: fee set as a Decimal value instead of Fraction. The fee has been converted inside the LP helper from {repr(fee)} to {repr(Fraction(fee))}, please adjust your code to specify a Fraction to remove this warning. e.g. Fraction(3,1000) is equivalent to Decimal('0.003')."
            )
            print("***")
            fee = Fraction(fee)

        if type(fee) != Fraction:
            raise TypeError(
                f"LP fee was not correctly passed! "
                f"Expected '{Fraction().__class__.__name__}', "
                f"was '{fee.__class__.__name__}'"
            )

        self.fee = fee
        self.fee_token0 = fee_token0 if fee_token0 is not None else fee
        self.fee_token1 = fee_token1 if fee_token1 is not None else fee
        self._update_method = update_method
        self._ratio_token0_in: Optional[Decimal] = None
        self._ratio_token1_in: Optional[Decimal] = None
        self.new_reserves = False
        self.update_block = chain.height

        if abi is None:
            abi = UNISWAPV2_LP_ABI

        # try:
        #     self._contract = Contract(self.address)
        # except:
        # try:
        self._contract = Contract.from_abi(
            name=f"{self.address}",
            abi=abi,
            address=self.address,
            persist=False,
        )
        self.abi = abi
        # else:
        #     self._contract = Contract.from_explorer(address=self.address)
        # else:
        #     self.abi = self._contract.abi

        self.factory = Web3.toChecksumAddress(self._contract.factory())

        # if a token pair was provided, check and set pointers for token0 and token1
        if tokens is not None:
            if len(tokens) != 2:
                raise ValueError(f"Expected 2 tokens, found {len(tokens)}")
            for token in tokens:
                if token.address == self._contract.token0():
                    self.token0 = token
                elif token.address == self._contract.token1():
                    self.token1 = token
                else:
                    raise ValueError(f"{token} not found in pool {self}")
        else:
            _token_manager = Erc20TokenHelperManager(chain.id)
            self.token0 = _token_manager.get_erc20token(
                address=self._contract.token0(),
                min_abi=True,
                silent=silent,
                unload_brownie_contract_after_init=True,
            )
            self.token1 = _token_manager.get_erc20token(
                address=self._contract.token1(),
                min_abi=True,
                silent=silent,
                unload_brownie_contract_after_init=True,
            )

        if name is not None:
            self.name = name
        else:
            if (
                self.fee_token0 is not None and self.fee_token1 is not None
            ) and (self.fee_token0 != self.fee_token1):
                fee_string = f"{100*self.fee_token0.numerator/self.fee_token0.denominator:.2f}/{100*self.fee_token1.numerator/self.fee_token1.denominator:.2f}"
            elif (
                self.fee_token0 is not None and self.fee_token1 is not None
            ) and (self.fee_token0 == self.fee_token1):
                fee_string = f"{100*self.fee_token0.numerator/self.fee_token0.denominator:.2f}"
            else:
                fee_string = (
                    f"{100*self.fee.numerator/self.fee.denominator:.2f}"
                )
            self.name = f"{self.token0}-{self.token1} (V2, {fee_string}%)"

        if update_reserves_on_start:
            (
                self.reserves_token0,
                self.reserves_token1,
                *_,
            ) = self._contract.getReserves(block_identifier=self.update_block)[
                0:2
            ]
        else:
            self.reserves_token0 = self.reserves_token1 = 0

        if self._update_method == "event":
            raise ValueError(
                "The 'event' update method is inaccurate and unsupported, please update your bot to use the default 'polling' method"
            )

        if (
            self._update_method == "external"
            and unload_brownie_contract_after_init
        ):
            # memory saving if LP contract object is not used after initialization
            self._contract = None

        self.state: dict = {}
        self._update_pool_state()

        if not silent:
            print(self.name)
            print(
                f"• Token 0: {self.token0} - Reserves: {self.reserves_token0}"
            )
            print(
                f"• Token 1: {self.token1} - Reserves: {self.reserves_token1}"
            )

    # The Brownie contract object cannot be pickled, so remove it and return the state
    def __getstate__(self):
        state = self.__dict__.copy()
        state["_contract"] = None
        return state

    def __setstate__(self, state):
        self.__dict__.update(state)

    def __eq__(self, other) -> bool:
        return self.address == other.address

    def __str__(self):
        """
        Return the pool name when the object is included in a print statement, or cast as a string
        """
        return self.name

    def _update_pool_state(self):
        self.state = {
            "reserves_token0": self.reserves_token0,
            "reserves_token1": self.reserves_token1,
        }

    def calculate_tokens_in_from_ratio_out(self) -> None:
        """
        Calculates the maximum token inputs for the target output ratios at current pool reserves
        """

        self.token0_max_swap: int
        self.token1_max_swap: int

        # token0 in, token1 out
        # formula: dx = y0*C - x0/(1-FEE), where C = token0/token1
        if self._ratio_token0_in:
            self.token0_max_swap = max(
                0,
                int(self.reserves_token1 * self._ratio_token0_in)
                - int(self.reserves_token0 / (1 - self.fee_token0)),
            )
        else:
            self.token0_max_swap = 0

        # token1 in, token0 out
        # formula: dy = x0*C - y0(1/FEE), where C = token1/token0
        if self._ratio_token1_in:
            self.token1_max_swap = max(
                0,
                int(self.reserves_token0 * self._ratio_token1_in)
                - int(self.reserves_token1 / (1 - self.fee_token1)),
            )
        else:
            self.token1_max_swap = 0

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out_quantity: int,
        token_in: Optional[Erc20Token] = None,
        token_out: Optional[Erc20Token] = None,
        override_reserves_token0: Optional[int] = None,
        override_reserves_token1: Optional[int] = None,
        override_state: Optional[dict] = None,
    ) -> int:
        """
        Calculates the required token INPUT of token_in for a target OUTPUT at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        """

        # TODO: check for conflicting overrides
        if override_state and (
            override_reserves_token0 is None
            and override_reserves_token1 is None
        ):
            override_reserves_token0 = override_state["reserves_token0"]
            override_reserves_token1 = override_state["reserves_token1"]

            print("Overrides applied:")
            print(f"{override_reserves_token0=}")
            print(f"{override_reserves_token1=}")

        if not (
            (
                override_reserves_token0 is not None
                and override_reserves_token1 is not None
            )
            or (
                override_reserves_token0 is None
                and override_reserves_token1 is None
            )
        ):
            raise ValueError(
                "Must provide override values for both token reserves"
            )

        if token_in is not None:
            if token_in not in [self.token0, self.token1]:
                raise ValueError(
                    f"Could not identify token_in: {token_in}! This pool holds: {self.token0} {self.token1}"
                )
            if token_in == self.token0:
                reserves_in = (
                    override_reserves_token0
                    if override_reserves_token0 is not None
                    else self.reserves_token0
                )
                fee = self.fee_token0
                reserves_out = (
                    override_reserves_token1
                    if override_reserves_token1 is not None
                    else self.reserves_token1
                )
            elif token_in == self.token1:
                reserves_in = (
                    override_reserves_token1
                    if override_reserves_token1 is not None
                    else self.reserves_token1
                )
                reserves_out = (
                    override_reserves_token0
                    if override_reserves_token0 is not None
                    else self.reserves_token0
                )
                fee = self.fee_token1
            else:
                raise ValueError("wtf happened here? (token_in)")
        elif token_out is not None:
            if token_out not in [self.token0, self.token1]:
                raise ValueError(
                    f"Could not identify token_out: {token_out}! This pool holds: {self.token0} {self.token1}"
                )
            if token_out == self.token1:
                reserves_in = (
                    override_reserves_token0
                    if override_reserves_token0 is not None
                    else self.reserves_token0
                )
                reserves_out = (
                    override_reserves_token1
                    if override_reserves_token1 is not None
                    else self.reserves_token1
                )
                fee = self.fee_token0
            elif token_out == self.token0:
                reserves_in = (
                    override_reserves_token1
                    if override_reserves_token1 is not None
                    else self.reserves_token1
                )
                reserves_out = (
                    override_reserves_token0
                    if override_reserves_token0 is not None
                    else self.reserves_token0
                )
                fee = self.fee_token1
            else:
                raise ValueError("wtf happened here? (token_in)")

        # last token becomes infinitely expensive, so largest possible swap out is reserves - 1
        if token_out_quantity > reserves_out - 1:
            raise LiquidityPoolError(
                f"Requested amount out ({token_out_quantity}) >= pool reserves ({reserves_out})"
            )

        numerator = reserves_in * token_out_quantity * fee.denominator
        denominator = (reserves_out - token_out_quantity) * (
            fee.denominator - fee.numerator
        )
        return numerator // denominator + 1

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_reserves_token0: Optional[int] = None,
        override_reserves_token1: Optional[int] = None,
        override_state: Optional[dict] = None,
    ) -> int:
        """
        Calculates the expected token OUTPUT for a target INPUT at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        """

        # TODO: check for conflicting overrides
        if override_state and (
            override_reserves_token0 is None
            and override_reserves_token1 is None
        ):
            override_reserves_token0 = override_state["reserves_token0"]
            override_reserves_token1 = override_state["reserves_token1"]

            print("Overrides applied:")
            print(f"{override_reserves_token0=}")
            print(f"{override_reserves_token1=}")

        if not (
            (
                override_reserves_token0 is not None
                and override_reserves_token1 is not None
            )
            or (
                override_reserves_token0 is None
                and override_reserves_token1 is None
            )
        ):
            raise ValueError(
                "Must provide override values for both token reserves"
            )

        if token_in_quantity <= 0:
            raise ValueError("token_in_quantity must be positive")

        if token_in == self.token0:
            reserves_in = (
                override_reserves_token0
                if override_reserves_token0 is not None
                else self.reserves_token0
            )
            reserves_out = (
                override_reserves_token1
                if override_reserves_token1 is not None
                else self.reserves_token1
            )
            fee = self.fee_token0
        elif token_in == self.token1:
            reserves_in = (
                override_reserves_token1
                if override_reserves_token1 is not None
                else self.reserves_token1
            )
            reserves_out = (
                override_reserves_token0
                if override_reserves_token0 is not None
                else self.reserves_token0
            )
            fee = self.fee_token1
        else:
            raise ValueError(
                f"Could not identify token_in: {token_in}! Pool holds: {self.token0} {self.token1}"
            )

        # amount_in_with_fee = token_in_quantity * (
        #     self.fee.denominator - self.fee.numerator
        # )
        amount_in_with_fee = token_in_quantity * (
            fee.denominator - fee.numerator
        )
        numerator = amount_in_with_fee * reserves_out
        # denominator = reserves_in * self.fee.denominator + amount_in_with_fee
        denominator = reserves_in * fee.denominator + amount_in_with_fee
        return numerator // denominator

    def set_swap_target(
        self,
        token_in: Erc20Token,
        token_in_qty: Union[Wei, int],
        token_out: Erc20Token,
        token_out_qty: Union[Wei, int],
        silent: bool = False,
    ) -> None:
        # check to ensure that token_in and token_out are exactly the two tokens held by the LP
        if not (
            (token_in == self.token0 and token_out == self.token1)
            or (token_in == self.token1 and token_out == self.token0)
        ):
            raise ValueError(
                "Tokens must match the two tokens held by this pool!"
            )

        if not silent:
            print(
                f"{token_in} -> {token_out} @ ({token_in_qty} {token_in} = {token_out_qty} {token_out})"
            )

        if token_in == self.token0:
            # calculate the ratio of token0/token1 for swap of token0 -> token1
            self._ratio_token0_in = Decimal(
                (token_in_qty * 10**token_in.decimals)
            ) / Decimal(token_out_qty * 10**token_out.decimals)

        if token_in == self.token1:
            # calculate the ratio of token1/token0 for swap of token1 -> token0
            self._ratio_token1_in = Decimal(
                (token_in_qty * 10**token_in.decimals)
            ) / Decimal(token_out_qty * 10**token_out.decimals)

        self.calculate_tokens_in_from_ratio_out()

    def simulate_swap(
        self,
        token_in: Optional[Erc20Token] = None,
        token_in_quantity: Optional[int] = None,
        token_out: Optional[Erc20Token] = None,
        token_out_quantity: Optional[int] = None,
        override_state: Optional[dict] = None,
    ) -> dict:
        """
        TODO
        """

        if token_in_quantity is None and token_out_quantity is None:
            raise ValueError("No quantity was provided")

        if token_in_quantity is not None and token_out_quantity is not None:
            raise ValueError(
                "Provide token_in_quantity or token_out_quantity, not both"
            )

        if token_in and token_out and token_in == token_out:
            raise ValueError("Both tokens are the same!")

        if override_state is None:
            override_state = {}
        else:
            print(f"Overridden reserves: {override_state}")

        if token_in and token_in not in (self.token0, self.token1):
            raise ValueError(
                f"Token not found! token_in = {repr(token_in)}, pool holds {self.token0},{self.token1}"
            )
        if token_out and token_out not in (self.token0, self.token1):
            raise ValueError(
                f"Token not found! token_out = {repr(token_out)}, pool holds {self.token0},{self.token1}"
            )

        if token_in is not None and token_in == self.token0:
            token_out = self.token1
        elif token_in is not None and token_in == self.token1:
            token_out = self.token0

        if token_out is not None and token_out == self.token0:
            token_in = self.token1
        elif token_out is not None and token_out == self.token1:
            token_in = self.token0

        pool_state_after_swap = {}

        # bugfix: (changed check `token_in_quantity is not None`)
        # swaps with zero amounts (a stupid value, but valid) were falling through
        # both blocks and function was returning None
        if token_in_quantity is not None and token_in is not None:
            # delegate calculations to the `calculate_tokens_out_from_tokens_in` method
            token_out_quantity = self.calculate_tokens_out_from_tokens_in(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
                override_reserves_token0=override_state.get("reserves_token0"),
                override_reserves_token1=override_state.get("reserves_token1"),
            )

            token0_delta = (
                -token_out_quantity
                if token_in is self.token1
                else token_in_quantity
            )
            token1_delta = (
                -token_out_quantity
                if token_in is self.token0
                else token_in_quantity
            )

            pool_state_after_swap = {
                "amount0_delta": token0_delta,
                "amount1_delta": token1_delta,
                "reserves_token0": self.reserves_token0 + token0_delta,
                "reserves_token1": self.reserves_token1 + token1_delta,
            }

        # bugfix: (changed check `token_out_quantity is not None`)
        # swaps with zero amounts (a stupid value, but valid) were falling through
        # both blocks and function was returning None
        elif token_out_quantity is not None:
            # delegate calculations to the `calculate_tokens_in_from_tokens_out` method
            token_in_quantity = self.calculate_tokens_in_from_tokens_out(
                token_in=token_in,
                token_out=token_out,
                token_out_quantity=token_out_quantity,
                override_reserves_token0=override_state.get("reserves_token0"),
                override_reserves_token1=override_state.get("reserves_token1"),
            )

            token0_delta = (
                token_in_quantity
                if token_in == self.token0
                else -token_out_quantity
            )
            token1_delta = (
                token_in_quantity
                if token_in == self.token1
                else -token_out_quantity
            )

            pool_state_after_swap = {
                "amount0_delta": token0_delta,
                "amount1_delta": token1_delta,
                "reserves_token0": self.reserves_token0 + token0_delta,
                "reserves_token1": self.reserves_token1 + token1_delta,
            }

        return pool_state_after_swap

    def update_reserves(
        self,
        silent: bool = False,
        print_reserves: bool = True,
        print_ratios: bool = True,
        external_token0_reserves: Optional[int] = None,
        external_token1_reserves: Optional[int] = None,
        override_update_method: Optional[str] = None,
        update_block: Optional[int] = None,
    ) -> bool:
        """
        Checks for updated reserve values when set to "polling", otherwise
        if set to "external" assumes that provided reserves are valid
        """

        success = False

        # get the chain height from Brownie if a specific update_block is not provided
        if update_block is None:
            update_block = chain.height

        # discard stale updates, but allow updating the same pool multiple times per block (necessary if sending sync events individually)
        if update_block < self.update_block:
            raise ExternalUpdateError(
                f"Current state recorded at block {self.update_block}, received update for stale block {update_block}"
            )
        else:
            self.update_block = update_block

        if (
            self._update_method == "polling"
            or override_update_method == "polling"
        ):
            try:
                reserves0, reserves1, *_ = self._contract.getReserves(
                    block_identifier=self.update_block
                )
                # Compare reserves to last-known values,
                # store and (optionally) print the reserves if they have changed
                if (self.reserves_token0, self.reserves_token1) != (
                    reserves0,
                    reserves1,
                ):
                    self.reserves_token0, self.reserves_token1 = (
                        reserves0,
                        reserves1,
                    )
                    if not silent:
                        print(f"[{self.name}]")
                        if print_reserves:
                            print(f"{self.token0}: {self.reserves_token0}")
                            print(f"{self.token1}: {self.reserves_token1}")
                        if print_ratios:
                            print(
                                f"{self.token0}/{self.token1}: {(self.reserves_token0/10**self.token0.decimals) / (self.reserves_token1/10**self.token1.decimals)}"
                            )
                            print(
                                f"{self.token1}/{self.token0}: {(self.reserves_token1/10**self.token1.decimals) / (self.reserves_token0/10**self.token0.decimals)}"
                            )

                    # recalculate possible swaps using the new reserves
                    self.calculate_tokens_in_from_ratio_out()
                    self._update_pool_state()
                    success = True
                else:
                    success = False
            except Exception as e:
                print(
                    f"LiquidityPool: Exception in update_reserves (polling): {e}"
                )
        elif self._update_method == "external":
            if not (
                (
                    external_token0_reserves is not None
                    and external_token1_reserves is not None
                )
            ):
                raise ValueError(
                    "Called update_reserves without providing reserve values for both tokens!"
                )

            # skip follow-up processing if the LP object already has the latest reserves, or if no reserves were provided
            if (
                external_token0_reserves == self.reserves_token0
                and external_token1_reserves == self.reserves_token1
            ):
                self.new_reserves = False
                success = False
            else:
                self.reserves_token0 = external_token0_reserves
                self.reserves_token1 = external_token1_reserves
                self.new_reserves = True
                self._update_pool_state()

            if not silent:
                print(f"[{self.name}]")
                if print_reserves:
                    print(f"{self.token0}: {self.reserves_token0}")
                    print(f"{self.token1}: {self.reserves_token1}")
                if print_ratios:
                    print(
                        f"{self.token0}/{self.token1}: {self.reserves_token0 / self.reserves_token1}"
                    )
                    print(
                        f"{self.token1}/{self.token0}: {self.reserves_token1 / self.reserves_token0}"
                    )
            self.calculate_tokens_in_from_ratio_out()
            success = True
        elif self._update_method == "event":
            raise DeprecationError(
                "***"
                "DEPRECATION WARNING: the 'event' update method is deprecated. Please update your bot to use the default 'polling' method"
                "***"
            )
        else:
            success = False

        return success


class CamelotLiquidityPool(LiquidityPool):
    def __init__(
        self,
        address: str,
        tokens: Optional[List[Erc20Token]] = None,
        name: Optional[str] = None,
        update_method: str = "polling",
        router: Optional[Router] = None,
        abi: Optional[list] = None,
        # default fee for most UniswapV2 AMMs is 0.3%
        fee: Fraction = Fraction(3, 1000),
        silent: bool = False,
        update_reserves_on_start: bool = True,
        unload_brownie_contract_after_init: bool = False,
    ) -> None:
        if abi is None:
            import json

            abi = json.loads(
                """
                [{"inputs":[],"payable":false,"stateMutability":"nonpayable","type":"constructor"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"owner","type":"address"},{"indexed":true,"internalType":"address","name":"spender","type":"address"},{"indexed":false,"internalType":"uint256","name":"value","type":"uint256"}],"name":"Approval","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"sender","type":"address"},{"indexed":false,"internalType":"uint256","name":"amount0","type":"uint256"},{"indexed":false,"internalType":"uint256","name":"amount1","type":"uint256"},{"indexed":true,"internalType":"address","name":"to","type":"address"}],"name":"Burn","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"token","type":"address"},{"indexed":false,"internalType":"address","name":"to","type":"address"}],"name":"DrainWrongToken","type":"event"},{"anonymous":false,"inputs":[{"indexed":false,"internalType":"uint16","name":"token0FeePercent","type":"uint16"},{"indexed":false,"internalType":"uint16","name":"token1FeePercent","type":"uint16"}],"name":"FeePercentUpdated","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"sender","type":"address"},{"indexed":false,"internalType":"uint256","name":"amount0","type":"uint256"},{"indexed":false,"internalType":"uint256","name":"amount1","type":"uint256"}],"name":"Mint","type":"event"},{"anonymous":false,"inputs":[],"name":"SetPairTypeImmutable","type":"event"},{"anonymous":false,"inputs":[{"indexed":false,"internalType":"bool","name":"prevStableSwap","type":"bool"},{"indexed":false,"internalType":"bool","name":"stableSwap","type":"bool"}],"name":"SetStableSwap","type":"event"},{"anonymous":false,"inputs":[],"name":"Skim","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"sender","type":"address"},{"indexed":false,"internalType":"uint256","name":"amount0In","type":"uint256"},{"indexed":false,"internalType":"uint256","name":"amount1In","type":"uint256"},{"indexed":false,"internalType":"uint256","name":"amount0Out","type":"uint256"},{"indexed":false,"internalType":"uint256","name":"amount1Out","type":"uint256"},{"indexed":true,"internalType":"address","name":"to","type":"address"}],"name":"Swap","type":"event"},{"anonymous":false,"inputs":[{"indexed":false,"internalType":"uint112","name":"reserve0","type":"uint112"},{"indexed":false,"internalType":"uint112","name":"reserve1","type":"uint112"}],"name":"Sync","type":"event"},{"anonymous":false,"inputs":[{"indexed":true,"internalType":"address","name":"from","type":"address"},{"indexed":true,"internalType":"address","name":"to","type":"address"},{"indexed":false,"internalType":"uint256","name":"value","type":"uint256"}],"name":"Transfer","type":"event"},{"constant":true,"inputs":[],"name":"DOMAIN_SEPARATOR","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[],"name":"FEE_DENOMINATOR","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[],"name":"MAX_FEE_PERCENT","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[],"name":"MINIMUM_LIQUIDITY","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[],"name":"PERMIT_TYPEHASH","outputs":[{"internalType":"bytes32","name":"","type":"bytes32"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[{"internalType":"address","name":"","type":"address"},{"internalType":"address","name":"","type":"address"}],"name":"allowance","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"internalType":"address","name":"spender","type":"address"},{"internalType":"uint256","name":"value","type":"uint256"}],"name":"approve","outputs":[{"internalType":"bool","name":"","type":"bool"}],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[{"internalType":"address","name":"","type":"address"}],"name":"balanceOf","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"internalType":"address","name":"to","type":"address"}],"name":"burn","outputs":[{"internalType":"uint256","name":"amount0","type":"uint256"},{"internalType":"uint256","name":"amount1","type":"uint256"}],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[],"name":"decimals","outputs":[{"internalType":"uint8","name":"","type":"uint8"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"internalType":"address","name":"token","type":"address"},{"internalType":"address","name":"to","type":"address"}],"name":"drainWrongToken","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[],"name":"factory","outputs":[{"internalType":"address","name":"","type":"address"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[{"internalType":"uint256","name":"amountIn","type":"uint256"},{"internalType":"address","name":"tokenIn","type":"address"}],"name":"getAmountOut","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[],"name":"getReserves","outputs":[{"internalType":"uint112","name":"_reserve0","type":"uint112"},{"internalType":"uint112","name":"_reserve1","type":"uint112"},{"internalType":"uint16","name":"_token0FeePercent","type":"uint16"},{"internalType":"uint16","name":"_token1FeePercent","type":"uint16"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"internalType":"address","name":"_token0","type":"address"},{"internalType":"address","name":"_token1","type":"address"}],"name":"initialize","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[],"name":"initialized","outputs":[{"internalType":"bool","name":"","type":"bool"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[],"name":"kLast","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"internalType":"address","name":"to","type":"address"}],"name":"mint","outputs":[{"internalType":"uint256","name":"liquidity","type":"uint256"}],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[],"name":"name","outputs":[{"internalType":"string","name":"","type":"string"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[{"internalType":"address","name":"","type":"address"}],"name":"nonces","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[],"name":"pairTypeImmutable","outputs":[{"internalType":"bool","name":"","type":"bool"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"internalType":"address","name":"owner","type":"address"},{"internalType":"address","name":"spender","type":"address"},{"internalType":"uint256","name":"value","type":"uint256"},{"internalType":"uint256","name":"deadline","type":"uint256"},{"internalType":"uint8","name":"v","type":"uint8"},{"internalType":"bytes32","name":"r","type":"bytes32"},{"internalType":"bytes32","name":"s","type":"bytes32"}],"name":"permit","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[],"name":"precisionMultiplier0","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[],"name":"precisionMultiplier1","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"internalType":"uint16","name":"newToken0FeePercent","type":"uint16"},{"internalType":"uint16","name":"newToken1FeePercent","type":"uint16"}],"name":"setFeePercent","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":false,"inputs":[],"name":"setPairTypeImmutable","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":false,"inputs":[{"internalType":"bool","name":"stable","type":"bool"},{"internalType":"uint112","name":"expectedReserve0","type":"uint112"},{"internalType":"uint112","name":"expectedReserve1","type":"uint112"}],"name":"setStableSwap","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":false,"inputs":[{"internalType":"address","name":"to","type":"address"}],"name":"skim","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[],"name":"stableSwap","outputs":[{"internalType":"bool","name":"","type":"bool"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"internalType":"uint256","name":"amount0Out","type":"uint256"},{"internalType":"uint256","name":"amount1Out","type":"uint256"},{"internalType":"address","name":"to","type":"address"},{"internalType":"bytes","name":"data","type":"bytes"}],"name":"swap","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":false,"inputs":[{"internalType":"uint256","name":"amount0Out","type":"uint256"},{"internalType":"uint256","name":"amount1Out","type":"uint256"},{"internalType":"address","name":"to","type":"address"},{"internalType":"bytes","name":"data","type":"bytes"},{"internalType":"address","name":"referrer","type":"address"}],"name":"swap","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[],"name":"symbol","outputs":[{"internalType":"string","name":"","type":"string"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[],"name":"sync","outputs":[],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":true,"inputs":[],"name":"token0","outputs":[{"internalType":"address","name":"","type":"address"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[],"name":"token0FeePercent","outputs":[{"internalType":"uint16","name":"","type":"uint16"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[],"name":"token1","outputs":[{"internalType":"address","name":"","type":"address"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[],"name":"token1FeePercent","outputs":[{"internalType":"uint16","name":"","type":"uint16"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":true,"inputs":[],"name":"totalSupply","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"payable":false,"stateMutability":"view","type":"function"},{"constant":false,"inputs":[{"internalType":"address","name":"to","type":"address"},{"internalType":"uint256","name":"value","type":"uint256"}],"name":"transfer","outputs":[{"internalType":"bool","name":"","type":"bool"}],"payable":false,"stateMutability":"nonpayable","type":"function"},{"constant":false,"inputs":[{"internalType":"address","name":"from","type":"address"},{"internalType":"address","name":"to","type":"address"},{"internalType":"uint256","name":"value","type":"uint256"}],"name":"transferFrom","outputs":[{"internalType":"bool","name":"","type":"bool"}],"payable":false,"stateMutability":"nonpayable","type":"function"}]
                """
            )

        _contract = Contract.from_abi(
            name=f"{address}",
            abi=abi,
            address=address,
            persist=False,
        )
        abi = abi

        _, _, fee_token0, fee_token1 = _contract.getReserves()
        fee_denominator = _contract.FEE_DENOMINATOR()
        fee_token0 = Fraction(fee_token0, fee_denominator)
        fee_token1 = Fraction(fee_token1, fee_denominator)

        super().__init__(
            address=address,
            abi=abi,
            fee_token0=fee_token0,
            fee_token1=fee_token1,
            silent=silent,
            update_method=update_method,
        )
