import time
from decimal import Decimal
from fractions import Fraction
from typing import List, Union

from brownie import Contract, Wei
from brownie.convert import to_address

from .router import Router
from degenbot.token import Erc20Token


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

        assert (
            type(fee) == Fraction
        ), f"LP fee was not correctly passed! Expected '{Fraction().__class__.__name__}', was '{fee.__class__.__name__}'"

        self.fee = fee
        self._update_method = update_method
        self._ratio_token0_in = None
        self._ratio_token1_in = None
        self.new_reserves = None
        self.update_block = 0

        try:
            self._contract = Contract(self.address)
        except:
            if abi:
                self._contract = Contract.from_abi(
                    name="", abi=abi, address=self.address
                )
                self.abi = abi
            else:
                self._contract = Contract.from_explorer(address=self.address)
        else:
            self.abi = self._contract.abi

        self.factory = to_address(self._contract.factory())

        # if a token pair was provided, check and set pointers for token0 and token1
        if tokens:
            assert len(tokens) == 2, f"Expected 2 tokens, found {len(tokens)}"
            for token in tokens:
                if token.address == self._contract.token0():
                    self.token0 = token
                elif token.address == self._contract.token1():
                    self.token1 = token
        else:
            self.token0 = Erc20Token(address=self._contract.token0())
            self.token1 = Erc20Token(address=self._contract.token1())

        if name:
            self.name = name
        else:
            self.name = f"{self.token0.symbol}-{self.token1.symbol} (V2, {100*self.fee.numerator/self.fee.denominator:.2f}%)"

        if update_reserves_on_start:
            (
                self.reserves_token0,
                self.reserves_token1,
            ) = self._contract.getReserves()[0:2]
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

        self.state = {
            "reserves_token0": self.reserves_token0,
            "reserves_token1": self.reserves_token1,
        }

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

        assert (
            override_reserves_token0 == 0 and override_reserves_token1 == 0
        ) or (
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
            print(f"Could not identify token_in: {token_in}!")
            print(f"This pool holds: {self.token0} {self.token1}")
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

        assert (
            override_reserves_token0 == 0 and override_reserves_token1 == 0
        ) or (
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
            print(f"Could not identify token_in: {token_in}!")
            print(f"This pool holds: {self.token0} {self.token1}")
            raise Exception

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
            self._ratio_token0_in = Decimal(
                str(token_in_qty * 10**token_in.decimals)
            ) / Decimal(str(token_out_qty * 10**token_out.decimals))

        if token_in.address == self.token1.address:
            # calculate the ratio of token1/token0 for swap of token1 -> token0
            self._ratio_token1_in = Decimal(
                str(token_in_qty * 10**token_in.decimals)
            ) / Decimal(str(token_out_qty * 10**token_out.decimals))

        self.calculate_tokens_in_from_ratio_out()

    def update_reserves(
        self,
        silent: bool = False,
        print_reserves: bool = True,
        print_ratios: bool = True,
        external_token0_reserves: int = None,  # bugfix: hint was set to bool
        external_token1_reserves: int = None,  # bugfix: hint was set to bool
        override_update_method: str = None,
        update_block: int = None,
    ) -> bool:
        """
        Checks for updated reserve values when set to "polling", otherwise
        if set to "external" assumes that internal LP reserves are valid and recalculates token ratios
        """

        # discard stale updates
        if update_block and update_block <= self.update_block:
            return False
        elif update_block and update_block > self.update_block:
            self.update_block = update_block

        if (
            self._update_method == "polling"
            or override_update_method == "polling"
        ):
            try:
                result = self._contract.getReserves()[0:2]
                # Compare reserves to last-known values,
                # store and print the reserves if they have changed
                if (self.reserves_token0, self.reserves_token1) != result[0:2]:
                    self.reserves_token0, self.reserves_token1 = result[0:2]
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
                    self.state = {
                        "reserves_token0": self.reserves_token0,
                        "reserves_token1": self.reserves_token1,
                    }
                    return True
                else:
                    return False
            except Exception as e:
                print(
                    f"LiquidityPool: Exception in update_reserves (polling): {e}"
                )
        elif self._update_method == "external":
            assert (
                external_token0_reserves is not None
                and external_token1_reserves is not None
            ), "Called update_reserves without providing reserve values for both tokens!"

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

                self.state = {
                    "reserves_token0": self.reserves_token0,
                    "reserves_token1": self.reserves_token1,
                }

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
