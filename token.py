import json

from typing import Optional
from warnings import catch_warnings, simplefilter

from brownie import Contract, web3 as brownie_w3
from brownie.convert import to_address
from brownie.convert.datatypes import HexString
from brownie.network.account import LocalAccount
from hexbytes import HexBytes

from degenbot.chainlink import ChainlinkPriceContract

MIN_ERC20_ABI = json.loads(
    """
    [{"constant": true, "inputs": [], "name": "name", "outputs": [ { "name": "", "type": "string" } ], "payable": false, "stateMutability": "view", "type": "function" }, { "constant": false, "inputs": [ { "name": "_spender", "type": "address" }, { "name": "_value", "type": "uint256" } ], "name": "approve", "outputs": [ { "name": "", "type": "bool" } ], "payable": false, "stateMutability": "nonpayable", "type": "function" }, { "constant": true, "inputs": [], "name": "totalSupply", "outputs": [ { "name": "", "type": "uint256" } ], "payable": false, "stateMutability": "view", "type": "function" }, { "constant": false, "inputs": [ { "name": "_from", "type": "address" }, { "name": "_to", "type": "address" }, { "name": "_value", "type": "uint256" } ], "name": "transferFrom", "outputs": [ { "name": "", "type": "bool" } ], "payable": false, "stateMutability": "nonpayable", "type": "function" }, { "constant": true, "inputs": [], "name": "decimals", "outputs": [ { "name": "", "type": "uint8" } ], "payable": false, "stateMutability": "view", "type": "function" }, { "constant": true, "inputs": [ { "name": "_owner", "type": "address" } ], "name": "balanceOf", "outputs": [ { "name": "balance", "type": "uint256" } ], "payable": false, "stateMutability": "view", "type": "function" }, { "constant": true, "inputs": [], "name": "symbol", "outputs": [ { "name": "", "type": "string" } ], "payable": false, "stateMutability": "view", "type": "function" }, { "constant": false, "inputs": [ { "name": "_to", "type": "address" }, { "name": "_value", "type": "uint256" } ], "name": "transfer", "outputs": [ { "name": "", "type": "bool" } ], "payable": false, "stateMutability": "nonpayable", "type": "function" }, { "constant": true, "inputs": [ { "name": "_owner", "type": "address" }, { "name": "_spender", "type": "address" } ], "name": "allowance", "outputs": [ { "name": "", "type": "uint256" } ], "payable": false, "stateMutability": "view", "type": "function" }, { "payable": true, "stateMutability": "payable", "type": "fallback" }, { "anonymous": false, "inputs": [ { "indexed": true, "name": "owner", "type": "address" }, { "indexed": true, "name": "spender", "type": "address" }, { "indexed": false, "name": "value", "type": "uint256" } ], "name": "Approval", "type": "event" }, { "anonymous": false, "inputs": [ { "indexed": true, "name": "from", "type": "address" }, { "indexed": true, "name": "to", "type": "address" }, { "indexed": false, "name": "value", "type": "uint256" } ], "name": "Transfer", "type": "event"}]
    """
)


class Erc20Token:
    """
    Represents an ERC-20 token. Must be initialized with an address.
    Brownie will load the Contract object from storage, then attempt to load the verified ABI from the block explorer.
    If both methods fail, it will attempt to use a supplied ERC-20 ABI

    If built with `min_abi=True`, a minimal ERC-20 ABI will be used instead of
    fetching the verified contract ABI from Etherscan (or similar).
    """

    def __init__(
        self,
        address: str,
        user: Optional[LocalAccount] = None,
        abi: Optional[list] = None,
        oracle_address: Optional[str] = None,
        silent: bool = False,
        unload_brownie_contract_after_init: bool = False,
        min_abi: bool = False,
    ) -> None:

        try:
            self.address = to_address(address)
        except ValueError:
            print(
                "Could not checksum address, storing non-checksummed version"
            )
            self.address = address

        if user:
            self._user = user

        if min_abi:
            self._contract = Contract.from_abi(
                name=f"ERC-20 @ {address}",
                address=self.address,
                abi=MIN_ERC20_ABI,
                persist=False,
            )
        else:
            with catch_warnings():
                simplefilter("ignore")
                try:
                    # attempt to load stored contract
                    self._contract = Contract(self.address)
                except:
                    # use the provided ABI if given
                    if abi:
                        try:
                            self._contract = Contract.from_abi(
                                name="", address=self.address, abi=abi
                            )
                        except:
                            raise
                    # otherwise attempt to fetch from the block explorer
                    else:
                        try:
                            self._contract = Contract.from_explorer(address)
                        except:
                            raise

        try:
            self.name = self._contract.name()
        except (OverflowError, ValueError):
            self.name = brownie_w3.eth.call(
                {
                    "to": self.address,
                    "data": brownie_w3.keccak(text="name()"),
                }
            )

        # if "name" in dir(self._contract):
        #     self.name = self._contract.name()
        # elif "NAME" in dir(self._contract):
        #     self.name = self._contract.NAME()
        # elif (
        #     "_name" in dir(self._contract)
        #     and type(self._contract._name) != str
        # ):
        #     self.name = self._contract._name()
        # else:
        #     print(
        #         f"Contract does not have a 'name' or similar function. Setting to 'UNKNOWN', confirm on Etherscan: address {address}"
        #     )
        #     self.name = "UNKNOWN"

        if type(self.name) in [HexString, HexBytes]:
            self.name = self.name.decode()

        try:
            self.symbol = self._contract.symbol()
        except OverflowError:
            self.symbol = brownie_w3.eth.call(
                {
                    "to": self.address,
                    "data": brownie_w3.keccak(text="symbol()"),
                }
            )

        # if "symbol" in dir(self._contract):
        #     self.symbol = self._contract.symbol()
        # elif "SYMBOL" in dir(self._contract):
        #     self.symbol = self._contract.SYMBOL()
        # elif "_symbol" in dir(self._contract):
        #     self.symbol = self._contract._symbol()
        # else:
        #     print(
        #         f"Contract does not have a 'symbol' or similar function. Setting to 'UNKNOWN', confirm on Etherscan: address {address}"
        #     )
        #     self.symbol = "UNKNOWN"

        if type(self.symbol) in [HexString, HexBytes]:
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
        if not isinstance(other, Erc20Token):
            raise TypeError(
                f"Equality can only be evaluated against another Erc20Token. Found {type(other)}"
            )
        return self.address.lower() == other.address.lower()

    def __lt__(self, other) -> bool:
        return self.address.lower() < other.address.lower()

    def __gt__(self, other) -> bool:
        return self.address.lower() > other.address.lower()

    def __str__(self):
        return self.symbol

    def get_approval(self, external_address: str):
        return self._contract.allowance(self._user.address, external_address)

    def set_approval(self, external_address: str, value: int):
        """
        Sets the approval value for an external contract to transfer tokens quantites up to the specified amount from this address.
        For unlimited approval, set value to -1
        """
        if type(value) is not int:
            raise TypeError("Value must be an integer!")

        if not (-1 <= value <= 2**256 - 1):
            raise ValueError(
                "Approval value MUST be an integer between 0 and 2**256-1, or -1"
            )

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
