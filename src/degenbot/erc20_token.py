from typing import Any, Dict, List, Tuple

import eth_abi.abi
import ujson
from eth_typing import AnyAddress, ChecksumAddress
from eth_utils.address import to_checksum_address
from web3 import Web3
from web3.contract.contract import Contract
from web3.exceptions import BadFunctionCallOutput, ContractLogicError
from web3.types import BlockIdentifier

from . import config
from .baseclasses import BaseToken
from .chainlink import ChainlinkPriceContract
from .functions import get_number_for_block_identifier
from .logging import logger
from .registry.all_tokens import AllTokens

# Taken from OpenZeppelin's ERC-20 implementation
# ref: https://www.npmjs.com/package/@openzeppelin/contracts?activeTab=code
ERC20_ABI_MINIMAL = ujson.loads(
    '[{"inputs": [{"internalType": "string", "name": "name_", "type": "string"}, {"internalType": "string", "name": "symbol_", "type": "string"}], "stateMutability": "nonpayable", "type": "constructor"}, {"anonymous": false, "inputs": [{"indexed": true, "internalType": "address", "name": "owner", "type": "address"}, {"indexed": true, "internalType": "address", "name": "spender", "type": "address"}, {"indexed": false, "internalType": "uint256", "name": "value", "type": "uint256"}], "name": "Approval", "type": "event"}, {"anonymous": false, "inputs": [{"indexed": true, "internalType": "address", "name": "from", "type": "address"}, {"indexed": true, "internalType": "address", "name": "to", "type": "address"}, {"indexed": false, "internalType": "uint256", "name": "value", "type": "uint256"}], "name": "Transfer", "type": "event"}, {"inputs": [{"internalType": "address", "name": "owner", "type": "address"}, {"internalType": "address", "name": "spender", "type": "address"}], "name": "allowance", "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}], "stateMutability": "view", "type": "function"}, {"inputs": [{"internalType": "address", "name": "spender", "type": "address"}, {"internalType": "uint256", "name": "amount", "type": "uint256"}], "name": "approve", "outputs": [{"internalType": "bool", "name": "", "type": "bool"}], "stateMutability": "nonpayable", "type": "function"}, {"inputs": [{"internalType": "address", "name": "account", "type": "address"}], "name": "balanceOf", "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}], "stateMutability": "view", "type": "function"}, {"inputs": [], "name": "decimals", "outputs": [{"internalType": "uint8", "name": "", "type": "uint8"}], "stateMutability": "view", "type": "function"}, {"inputs": [{"internalType": "address", "name": "spender", "type": "address"}, {"internalType": "uint256", "name": "subtractedValue", "type": "uint256"}], "name": "decreaseAllowance", "outputs": [{"internalType": "bool", "name": "", "type": "bool"}], "stateMutability": "nonpayable", "type": "function"}, {"inputs": [{"internalType": "address", "name": "spender", "type": "address"}, {"internalType": "uint256", "name": "addedValue", "type": "uint256"}], "name": "increaseAllowance", "outputs": [{"internalType": "bool", "name": "", "type": "bool"}], "stateMutability": "nonpayable", "type": "function"}, {"inputs": [], "name": "name", "outputs": [{"internalType": "string", "name": "", "type": "string"}], "stateMutability": "view", "type": "function"}, {"inputs": [], "name": "symbol", "outputs": [{"internalType": "string", "name": "", "type": "string"}], "stateMutability": "view", "type": "function"}, {"inputs": [], "name": "totalSupply", "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}], "stateMutability": "view", "type": "function"}, {"inputs": [{"internalType": "address", "name": "to", "type": "address"}, {"internalType": "uint256", "name": "amount", "type": "uint256"}], "name": "transfer", "outputs": [{"internalType": "bool", "name": "", "type": "bool"}], "stateMutability": "nonpayable", "type": "function"}, {"inputs": [{"internalType": "address", "name": "from", "type": "address"}, {"internalType": "address", "name": "to", "type": "address"}, {"internalType": "uint256", "name": "amount", "type": "uint256"}], "name": "transferFrom", "outputs": [{"internalType": "bool", "name": "", "type": "bool"}], "stateMutability": "nonpayable", "type": "function"}]'
)


