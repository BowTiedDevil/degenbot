from curses.ascii import FF
import string
import time
import datetime
import json

from decimal import Decimal
from brownie import *
from abis import *


class Router:
    """
    Represents a Uniswap V2 router contract
    """

    def __init__(
        self,
        address: str,
        name: str,
        user: network.account.LocalAccount,
        abi: list = None,
    ) -> None:
        self.address = address
        if abi:
            self._contract = Contract.from_abi(name=name, address=address, abi=abi)
        else:
            self._contract = Contract.from_explorer(address=address)
        self.name = name
        self._user = user
        print(f"• {name}")

    def __str__(self) -> str:
        return self.name

    def token_swap(
        self,
        token_in_quantity: int,
        token_in_address: str,
        token_out_quantity: int,
        token_out_address: str,
        slippage: Decimal,
        deadline: int,
        scale=0,
    ) -> bool:
        try:
            params = {}
            params["from"] = self._user.address
            # if scale:
            #     params['priority_fee'] = get_scaled_priority_fee()

            self._contract.swapExactTokensForTokens(
                token_in_quantity,
                int(token_out_quantity * (1 - slippage)),
                [token_in_address, token_out_address],
                self._user.address,
                1000 * int(time.time() + deadline),
                params,
            )
            return True
        except Exception as e:
            print(f"Exception: {e}")
            return False


class ChainlinkPriceContract:
    """
    Represents an on-chain Chainlink price oracle
    """

    def __init__(self, address: str) -> None:
        try:
            self._contract: Contract = Contract.from_explorer(address=address)
            self._decimals: int = self._contract.decimals.call()
            self.price: float = self.update_price
        except:
            raise

    def update_price(self) -> float:
        try:
            latest_price: float = self._contract.latestRoundData.call()[1] / (
                10 ** self._decimals()
            )
            self.price = latest_price
            return latest_price
        except Exception as e:
            print(f"Exception in update_price: {e}")


class Erc20Token:
    """
    Represents an ERC-20 token. Must be initialized with an address.
    Brownie will load the Contract object from the supplied ABI if given,
    then attempt to load the verified ABI from the block explorer.
    If both methods fail, it will attempt to use a supplied ERC-20 ABI
    """

    def __init__(
        self,
        address: str,
        user: network.account.LocalAccount,
        abi: list = None,
        oracle_address: str = None,
    ) -> None:
        self.address = address
        self._user = user
        if abi:
            try:
                self._contract = Contract.from_abi(
                    name="", address=self.address, abi=abi
                )
            except:
                raise
        else:
            try:
                self._contract = Contract.from_explorer(self.address)
            except:
                self._contract = Contract.from_abi(
                    name="", address=self.address, abi=ERC20
                )
        self.name = self._contract.name.call()
        self.symbol = self._contract.symbol.call()
        self.decimals = self._contract.decimals.call()
        self.balance = self._contract.balanceOf.call(self._user)
        self.normalized_balance = self.balance / (10 ** self.decimals)
        if oracle_address:
            self._price_oracle = ChainlinkPriceContract(address=oracle_address)
            self.price = self._price_oracle.price
        print(f"• {self.symbol} ({self.name})")

    def __str__(self):
        return self.symbol

    def get_approval(self, external_address: str):
        return self._contract.allowance.call(self._user.address, external_address)

    def set_approval(self, external_address: string, value: int):
        """
        Sets the approval value for an external contract to transfer tokens quantites up to the specified amount from this address.
        For unlimited approval, set value to -1
        """
        assert type(value) is int and (
            -1 <= value <= 2 ** 256 - 1
        ), "Approval value MUST be an integer between 0 and 2**256-1, or -1"

        if value == -1:
            print("Setting unlimited approval!")
            value = 2 ** 256 - 1

        try:
            self._contract.approve(
                external_address,
                value,
                {"from": self._user.address},
            )
        except Exception as e:
            print(f"Exception in token_approve: {e}")
            raise

    def update_balance(self):
        self.balance = self._contract.balanceOf.call(self._user)
        self.normalized_balance = self.balance / (10 ** self.decimals)

    def update_price(self):
        self.price = self._price_oracle.update_price()


