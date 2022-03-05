import datetime
import json
import brownie
from decimal import Decimal
from fractions import Fraction
from ..token import Erc20Token
from ..router import Router


class LiquidityPool:
    def __init__(
        self,
        address: str,
        name: str,
        tokens: list[Erc20Token],
        update_method: str = "polling",
        router: Router = None,
        abi: list = None,
        # default fee for most UniswapV2 AMMs is 0.3%
        # fee: Decimal = Decimal("0.003"),
        fee: Fraction = Fraction(3, 1000),
        silent: bool = False,
    ) -> None:

        # transforms to checksummed address, prevents web3's filter from throwing errors
        self.address = brownie.convert.to_address(address)

        if router:
            self.router = router

        self.name = name
        self.fee = fee
        self._update_method = update_method
        self._filter = None
        self._filter_active = False
        self._ratio_token0_in = None
        self._ratio_token1_in = None

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

        # set pointers for token0 and token1 to link to our actual token classes
        for token in tokens:
            if token.address == self._contract.token0():
                self.token0 = token
            if token.address == self._contract.token1():
                self.token1 = token

        assert (
            tokens[0].address == self._contract.token0.call()
            and tokens[1].address == self._contract.token1.call()
        ) or (
            tokens[0].address == self._contract.token1.call()
            and tokens[1].address == self._contract.token0.call()
        ), "token addresses do not match the on-chain contract!"

        self.reserves_token0, self.reserves_token1 = self._contract.getReserves.call()[
            0:2
        ]

        if self._update_method == "event" and self._create_filter():
            self._filter_active = True

        if not silent:
            print(self.name)
            print(f"• Token 0: {self.token0.symbol} - Reserves: {self.reserves_token0}")
            print(f"• Token 1: {self.token1.symbol} - Reserves: {self.reserves_token1}")

    def __eq__(
        self,
        other,
    ) -> bool:
        return self.address == other.address

    def _create_filter(self):
        """
        Create a web3.py event filter to watch for Sync events
        """

        # Recreating the filter after a disconnect sometimes fails, returning blank results when .get_new_entries() is called.
        # "blanking" it first seems to fix that behavior
        # BUGFIX: this used to delete the filter, but if the follow-up filter creation fails it will crash the bot since self._filter doesn't exist
        # now sets it to None
        self._filter = None

        try:
            self._filter = brownie.web3.eth.contract(
                address=self.address, abi=self.abi
            ).events.Sync.createFilter(fromBlock="latest")
            self._filter_active = True
        except Exception as e:
            print(f"Exception in create_filter: {e}")

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
    ) -> int:
        """
        Calculates the required token INPUT of token_in for a target OUTPUT at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        and uses the appropriate formula
        """

        if token_in.address == self.token0.address:
            return int(
                (self.reserves_token0 * token_out_quantity)
                // ((1 - self.fee) * (self.reserves_token1 - token_out_quantity))
                + 1
            )

        if token_in.address == self.token1.address:
            return int(
                (self.reserves_token1 * token_out_quantity)
                // ((1 - self.fee) * (self.reserves_token0 - token_out_quantity))
                + 1
            )

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
    ) -> int:
        """
        Calculates the expected token OUTPUT for a target INPUT at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        and uses the appropriate formula
        """

        if token_in.address == self.token0.address:
            return int(
                self.reserves_token1 * token_in_quantity * (1 - self.fee)
            ) // int(self.reserves_token0 + token_in_quantity * (1 - self.fee))

        if token_in.address == self.token1.address:
            return int(
                self.reserves_token0 * token_in_quantity * (1 - self.fee)
            ) // int(self.reserves_token1 + token_in_quantity * (1 - self.fee))

    def set_swap_target(
        self,
        token_in: str,
        token_in_qty,
        token_out: str,
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
    ) -> bool:
        """
        Checks the event filter for the last Sync event if the method is set to "polling"
        Otherwise call getReserves() directly on the LP contract
        """

        if self._update_method == "event":
            # check and recreate the filter if necessary
            if not self._filter_active:
                # recreate the filter
                self._create_filter()

            try:
                events = self._filter.get_new_entries()
                # retrieve Sync events from the event filter, store and print reserve values from the last-seen event
                if events:
                    self.reserves_token0, self.reserves_token1 = json.loads(
                        brownie.web3.toJSON(events[-1]["args"])
                    ).values()
                    if not silent:
                        print(
                            f"[{self.name} - {datetime.datetime.now().strftime('%I:%M:%S %p')}]"
                        )
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
                print(f"Exception in (event) update_reserves: {e}")
                self._filter_active = False

        elif self._update_method == "polling":
            try:
                result = self._contract.getReserves.call()[0:2]
                # Compare reserves to last-known values,
                # store and print the reserves if they have changed
                if (self.reserves_token0, self.reserves_token1) != result[0:2]:
                    self.reserves_token0, self.reserves_token1 = result[0:2]
                    if not silent:
                        print(
                            f"[{self.name} - {datetime.datetime.now().strftime('%I:%M:%S %p')}]\n{self.token0.symbol}: {self.reserves_token0}\n{self.token1.symbol}: {self.reserves_token1}\n"
                        )
                    # recalculate possible swaps using the new reserves, then return True
                    self.calculate_tokens_in_from_ratio_out()
                    return True
                else:
                    return False
            except Exception as e:
                print(f"Exception in (polling) update_reserves: {e}")
