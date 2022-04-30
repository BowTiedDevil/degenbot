import datetime
import json
import brownie
from decimal import Decimal
from fractions import Fraction
from ..token import Erc20Token
from ..router import Router
from typing import List

FACTORIES = {
    "0x9Ad6C38BE94206cA50bb0d90783181662f0Cfa10": "TraderJoe",
    "0xc35DADB65012eC5796536bD9864eD8773aBc74C4": "SushiSwap",
    "0xefa94DE7a4656D787667C749f7E1223D71E9FD88": "Pangolin",
}


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
    ) -> None:

        # transforms to checksummed address, prevents web3's event filter from throwing errors
        self.address = brownie.convert.to_address(address)

        if router:
            self.router = router

        self.fee = fee
        self._update_method = update_method
        self._sync_filter = None
        self._sync_filter_active = False
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

        # if a token pair was provided, check and set pointers for token0 and token1
        if tokens:
            assert len(tokens) == 2, f"Expected 2 tokens, found {len(tokens)}"
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
        else:
            self.token0 = Erc20Token(address=self._contract.token0.call())
            self.token1 = Erc20Token(address=self._contract.token1.call())

        if name:
            self.name = name
        else:
            factory_address = str(self._contract.factory.call())
            if factory_address in FACTORIES.keys():
                self.name = (
                    FACTORIES[factory_address]
                    + ": "
                    + self.token0.symbol
                    + "-"
                    + self.token1.symbol
                )
            else:
                self.name = f"Unknown: {self.token0.symbol}-{self.token1.symbol}"

        self.reserves_token0, self.reserves_token1 = self._contract.getReserves.call()[
            0:2
        ]

        if self._update_method == "event":
            print("***")
            print(
                "DEPRECATION WARNING: the 'event' update method is inaccurate, please update your bot to use the default 'polling' method going forward"
            )
            print("***")
            try:
                if self._create_filter():
                    self._sync_filter_active = True
                else:
                    self._sync_filter_active = False
            except Exception as e:
                print(e)

        if not silent:
            print(self.name)
            print(f"• Token 0: {self.token0.symbol} - Reserves: {self.reserves_token0}")
            print(f"• Token 1: {self.token1.symbol} - Reserves: {self.reserves_token1}")

    def __eq__(
        self,
        other,
    ) -> bool:
        return self.address == other.address

    def _create_filter(self) -> bool:
        """
        Create a web3.py event filter to watch for Sync events
        """

        # Recreating the filter after a disconnect sometimes fails, returning blank results when .get_new_entries() is called.
        # "blanking" it first seems to fix that behavior
        # BUGFIX: this used to delete the filter, but if the follow-up filter creation fails it will crash the bot since self._filter doesn't exist
        # now sets it to None
        self._sync_filter = None

        try:
            self._sync_filter = brownie.web3.eth.contract(
                address=self.address, abi=self.abi
            ).events.Sync.createFilter(fromBlock="latest")
            self._sync_filter_active = True
            return True
        except Exception as e:
            print(f"Exception in create_filter: {e}")
            return False

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
                self.reserves_token1
                * token_in_quantity
                * (1 - self.fee)
                // (self.reserves_token0 + token_in_quantity * (1 - self.fee))
            )

        if token_in.address == self.token1.address:
            return int(
                self.reserves_token0
                * token_in_quantity
                * (1 - self.fee)
                // (self.reserves_token1 + token_in_quantity * (1 - self.fee))
            )

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
            print("***")
            print(
                "DEPRECATION WARNING: the 'event' update method is inaccurate, please update your bot to use the default 'polling' method going forward"
            )
            print("***")
            # check and recreate the filter if necessary
            if not self._sync_filter_active:
                # recreate the filter
                if self._create_filter():
                    pass
                else:
                    return False
            try:
                # retrieve Sync events from the event filter, store and print reserve values from the last-seen event
                events = self._sync_filter.get_new_entries()
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
                print(f"LiquidityPool: Exception in update_reserves (event): {e}")
                self._sync_filter_active = False

        elif self._update_method == "polling":
            try:
                result = self._contract.getReserves.call()[0:2]
                # Compare reserves to last-known values,
                # store and print the reserves if they have changed
                if (self.reserves_token0, self.reserves_token1) != result[0:2]:
                    self.reserves_token0, self.reserves_token1 = result[0:2]
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
                print(f"LiquidityPool: Exception in update_reserves (polling): {e}")

        elif self._update_method == "external":
            self.calculate_tokens_in_from_ratio_out()
            return True