class Erc20Token(BaseToken):
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

    def __init__(
        self,
        address: str,
        abi: List[Any] | None = None,
        oracle_address: str | None = None,
        silent: bool = False,
    ) -> None:
        self.address: ChecksumAddress = to_checksum_address(address)
        self.abi = abi if abi is not None else ERC20_ABI_MINIMAL

        _w3 = config.get_web3()
        _w3_contract = self._w3_contract

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
        except AttributeError:
            if not _w3.eth.get_code(self.address):  # pragma: no cover
                raise ValueError("No contract deployed at this address")
            self.name = "Unknown"
            self.name = self.name.strip("\x00")
            logger.warning(
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
            logger.warning(
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
            logger.warning(
                f"Token contract at {self.address} does not implement a 'decimals' function. Setting to 0."
            )

        self.price: float | None = None
        if oracle_address:
            self._price_oracle = ChainlinkPriceContract(address=oracle_address)
            self.price = self._price_oracle.price
        else:
            self.price = None

        AllTokens(chain_id=_w3.eth.chain_id)[self.address] = self

        self._cached_approval: Dict[Tuple[int, ChecksumAddress, ChecksumAddress], int] = {}
        self._cached_balance: Dict[Tuple[int, ChecksumAddress], int] = {}
        self._cached_total_supply: Dict[int, int] = {}

        if not silent:  # pragma: no cover
            logger.info(f"• {self.symbol} ({self.name})")

    def __repr__(self) -> str:  # pragma: no cover
        return f"Erc20Token(address={self.address}, symbol='{self.symbol}', name='{self.name}', decimals={self.decimals})"

    @property
    def _w3_contract(self) -> Contract:
        return config.get_web3().eth.contract(
            address=self.address,
            abi=self.abi,
        )

    def _get_approval_cachable(
        self,
        owner: ChecksumAddress,
        spender: ChecksumAddress,
        block_number: int,
    ) -> int:
        try:
            return self._cached_approval[block_number, owner, spender]
        except KeyError:
            pass

        approval: int
        approval, *_ = eth_abi.abi.decode(
            types=["uint256"],
            data=config.get_web3().eth.call(
                transaction={
                    "to": self.address,
                    "data": Web3.keccak(text="allowance(address,address)")[:4]
                    + eth_abi.abi.encode(types=["address", "address"], args=[owner, spender]),
                },
                block_identifier=block_number,
            ),
        )
        self._cached_approval[block_number, owner, spender] = approval
        return approval

    def get_approval(
        self,
        owner: AnyAddress,
        spender: AnyAddress,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """
        Retrieve the amount that can be spent by `spender` on behalf of `owner`.
        """

        return self._get_approval_cachable(
            to_checksum_address(owner),
            to_checksum_address(spender),
            block_number=get_number_for_block_identifier(block_identifier),
        )

    def _get_balance_cachable(
        self,
        address: ChecksumAddress,
        block_number: int,
    ) -> int:
        try:
            return self._cached_balance[block_number, address]
        except KeyError:
            pass

        balance: int
        balance, *_ = eth_abi.abi.decode(
            types=["uint256"],
            data=config.get_web3().eth.call(
                transaction={
                    "to": self.address,
                    "data": Web3.keccak(text="balanceOf(address)")[:4]
                    + eth_abi.abi.encode(types=["address"], args=[address]),
                },
                block_identifier=block_number,
            ),
        )
        self._cached_balance[block_number, address] = balance
        return balance

    def get_balance(
        self,
        address: AnyAddress,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """
        Retrieve the ERC-20 balance for the given address.
        """
        return self._get_balance_cachable(
            address=to_checksum_address(address),
            block_number=get_number_for_block_identifier(block_identifier),
        )

    def _get_total_supply_cachable(self, block_number: int) -> int:
        try:
            return self._cached_total_supply[block_number]
        except KeyError:
            pass

        total_supply: int
        total_supply, *_ = eth_abi.abi.decode(
            types=["uint256"],
            data=config.get_web3().eth.call(
                transaction={"to": self.address, "data": Web3.keccak(text="totalSupply()")[:4]},
                block_identifier=block_number,
            ),
        )
        self._cached_total_supply[block_number] = total_supply
        return total_supply

    def get_total_supply(self, block_identifier: BlockIdentifier | None = None) -> int:
        """
        Retrieve the total supply for this token.
        """

        block_identifier = (
            config.get_web3().eth.get_block_number()
            if block_identifier is None
            else block_identifier
        )

        return self._get_total_supply_cachable(
            block_number=get_number_for_block_identifier(block_identifier)
        )

    def update_price(self) -> None:
        self._price_oracle.update_price()
        self.price = self._price_oracle.price


class EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE(Erc20Token):
    """
    An adapter for pools using the 'all Es' placeholder address to represent native Ether.
    """

    address = to_checksum_address("0xEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE")
    symbol = "ETH"
    name = "Ether Placeholder"
    decimals = 18

    def __init__(self) -> None:
        self._cached_balance: Dict[Tuple[int, ChecksumAddress], int] = {}
        AllTokens(chain_id=config.get_web3().eth.chain_id)[self.address] = self

    def _get_balance_cachable(
        self,
        address: ChecksumAddress,
        block_number: int,
    ) -> int:
        try:
            return self._cached_balance[block_number, address]
        except KeyError:
            pass

        balance = config.get_web3().eth.get_balance(
            address,
            block_identifier=block_number,
        )
        self._cached_balance[block_number, address] = balance
        return balance

    def get_balance(
        self,
        address: AnyAddress,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        return self._get_balance_cachable(
            address=to_checksum_address(address),
            block_number=get_number_for_block_identifier(block_identifier),
        )