class LiquidityPool:
    def __init__(
        self,
        address: str,
        router: Router,
        name: str,
        tokens: list,
        update_method: str = "polling",
        abi: list = None,
        # default fee for most UniswapV2 AMMs is 0.3%
        fee: Decimal = Decimal("0.003"),
        silent: bool = False,
    ):
        self.address = address
        self.name = name
        self.router = router
        self.fee = fee
        self._update_method = update_method
        self._filter = None
        self._filter_active = False

        if abi:
            self._contract = Contract.from_abi(name="", abi=abi, address=self.address)
            self.abi = abi
        else:
            self._contract = Contract.from_explorer(address=self.address)
            self.abi = self._contract.abi

        # set pointers for token0 and token1 to link to our actual token classes
        self.token0_address = self._contract.token0()
        for token in tokens:
            if token.address == self.token0_address:
                self.token0 = token

        self.token1_address = self._contract.token1()
        for token in tokens:
            if token.address == self.token1_address:
                self.token1 = token

        self.reserves_token0, self.reserves_token1 = self._contract.getReserves.call()[
            0:2
        ]

        if self._update_method == "event" and self._create_filter():
            self._filter_active = True

        if not silent:
            print(self.name)
            print(f"• Token 0: {self.token0.symbol}")
            print(f"• Token 1: {self.token1.symbol}")

    def _create_filter(self):
        """
        Create a web3.py event filter to watch for Sync events
        """

        # Recreating the filter after a disconnect sometimes fails, returning blank results when .get_new_entries() is called.
        # Deleting it first seems to fix that behavior
        del self._filter

        try:
            self._filter = web3.eth.contract(
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

    def calculate_tokens_in(self) -> int:
        """
        Calculates the maximum token inputs for the target output ratios at current pool reserves
        """

        # token0 in, token1 out
        # formula: dx = y0*C_0to1 - x0/(1-FEE)
        self.token0_max_swap = max(
            0,
            int(
                self.reserves_token1 * self.ratio_token0_per_token1
                - self.reserves_token0 / (1 - self.fee)
            ),
        )

        # token1 in, token0 out
        # formula: dy = x0/C_0to1 - y0/(1-FEE) or dy = x0*C_1to0 - y0(1/FEE)
        self.token1_max_swap = max(
            0,
            int(
                self.reserves_token0 * self.ratio_token1_per_token0
                - self.reserves_token1 / (1 - self.fee)
            ),
        )

    def calculate_tokens_out(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
    ) -> int:
        """
        Calculates the expected token output for a swap at current pool reserves.
        Uses the self.token0 and self.token1 pointer to determine which token is being swapped in
        and uses the appropriate formula
        """

        if token_in is self.token0:
            return (self.reserves_token1 * token_in_quantity * (1 - self.fee)) // (
                self.reserves_token0 + token_in_quantity * (1 - self.fee)
            )

        if token_in is self.token1:
            return (self.reserves_token0 * token_in_quantity * (1 - self.fee)) // (
                self.reserves_token1 + token_in_quantity * (1 - self.fee)
            )

    def set_swap_target(
        self, token_in: Erc20Token, targets: list, silent: bool = False
    ):
        # example: token_in=wsohm, targets=[(1, wsohm), (1.1, gohm)])
        # check to ensure that token_in is one of the two tokens held by the LP
        assert (token_in is self.token0) or (token_in is self.token1)
        # check that the targets list contains only the two tokens held by the LP
        assert (targets[0][1] is self.token0 and targets[1][1] is self.token1) or (
            targets[0][1] is self.token1 and targets[1][1] is self.token0
        )

        if not silent:
            if token_in is self.token0:
                token_out = self.token1
            else:
                token_out = self.token0
            print(
                f"Setting swap target: {token_in} -> {token_out} @ {targets[0][0]} {targets[0][1]} = {targets[1][0]} {targets[1][1]}"
            )

        if token_in is self.token0:
            if token_in is targets[0][1]:
                # token0 appears 1st in the list
                self.ratio_token1_per_token0 = Decimal(str(targets[1][0])) / Decimal(
                    str(targets[0][0])
                )
            if token_in is targets[1][1]:
                # token0 appears 2nd in the list
                self.ratio_token1_per_token0 = Decimal(str(targets[0][0])) / Decimal(
                    str(targets[1][0])
                )

        if token_in is self.token1:
            if token_in is targets[0][1]:
                # token_in is the 1st in the list
                self.ratio_token0_per_token1 = Decimal(str(targets[1][0])) / Decimal(
                    str(targets[0][0])
                )
            if token_in is targets[1][1]:
                # token_in is the 2nd in the list
                self.ratio_token0_per_token1 = Decimal(str(targets[0][0])) / Decimal(
                    str(targets[1][0])
                )

    def update_reserves(self, silent: bool = False):
        """
        Checks the event filter for the last Sync event if the method is set to "polling"
        Otherwise call getReserves() directly on the LP contract
        """

        if self._update_method == "event":

            # check and recreate the filter if it's
            if not self._filter_active:
                # recreate the filter
                self._create_filter()

            try:
                events = self._filter.get_new_entries()
                # retrieve Sync events from the event filter, store and print reserve values from the last-seen event
                if events:
                    self.reserves_token0, self.reserves_token1 = json.loads(
                        web3.toJSON(events[-1]["args"])
                    ).values()
                    if not silent:
                        print(
                            f"[{self.name} - {datetime.datetime.now().strftime('%I:%M:%S %p')}]\n{self.token0.symbol}: {self.reserves_token0}\n{self.token1.symbol}: {self.reserves_token1}\n"
                        )
            except Exception as e:
                print(f"Exception in (event) update_reserves: {e}")
                self._filter_active = False

        if self._update_method == "polling":
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
            except Exception as e:
                print(f"Exception in (polling) update_reserves: {e}")

        # recalculate possible swaps using the new reserves
        self.calculate_tokens_in()
