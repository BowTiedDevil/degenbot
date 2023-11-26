from typing import Any, Dict, Optional, Tuple
from warnings import warn

import ujson
from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address
from web3 import Web3
from web3.contract import Contract
from web3.exceptions import BadFunctionCallOutput, ContractLogicError

from . import config
from .baseclasses import TokenHelper
from .chainlink import ChainlinkPriceContract
from .logging import logger
from .registry import AllTokens

# Taken from OpenZeppelin's ERC-20 implementation
# ref: https://www.npmjs.com/package/@openzeppelin/contracts?activeTab=code
ERC20_ABI_MINIMAL = ujson.loads(
    '[{"inputs": [{"internalType": "string", "name": "name_", "type": "string"}, {"internalType": "string", "name": "symbol_", "type": "string"}], "stateMutability": "nonpayable", "type": "constructor"}, {"anonymous": false, "inputs": [{"indexed": true, "internalType": "address", "name": "owner", "type": "address"}, {"indexed": true, "internalType": "address", "name": "spender", "type": "address"}, {"indexed": false, "internalType": "uint256", "name": "value", "type": "uint256"}], "name": "Approval", "type": "event"}, {"anonymous": false, "inputs": [{"indexed": true, "internalType": "address", "name": "from", "type": "address"}, {"indexed": true, "internalType": "address", "name": "to", "type": "address"}, {"indexed": false, "internalType": "uint256", "name": "value", "type": "uint256"}], "name": "Transfer", "type": "event"}, {"inputs": [{"internalType": "address", "name": "owner", "type": "address"}, {"internalType": "address", "name": "spender", "type": "address"}], "name": "allowance", "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}], "stateMutability": "view", "type": "function"}, {"inputs": [{"internalType": "address", "name": "spender", "type": "address"}, {"internalType": "uint256", "name": "amount", "type": "uint256"}], "name": "approve", "outputs": [{"internalType": "bool", "name": "", "type": "bool"}], "stateMutability": "nonpayable", "type": "function"}, {"inputs": [{"internalType": "address", "name": "account", "type": "address"}], "name": "balanceOf", "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}], "stateMutability": "view", "type": "function"}, {"inputs": [], "name": "decimals", "outputs": [{"internalType": "uint8", "name": "", "type": "uint8"}], "stateMutability": "view", "type": "function"}, {"inputs": [{"internalType": "address", "name": "spender", "type": "address"}, {"internalType": "uint256", "name": "subtractedValue", "type": "uint256"}], "name": "decreaseAllowance", "outputs": [{"internalType": "bool", "name": "", "type": "bool"}], "stateMutability": "nonpayable", "type": "function"}, {"inputs": [{"internalType": "address", "name": "spender", "type": "address"}, {"internalType": "uint256", "name": "addedValue", "type": "uint256"}], "name": "increaseAllowance", "outputs": [{"internalType": "bool", "name": "", "type": "bool"}], "stateMutability": "nonpayable", "type": "function"}, {"inputs": [], "name": "name", "outputs": [{"internalType": "string", "name": "", "type": "string"}], "stateMutability": "view", "type": "function"}, {"inputs": [], "name": "symbol", "outputs": [{"internalType": "string", "name": "", "type": "string"}], "stateMutability": "view", "type": "function"}, {"inputs": [], "name": "totalSupply", "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}], "stateMutability": "view", "type": "function"}, {"inputs": [{"internalType": "address", "name": "to", "type": "address"}, {"internalType": "uint256", "name": "amount", "type": "uint256"}], "name": "transfer", "outputs": [{"internalType": "bool", "name": "", "type": "bool"}], "stateMutability": "nonpayable", "type": "function"}, {"inputs": [{"internalType": "address", "name": "from", "type": "address"}, {"internalType": "address", "name": "to", "type": "address"}, {"internalType": "uint256", "name": "amount", "type": "uint256"}], "name": "transferFrom", "outputs": [{"internalType": "bool", "name": "", "type": "bool"}], "stateMutability": "nonpayable", "type": "function"}]'
)


