import brownie
from ..chainlink.chainlink import *
from ..abis.abis import *

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
        user: brownie.network.account.LocalAccount,
        abi: list = None,
        oracle_address: str = None,
    ) -> None:
        self.address = address
        self._user = user
        if abi:
            try:
                self._contract = brownie.Contract.from_abi(
                    name="", address=self.address, abi=abi
                )
            except:
                raise
        else:
            try:
                self._contract = brownie.Contract.from_explorer(self.address)
            except:
                self._contract = brownie.Contract.from_abi(
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
