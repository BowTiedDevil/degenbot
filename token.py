import json
from typing import Optional, Union
from warnings import catch_warnings, simplefilter, warn

import brownie  # type: ignore
from brownie.convert.datatypes import HexString  # type: ignore
from brownie.network.account import LocalAccount  # type: ignore
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3 import Web3

from degenbot.chainlink import ChainlinkPriceContract
from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.logging import logger

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
        self.address: ChecksumAddress = Web3.toChecksumAddress(address)

        if user:
            self._user = user

        if min_abi:
            self._brownie_contract = brownie.Contract.from_abi(
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
                    self._brownie_contract = brownie.Contract(self.address)
                except:
                    # use the provided ABI if given
                    if abi:
                        try:
                            self._brownie_contract = brownie.Contract.from_abi(
                                name=f"ERC-20 @ {address}",
                                address=self.address,
                                abi=abi,
                            )
                        except:
                            raise
                    # otherwise attempt to fetch from the block explorer
                    else:
                        try:
                            self._brownie_contract = (
                                brownie.Contract.from_explorer(address)
                            )
                        except:
                            raise

        try:
            self.name = self._brownie_contract.name()
        except (OverflowError, ValueError):
            self.name = brownie.web3.eth.call(
                {
                    "to": self.address,
                    "data": Web3.keccak(text="name()"),
                }
            )
        except:
            warn(
                f"Token contract at {address} does not implement a 'name' function."
            )
            self.name = f"UNKNOWN TOKEN @ {self.address}"
        if type(self.name) in [HexString, HexBytes]:
            self.name = self.name.decode()

        try:
            self.symbol = self._brownie_contract.symbol()
        except (OverflowError, ValueError):
            self.symbol = brownie.web3.eth.call(
                {
                    "to": self.address,
                    "data": Web3.keccak(text="symbol()"),
                }
            )
        if type(self.symbol) in [HexString, HexBytes]:
            self.symbol = self.symbol.decode()

        self.decimals: int

        try:
            self.decimals = self._brownie_contract.decimals()
        except:
            warn(
                f"Token contract at {address} does not implement a 'decimals' function. Setting to 0."
            )
            self.decimals = 0

        if user:
            self.balance = self._brownie_contract.balanceOf(self._user)
            self.normalized_balance = self.balance / (10**self.decimals)

        self.price: Optional[float]

        if oracle_address:
            self._price_oracle = ChainlinkPriceContract(address=oracle_address)
            self.price = self._price_oracle.price
        else:
            self.price = None

        if not silent:
            logger.info(f"• {self.symbol} ({self.name})")

        # Memory savings if token contract object is not used after initialization
        if unload_brownie_contract_after_init:
            self._brownie_contract = None

    # The Brownie contract object cannot be pickled, so remove it and return the state
    def __getstate__(self):
        keys_to_remove = ["_brownie_contract"]
        state = self.__dict__.copy()
        for key in keys_to_remove:
            if key in state:
                del state[key]
        return state

    def __setstate__(self, state):
        self.__dict__ = state

    def __eq__(self, other) -> bool:
        if isinstance(other, Erc20Token):
            return self.address == other.address
        elif isinstance(other, str):
            return self.address.lower() == other.lower()
        else:
            return NotImplemented

    def __lt__(self, other) -> bool:
        if isinstance(other, Erc20Token):
            return self.address < other.address
        elif isinstance(other, str):
            return self.address.lower() < other.lower()
        else:
            return NotImplemented

    def __gt__(self, other) -> bool:
        if isinstance(other, Erc20Token):
            return self.address > other.address
        elif isinstance(other, str):
            return self.address.lower() > other.lower()
        else:
            return NotImplemented

    def __str__(self):
        return self.symbol

    def get_approval(self, external_address: str):
        return self._brownie_contract.allowance(
            self._user.address, external_address
        )

    def set_approval(self, external_address: str, value: Union[int, str]):
        """
        Sets the approval value for an external contract to transfer tokens
        quantites up to the specified amount from this address. For unlimited
        approval, set value to the string "UNLIMITED".
        """

        if isinstance(value, int):
            if not (MIN_UINT256 <= value <= MAX_UINT256):
                raise ValueError(
                    f"Provide an integer value between 0 and 2**256-1"
                )
        elif isinstance(value, str):
            if value != "UNLIMITED":
                raise ValueError("Value must be 'UNLIMITED' or an integer")
            else:
                print("Setting unlimited approval!")
                value = MAX_UINT256
        else:
            raise TypeError(
                f"Value must be an integer or string! Was {type(value)}"
            )

        try:
            self._brownie_contract.approve(
                external_address,
                value,
                {"from": self._user.address},
            )
        except Exception as e:
            print(f"Exception in token_approve: {e}")
            raise

    def update_balance(self):
        self.balance = self._brownie_contract.balanceOf(self._user)
        self.normalized_balance = self.balance / (10**self.decimals)

    def update_price(self):
        self._price_oracle.update_price()
        self.price = self._price_oracle.price