class Erc20Token(TokenHelper):
    """
    An ERC-20 token contract.

    If an ABI is specified, it will be used. Otherwise, a minimal ERC-20 ABI
    will be used that provides access to the interface defined by EIP-20, plus
    some commonly used extensions:

    - Functions:
        # EIP-20 BASELINE
        - totalSupply()
        - balanceOf(account)
        - transfer(to, amount)
        - allowance(owner, spender)
        - approve(spender, amount)
        - transferFrom(from, to, amount)
        # METADATA EXTENSIONS
        - name()
        - symbol()
        - decimals()
        # ALLOWANCE EXTENSIONS
        - increaseAllowance(spender, addedValue)
        - decreaseAllowance(spender, subtractedValue)
    - Events:
        - Transfer(from, to, value)
        - Approval(owner, spender, value)
    """

    __slots__: Tuple[str, ...] = (
        "_price_oracle",
        "abi",
        "address",
        "decimals",
        "name",
        "price",
        "symbol",
    )

    def __init__(
        self,
        address: str,
        abi: Optional[list] = None,
        oracle_address: Optional[str] = None,
        silent: bool = False,
        unload_brownie_contract_after_init: bool = False,  # deprecated
        min_abi: bool = False,  # deprecated
        user: Optional[Any] = None,  # deprecated
    ) -> None:
        self.address: ChecksumAddress = to_checksum_address(address)
        self.abi = abi if abi is not None else ERC20_ABI_MINIMAL

        _w3 = config.get_web3()
        _w3_contract = self._w3_contract

        if user:  # pragma: no cover
            warn(
                "Instantiating with a single user is deprecated. You may use "
                "the get_balance() method to retrieve token balances for a "
                "particular address."
            )

        if min_abi:  # pragma: no cover
            warn(
                "Using a minimal ABI is now the default behavior. Remove "
                "min_abi constructor argument to stop seeing this message."
            )

        if unload_brownie_contract_after_init:  # pragma: no cover
            warn(
                "unload_brownie_contract_after_init is deprecated. Remove "
                "constructor argument to stop seeing this message."
            )

        try:
            self.name: str
            self.name = _w3_contract.functions.name().call()
        except (ContractLogicError, OverflowError, BadFunctionCallOutput):
            # Workaround for non-ERC20 compliant tokens
            for func in ("name", "NAME"):
                try:
                    self.name = (
                        _w3.eth.call(
                            {
                                "to": self.address,
                                "data": Web3.keccak(text=f"{func}()"),
                            }
                        )
                    ).decode("utf-8", errors="ignore")
                except Exception:
                    continue
                else:
                    break
        except Exception as e:
            print(f"(token.name @ {self.address}) {type(e)}: {e}")
            raise

        try:
            self.name
        except AttributeError:  # pragma: no cover
            if not _w3.eth.get_code(self.address):
                raise ValueError("No contract deployed at this address")
            self.name = f"Unknown @ {self.address}"
            warn(
                f"Token contract at {self.address} does not implement a 'name' function. Setting to '{self.name}'"
            )

        try:
            self.symbol: str
            self.symbol = _w3_contract.functions.symbol().call()
        except (ContractLogicError, OverflowError, BadFunctionCallOutput):
            for func in ("symbol", "SYMBOL"):
                # Workaround for non-ERC20 compliant tokens
                try:
                    self.symbol = (
                        _w3.eth.call(
                            {
                                "to": self.address,
                                "data": Web3.keccak(text=f"{func}()"),
                            }
                        )
                    ).decode("utf-8", errors="ignore")
                except Exception:
                    continue
                else:
                    break
        except Exception as e:
            print(f"(token.symbol @ {self.address}) {type(e)}: {e}")
            raise

        try:
            self.symbol
        except AttributeError:
            if not _w3.eth.get_code(self.address):  # pragma: no cover
                raise ValueError("No contract deployed at this address")
            self.symbol = "UNKN"
            warn(
                f"Token contract at {self.address} does not implement a 'symbol' function. Setting to {self.symbol}"
            )

        try:
            self.decimals: int
            self.decimals = _w3_contract.functions.decimals().call()
        except (ContractLogicError, OverflowError, BadFunctionCallOutput):
            for func in ("decimals", "DECIMALS"):
                try:
                    # Workaround for non-ERC20 compliant tokens
                    self.decimals = int.from_bytes(
                        bytes=_w3.eth.call(
                            {
                                "to": self.address,
                                "data": Web3.keccak(text=f"{func}()"),
                            }
                        ),
                        byteorder="big",
                    )
                except Exception:
                    continue
                else:
                    break
        except Exception as e:
            print(f"(token.decimals @ {self.address}) {type(e)}: {e}")
            raise

        try:
            self.decimals
        except Exception:
            if not _w3.eth.get_code(self.address):  # pragma: no cover
                raise ValueError("No contract deployed at this address")
            self.decimals = 0
            warn(
                f"Token contract at {self.address} does not implement a 'decimals' function. Setting to 0."
            )

        self.price: Optional[float] = None
        if oracle_address:
            self._price_oracle = ChainlinkPriceContract(address=oracle_address)
            self.price = self._price_oracle.price
        else:
            self.price = None

        AllTokens(chain_id=_w3.eth.chain_id)[self.address] = self

        if not silent:
            logger.info(f"• {self.symbol} ({self.name})")

    def __repr__(self):  # pragma: no cover
        return f"Erc20Token(address={self.address}, symbol='{self.symbol}', name='{self.name}', decimals={self.decimals})"

    def __getstate__(self):
        # Remove objects that cannot be pickled and are unnecessary to perform
        # the calculation
        dropped_attributes = ()

        return {
            attr_name: getattr(self, attr_name, None)
            for attr_name in self.__slots__
            if attr_name not in dropped_attributes
        }

    def __setstate__(self, state: Dict):
        for attr_name, attr_value in state.items():
            setattr(self, attr_name, attr_value)

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

    @property
    def _w3_contract(self) -> Contract:
        return config.get_web3().eth.contract(
            address=self.address,
            abi=self.abi,
        )

    def get_approval(self, owner: str, spender: str) -> int:
        return self._w3_contract.functions.allowance(
            to_checksum_address(owner),
            to_checksum_address(spender),
        ).call()

    # def set_approval(self, owner: str, spender: str, value: Union[int, str]):
    #     """
    #     Sets the approval value for an external contract to transfer tokens
    #     quantites up to the specified amount from this address. For unlimited
    #     approval, set value to the string "UNLIMITED".
    #     """

    #     if isinstance(value, int):
    #         if not (MIN_UINT256 <= value <= MAX_UINT256):
    #             raise ValueError(
    #                 f"Provide an integer value between 0 and 2**256-1"
    #             )
    #     elif isinstance(value, str):
    #         if value != "UNLIMITED":
    #             raise ValueError("Value must be 'UNLIMITED' or an integer")
    #         else:
    #             print("Setting unlimited approval!")
    #             value = MAX_UINT256
    #     else:
    #         raise TypeError(
    #             f"Value must be an integer or string! Was {type(value)}"
    #         )

    #     try:
    #         self._w3_contract.functions["approve"](
    #             to_checksum_address(owner),
    #             to_checksum_address(spender),
    #             value,
    #         )
    #         # self._brownie_contract.approve(
    #         #     external_address,
    #         #     value,
    #         #     {"from": self._user.address},
    #         # )
    #     except Exception as e:
    #         print(f"Exception in token_approve: {e}")
    #         raise

    def get_balance(self, address: str) -> int:
        return self._w3_contract.functions.balanceOf(to_checksum_address(address)).call()

    # def update_balance(self):
    #     self.balance = self.get_balance(self._user)
    #     self.normalized_balance = self.balance / (10**self.decimals)

    def update_price(self):
        self._price_oracle.update_price()
        self.price = self._price_oracle.price
