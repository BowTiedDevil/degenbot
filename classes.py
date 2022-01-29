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

    def set_approval(self, external_address: str, value: int):
        if value == "unlimited":
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
    ):
        self.address = address
        self.name = name
        self.router = router
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

    def calculate_tokens_in_at_ratio_out(
        self,
        token0_out: bool = False,
        token1_out: bool = False,
        token0_per_token1: Decimal = 0,
        # default fee for most UniswapV2 AMMs is 0.3%
        fee: Decimal = Decimal("0.003"),
    ) -> int:
        """
        Calculates the maximum token input for a given output ratio at current pool reserves
        """
        assert not (token0_out and token1_out)
        assert token0_per_token1

        # token1 input, token0 output
        if token0_out:
            # dy = x0/C - y0/(1-FEE)
            dy = int(
                self.reserves_token0 / token0_per_token1
                - self.reserves_token1 / (1 - fee)
            )
            return max(0, dy)

        # token0 input, token1 output
        if token1_out:
            # dx = y0*C - x0/(1-FEE)
            dx = int(
                self.reserves_token1 * token0_per_token1
                - self.reserves_token0 / (1 - fee)
            )
            return max(0, dx)

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        # OLD INPUTS, VERIFY REPLACEMENT WORKS BEFORE REMOVING
        # quantity_token0_in: int = 0,
        # quantity_token1_in: int = 0,
        fee: Decimal = 0,
    ) -> int:
        """
        Calculates the expected token output for a swap at current pool reserves
        Uses the self.token0 and self.token1 pointer to determine which token is being swapped in
        and uses the appropriate formula
        """

        if token_in is self.token0:
            return (self.reserves_token1 * token_in_quantity * (1 - fee)) // (
                self.reserves_token0 + token_in_quantity * (1 - fee)
            )

        if token_in is self.token1:
            return (self.reserves_token0 * token_in_quantity * (1 - fee)) // (
                self.reserves_token1 + token_in_quantity * (1 - fee)
            )

    def update_reserves(self):
        """
        Checks the event filter for the last Sync event if the method is set to "polling"
        Otherwise call getReserves() directly on the LP contract
        """

        # check the filter status before proceeding
        if not self._filter_active:
            # recreate the filter
            self._create_filter()

        if self._update_method == "event":
            try:
                events = self._filter.get_new_entries()
                if events:
                    self.reserves_token0, self.reserves_token1 = json.loads(
                        web3.toJSON(events[-1]["args"])
                    ).values()
                    print(
                        f"[{self.name} - {datetime.datetime.now().strftime('%I:%M:%S %p')}]\n{self.token0.symbol}: {self.reserves_token0}\n{self.token1.symbol}: {self.reserves_token1}\n"
                    )
            except Exception as e:
                print(f"Exception in (event) update_reserves: {e}")
                self._filter_active = False

        if self._update_method == "polling":
            try:
                (
                    self.reserves_token0,
                    self.reserves_token1,
                ) = self._contract.getReserves.call()[0:2]
            except Exception as e:
                print(f"Exception in (polling) update_reserves: {e}")
