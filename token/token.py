import brownie

from ..abi import *
from ..chainlink import *


class Erc20Token:
    """
    Represents an ERC-20 token. Must be initialized with an address.
    Brownie will load the Contract object from storage, then attempt to load the verified ABI from the block explorer.
    If both methods fail, it will attempt to use a supplied ERC-20 ABI
    """

    def __init__(
        self,
        address: str,
        user: brownie.network.account.LocalAccount = None,
        abi: list = None,
        oracle_address: str = None,
        silent: bool = False,
        unload_brownie_contract_after_init: bool = False,
    ) -> None:

        try:
            self.address = brownie.convert.to_address(address)
        except ValueError:
            print(
                "Could not checksum address, storing non-checksummed version"
            )
            self.address = address

        if user:
            self._user = user

        try:
            # attempt to load stored contract
            self._contract = brownie.Contract(address)
        except:
            # use the provided ABI if given
            if abi:
                try:
                    self._contract = brownie.Contract.from_abi(
                        name="", address=self.address, abi=abi
                    )
                except:
                    raise
            # otherwise attempt to fetch from the block explorer
            else:
                try:
                    self._contract = brownie.Contract.from_explorer(address)
                except:
                    raise

        if "name" in dir(self._contract):
            self.name = self._contract.name()
        elif "NAME" in dir(self._contract):
            self.name = self._contract.NAME()
        elif (
            "_name" in dir(self._contract)
            and type(self._contract._name) != str
        ):
            self.name = self._contract._name()
        else:
            print(
                f"Contract does not have a 'name' or similar function. Setting to 'UNKNOWN', confirm on Etherscan: address {address}"
            )
            self.name = "UNKNOWN"
        if type(self.name) == brownie.convert.datatypes.HexString:
            self.name = self.name.decode()

        if "symbol" in dir(self._contract):
            self.symbol = self._contract.symbol()
        elif "SYMBOL" in dir(self._contract):
            self.symbol = self._contract.SYMBOL()
        elif "_symbol" in dir(self._contract):
            self.symbol = self._contract._symbol()
        else:
            print(
                f"Contract does not have a 'symbol' or similar function. Setting to 'UNKNOWN', confirm on Etherscan: address {address}"
            )
            self.symbol = "UNKNOWN"

        if type(self.symbol) == brownie.convert.datatypes.HexString:
            self.symbol = self.symbol.decode()

        if "decimals" in dir(self._contract):
            self.decimals = self._contract.decimals()
        elif "DECIMALS" in dir(self._contract):
            self.decimals = self._contract.DECIMALS()
        elif "_decimals" in dir(self._contract):
            self.decimals = self._contract._decimals()
        else:
            print(
                f"Contract does not have a 'decimals' or similar functions. Setting to 18, confirm on Etherscan: address {address}"
            )
            self.decimals = 18

        if user:
            self.balance = self._contract.balanceOf(self._user)
            self.normalized_balance = self.balance / (10**self.decimals)

        if oracle_address:
            self._price_oracle = ChainlinkPriceContract(address=oracle_address)
            self.price = self._price_oracle.price
        else:
            self.price = None

        if not silent:
            print(f"• {self.symbol} ({self.name})")

        # WIP: huge memory savings if token contract object is not used after initialization
        # testing in progress
        if unload_brownie_contract_after_init:
            self._contract = None

    def __eq__(self, other) -> bool:
        assert isinstance(
            other, Erc20Token
        ), "Equality can only be evaluated against another Erc20Token"
        return self.address.lower() == other.address.lower()

    def __str__(self):
        return self.symbol

    def get_approval(self, external_address: str):
        return self._contract.allowance(self._user.address, external_address)

    def set_approval(self, external_address: str, value: int):
        """
        Sets the approval value for an external contract to transfer tokens quantites up to the specified amount from this address.
        For unlimited approval, set value to -1
        """
        assert type(value) is int and (
            -1 <= value <= 2**256 - 1
        ), "Approval value MUST be an integer between 0 and 2**256-1, or -1"

        if value == -1:
            print("Setting unlimited approval!")
            value = 2**256 - 1

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
        self.balance = self._contract.balanceOf(self._user)
        self.normalized_balance = self.balance / (10**self.decimals)

    def update_price(self):
        self._price_oracle.update_price()
        self.price = self._price_oracle.price
