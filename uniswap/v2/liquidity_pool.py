import time
from decimal import Decimal
from fractions import Fraction
from typing import List, Optional, Union, Tuple

from brownie import Contract, Wei, chain
from brownie.convert import to_address

from degenbot.exceptions import (
    DeprecationError,
    ExternalUpdateError,
    LiquidityPoolError,
)
from degenbot.token import Erc20Token

from .abi import UNISWAPV2_LP_ABI
from .router import Router


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
        silent: bool = False,
        update_reserves_on_start: bool = True,
        unload_brownie_contract_after_init: bool = False,
    ) -> None:

        self.uniswap_version = 2

        # transforms to checksummed address
        try:
            self.address = to_address(address)
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

        # assert (
        #     type(fee) == Fraction
        # ), f"LP fee was not correctly passed! Expected '{Fraction().__class__.__name__}', was '{fee.__class__.__name__}'"

        self.fee = fee
        self._update_method = update_method
        self._ratio_token0_in = None
        self._ratio_token1_in = None
        self.new_reserves = None
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
        )
        self.abi = abi
        # else:
        #     self._contract = Contract.from_explorer(address=self.address)
        # else:
        #     self.abi = self._contract.abi

        self.factory = to_address(self._contract.factory())

        # if a token pair was provided, check and set pointers for token0 and token1
        if tokens is not None:
            if len(tokens) != 2:
                raise ValueError(f"Expected 2 tokens, found {len(tokens)}")
            # assert len(tokens) == 2, f"Expected 2 tokens, found {len(tokens)}"
            for token in tokens:
                if token.address == self._contract.token0():
                    self.token0 = token
                elif token.address == self._contract.token1():
                    self.token1 = token
        else:
            self.token0 = Erc20Token(address=self._contract.token0())
            self.token1 = Erc20Token(address=self._contract.token1())

        if name is not None:
            self.name = name
        else:
            self.name = f"{self.token0.symbol}-{self.token1.symbol} (V2, {100*self.fee.numerator/self.fee.denominator:.2f}%)"

        if update_reserves_on_start:
            (
                self.reserves_token0,
                self.reserves_token1,
            ) = self._contract.getReserves(block_identifier=self.update_block)[
                0:2
            ]
        else:
            self.reserves_token0 = self.reserves_token1 = 0

        if self._update_method == "event":
            print("***")
            print(
                "DEPRECATION WARNING: the 'event' update method is inaccurate, please update your bot to use the default 'polling' method"
            )
            print("***")
            raise Exception

        if (
            self._update_method == "external"
            and unload_brownie_contract_after_init
        ):
            # huge memory savings if LP contract object is not used after initialization
            self._contract = None

        self.state = {}
        self._update_pool_state()

        if not silent:
            print(self.name)
            print(
                f"• Token 0: {self.token0.symbol} - Reserves: {self.reserves_token0}"
            )
            print(
                f"• Token 1: {self.token1.symbol} - Reserves: {self.reserves_token1}"
            )

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

    def calculate_tokens_in_from_ratio_out(self) -> int:
        """
        Calculates the maximum token inputs for the target output ratios at current pool reserves
        """

        # token0 in, token1 out
        # formula: dx = y0*C - x0/(1-FEE), where C = token0/token1
        if self._ratio_token0_in:
            self.token0_max_swap = max(
                0,
                int(self.reserves_token1 * self._ratio_token0_in)
                - int(self.reserves_token0 / (1 - self.fee)),
            )
        else:
            self.token0_max_swap = 0

        # token1 in, token0 out
        # formula: dy = x0*C - y0(1/FEE), where C = token1/token0
        if self._ratio_token1_in:
            self.token1_max_swap = max(
                0,
                int(self.reserves_token0 * self._ratio_token1_in)
                - int(self.reserves_token1 / (1 - self.fee)),
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
    ) -> int:
        """
        Calculates the required token INPUT of token_in for a target OUTPUT at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        """

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
                reserves_out = (
                    override_reserves_token1
                    if override_reserves_token1 is not None
                    else self.reserves_token1
                )
            elif token_in is not None and token_in == self.token1:
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
            elif token_out is not None and token_out == self.token0:
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
            else:
                raise ValueError("wtf happened here? (token_in)")

        # last token becomes infinitely expensive, so largest possible swap out is reserves - 1
        if token_out_quantity >= reserves_out - 1:
            raise LiquidityPoolError(
                f"Requested amount out exceeds pool reserves"
            )

        numerator = reserves_in * token_out_quantity * self.fee.denominator
        denominator = (reserves_out - token_out_quantity) * (
            self.fee.denominator - self.fee.numerator
        )
        return numerator // denominator + 1

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_reserves_token0: Optional[int] = None,
        override_reserves_token1: Optional[int] = None,
    ) -> int:
        """
        Calculates the expected token OUTPUT for a target INPUT at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        """

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

        # assert (
        #     override_reserves_token0 == 0 and override_reserves_token1 == 0
        # ) or (
        #     override_reserves_token0 != 0 and override_reserves_token1 != 0
        # ), "Must provide override values for both token reserves"

        if token_in_quantity < 0:
            raise ValueError("token_in_quantity cannot be negative")

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
        else:
            raise ValueError(
                f"Could not identify token_in: {token_in}! Pool holds: {self.token0} {self.token1}"
            )

        amount_in_with_fee = token_in_quantity * (
            self.fee.denominator - self.fee.numerator
        )
        numerator = amount_in_with_fee * reserves_out
        denominator = reserves_in * self.fee.denominator + amount_in_with_fee
        return numerator // denominator

    def set_swap_target(
        self,
        token_in: Erc20Token,
        token_in_qty: Union[Wei, int],
        token_out: Erc20Token,
        token_out_qty: Union[Wei, int],
        silent: bool = False,
    ):
        # check to ensure that token_in and token_out are exactly the two tokens held by the LP
        if not (
            (
                token_in.address == self.token0.address
                and token_out.address == self.token1.address
            )
            or (
                token_in.address == self.token1.address
                and token_out.address == self.token0.address
            )
        ):
            raise ValueError(
                "Tokens must match the two tokens held by this pool!"
            )
        # assert (
        #     token_in.address == self.token0.address
        #     and token_out.address == self.token1.address
        # ) or (
        #     token_in.address == self.token1.address
        #     and token_out.address == self.token0.address
        # ), "Tokens must match the two tokens held by this pool!"

        if not silent:
            print(
                f"{token_in} -> {token_out} @ ({token_in_qty} {token_in} = {token_out_qty} {token_out})"
            )

        if token_in.address == self.token0.address:
            # calculate the ratio of token0/token1 for swap of token0 -> token1
            self._ratio_token0_in = Decimal(
                (token_in_qty * 10**token_in.decimals)
            ) / Decimal(token_out_qty * 10**token_out.decimals)

        if token_in.address == self.token1.address:
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
        override_state: dict = None,
    ) -> Tuple[dict]:
        """
        [TBD]
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

        if token_in_quantity:
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

            return {
                "reserves_token0": self.reserves_token0 + token0_delta,
                "reserves_token1": self.reserves_token1 + token1_delta,
            }
        elif token_out_quantity:
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

            print(f"{token_in_quantity=}")

            return {
                "amount0_delta": token0_delta,
                "amount1_delta": token1_delta,
            }, {
                "reserves_token0": self.reserves_token0 + token0_delta,
                "reserves_token1": self.reserves_token1 + token1_delta,
            }

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
                reserves0, reserves1, _ = self._contract.getReserves(
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
                            print(
                                f"{self.token0.symbol}: {self.reserves_token0}"
                            )
                            print(
                                f"{self.token1.symbol}: {self.reserves_token1}"
                            )
                        if print_ratios:
                            print(
                                f"{self.token0.symbol}/{self.token1.symbol}: {(self.reserves_token0/10**self.token0.decimals) / (self.reserves_token1/10**self.token1.decimals)}"
                            )
                            print(
                                f"{self.token1.symbol}/{self.token0.symbol}: {(self.reserves_token1/10**self.token1.decimals) / (self.reserves_token0/10**self.token0.decimals)}"
                            )

                    # recalculate possible swaps using the new reserves
                    self.calculate_tokens_in_from_ratio_out()

                    self._update_pool_state()

                    return True
                else:
                    return False
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
            # assert (
            #     external_token0_reserves is not None
            #     and external_token1_reserves is not None
            # ), "Called update_reserves without providing reserve values for both tokens!"

            # skip follow-up processing if the LP object already has the latest reserves, or if no reserves were provided
            if (
                external_token0_reserves == self.reserves_token0
                and external_token1_reserves == self.reserves_token1
            ):
                self.new_reserves = False
                return False
            else:
                self.reserves_token0 = external_token0_reserves
                self.reserves_token1 = external_token1_reserves
                self.new_reserves = True
                self._update_pool_state()

            if not silent:
                print(f"[{self.name}]")
                if print_reserves:
                    print(f"{self.token0.symbol}: {self.reserves_token0}")
                    print(f"{self.token1.symbol}: {self.reserves_token1}")
                if print_ratios:
                    print(
                        f"{self.token0.symbol}/{self.token1.symbol}: {self.reserves_token0 / self.reserves_token1}"
                    )
                    print(
                        f"{self.token1.symbol}/{self.token0.symbol}: {self.reserves_token1 / self.reserves_token0}"
                    )
            self.calculate_tokens_in_from_ratio_out()
            return True
        elif self._update_method == "event":
            raise DeprecationError(
                "***"
                "DEPRECATION WARNING: the 'event' update method is deprecated. Please update your bot to use the default 'polling' method"
                "***"
            )
