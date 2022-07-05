import brownie
import time
from decimal import Decimal
from fractions import Fraction
from typing import List
from ..token import Erc20Token
from ..router import Router


class LiquidityPool:
    def __init__(
        self,
        address: str,
        tokens: List[Erc20Token] = [],
        name: str = "",
        update_method: str = "polling",
        router: Router = None,
        abi: list = None,
        # default fee for most UniswapV2 AMMs is 0.3%
        fee: Fraction = Fraction(3, 1000),
        silent: bool = False,
        update_reserves_on_start: bool = True,
    ) -> None:

        # transforms to checksummed address
        self.address = brownie.convert.to_address(address)

        if router:
            self.router = router

        self.fee = fee
        self._update_method = update_method
        self._ratio_token0_in = None
        self._ratio_token1_in = None
        self.new_reserves = None
        self.update_timestamp = None

        try:
            self._contract = brownie.Contract(self.address)
            self.abi = self._contract.abi
        except Exception as e:
            print(e)
            if abi:
                self._contract = brownie.Contract.from_abi(
                    name="", abi=abi, address=self.address
                )
                self.abi = abi
            else:
                self._contract = brownie.Contract.from_explorer(address=self.address)
                self.abi = self._contract.abi

        self.factory = brownie.convert.to_address(self._contract.factory())

        # if a token pair was provided, check and set pointers for token0 and token1
        if tokens:
            assert len(tokens) == 2, f"Expected 2 tokens, found {len(tokens)}"
            for token in tokens:
                if token.address == self._contract.token0():
                    self.token0 = token
                if token.address == self._contract.token1():
                    self.token1 = token
            assert (
                tokens[0].address == self._contract.token0()
                and tokens[1].address == self._contract.token1()
            ) or (
                tokens[0].address == self._contract.token1()
                and tokens[1].address == self._contract.token0()
            ), "token addresses do not match the on-chain contract!"
        else:
            self.token0 = Erc20Token(address=self._contract.token0())
            self.token1 = Erc20Token(address=self._contract.token1())

        if name:
            self.name = name
        else:
            self.name = f"{self.token0.symbol}-{self.token1.symbol}"

        if update_reserves_on_start:
            self.reserves_token0, self.reserves_token1 = self._contract.getReserves()[
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

        if not silent:
            print(self.name)
            print(f"• Token 0: {self.token0.symbol} - Reserves: {self.reserves_token0}")
            print(f"• Token 1: {self.token1.symbol} - Reserves: {self.reserves_token1}")

    def __eq__(self, other) -> bool:
        return self.address == other.address

    def __str__(self):
        """
        Return the pool name when the object is included in a print statement, or cast as a string
        """
        return self.name

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
        token_in: Erc20Token,
        token_out_quantity: int,
        override_reserves_token0: int = 0,
        override_reserves_token1: int = 0,
    ) -> int:
        """
        Calculates the required token INPUT of token_in for a target OUTPUT at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        """

        assert (override_reserves_token0 == 0 and override_reserves_token1 == 0) or (
            override_reserves_token0 != 0 and override_reserves_token1 != 0
        ), "Must provide override values for both token reserves"

        if token_in.address == self.token0.address:
            if override_reserves_token0 or override_reserves_token1:
                reserves_in = override_reserves_token0
                reserves_out = override_reserves_token1
            else:
                reserves_in = self.reserves_token0
                reserves_out = self.reserves_token1
        elif token_in.address == self.token1.address:
            if override_reserves_token0 or override_reserves_token1:
                reserves_in = override_reserves_token1
                reserves_out = override_reserves_token0
            else:
                reserves_in = self.reserves_token1
                reserves_out = self.reserves_token0
        else:
            print("WTF?  Could not identify token_in")
            raise Exception

        numerator = reserves_in * token_out_quantity * self.fee.denominator
        denominator = (reserves_out - token_out_quantity) * (
            self.fee.denominator - self.fee.numerator
        )
        return numerator // denominator + 1

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_reserves_token0: int = 0,
        override_reserves_token1: int = 0,
    ) -> int:
        """
        Calculates the expected token OUTPUT for a target INPUT at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        """

        assert (override_reserves_token0 == 0 and override_reserves_token1 == 0) or (
            override_reserves_token0 != 0 and override_reserves_token1 != 0
        ), "Must provide override values for both token reserves"

        if token_in.address == self.token0.address:
            if override_reserves_token0 or override_reserves_token1:
                reserves_in = override_reserves_token0
                reserves_out = override_reserves_token1
            else:
                reserves_in = self.reserves_token0
                reserves_out = self.reserves_token1
        elif token_in.address == self.token1.address:
            if override_reserves_token0 or override_reserves_token1:
                reserves_in = override_reserves_token1
                reserves_out = override_reserves_token0
            else:
                reserves_in = self.reserves_token1
                reserves_out = self.reserves_token0

        amount_in_with_fee = token_in_quantity * (
            self.fee.denominator - self.fee.numerator
        )
        numerator = amount_in_with_fee * reserves_out
        denominator = reserves_in * self.fee.denominator + amount_in_with_fee
        return numerator // denominator

    def set_swap_target(
        self,
        token_in: Erc20Token,
        token_in_qty,
        token_out: Erc20Token,
        token_out_qty,
        silent: bool = False,
    ):
        # check to ensure that token_in is one of the two tokens held by the LP
        assert (
            token_in.address == self.token0.address
            and token_out.address == self.token1.address
        ) or (
            token_in.address == self.token1.address
            and token_out.address == self.token0.address
        ), "Tokens must match the two tokens held by this pool!"

        if not silent:
            print(
                f"{token_in} -> {token_out} @ ({token_in_qty} {token_in} = {token_out_qty} {token_out})"
            )

        if token_in.address == self.token0.address:
            # calculate the ratio of token0/token1 for swap of token0 -> token1
            self._ratio_token0_in = Decimal(str(token_in_qty)) / Decimal(
                str(token_out_qty)
            )

        if token_in.address == self.token1.address:
            # calculate the ratio of token1/token0 for swap of token1 -> token0
            self._ratio_token1_in = Decimal(str(token_in_qty)) / Decimal(
                str(token_out_qty)
            )

        self.calculate_tokens_in_from_ratio_out()

    def update_reserves(
        self,
        silent: bool = False,
        print_reserves: bool = True,
        print_ratios: bool = True,
        external_token0_reserves: bool = None,
        external_token1_reserves: bool = None,
        override_update_method: str = None,
    ) -> bool:
        """
        Checks for updated reserve values when set to "polling", otherwise
        if set to "external" assumes that internal LP reserves are valid and recalculates token ratios
        """

        # record the last time this LP was updated
        self.update_timestamp = time.monotonic()

        if self._update_method == "polling" or override_update_method == "polling":
            try:
                result = self._contract.getReserves()[0:2]
                # Compare reserves to last-known values,
                # store and print the reserves if they have changed
                if (self.reserves_token0, self.reserves_token1) != result[0:2]:
                    self.reserves_token0, self.reserves_token1 = result[0:2]
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
                    # recalculate possible swaps using the new reserves, then return True
                    self.calculate_tokens_in_from_ratio_out()
                    return True
                else:
                    return False
            except Exception as e:
                print(f"LiquidityPool: Exception in update_reserves (polling): {e}")

        elif self._update_method == "external":
            assert (
                external_token0_reserves and external_token1_reserves
            ), "Reserve values must be provided for both tokens!"

            # skip follow-up processing if the LP object already has the latest reserves
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
            print("***")
            print(
                "DEPRECATION WARNING: the 'event' update method is deprecated. Please update your bot to use the default 'polling' method going forward"
            )
            print("***")
