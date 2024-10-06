import contextlib
from typing import TYPE_CHECKING, cast

import eth_abi.abi
from eth_abi.exceptions import DecodingError
from eth_typing import AnyAddress, ChecksumAddress
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes
from web3 import Web3
from web3.exceptions import Web3Exception
from web3.types import BlockIdentifier

from . import config
from .chainlink import ChainlinkPriceContract
from .exceptions import DegenbotValueError, NoPriceOracle
from .functions import encode_function_calldata, get_number_for_block_identifier, raw_call
from .logging import logger
from .registry.all_tokens import AllTokens
from .types import AbstractErc20Token


class Erc20Token(AbstractErc20Token):
    """
    An ERC-20 token contract.
    """

    UNKNOWN_NAME = "Unknown"
    UNKNOWN_SYMBOL = "UNKN"
    UNKNOWN_DECIMALS = 0

    def get_name_symbol_decimals_batched(self, w3: Web3) -> tuple[str, str, int]:
        with w3.batch_requests() as batch:
            batch.add_mapping(
                {
                    w3.eth.call: [
                        {
                            "to": self.address,
                            "data": encode_function_calldata(
                                function_prototype="name()",
                                function_arguments=None,
                            ),
                        },
                        {
                            "to": self.address,
                            "data": encode_function_calldata(
                                function_prototype="symbol()",
                                function_arguments=None,
                            ),
                        },
                        {
                            "to": self.address,
                            "data": encode_function_calldata(
                                function_prototype="decimals()",
                                function_arguments=None,
                            ),
                        },
                    ]
                }
            )

            name, symbol, decimals = batch.execute()

            name, *_ = eth_abi.abi.decode(types=["string"], data=cast(HexBytes, name))
            symbol, *_ = eth_abi.abi.decode(types=["string"], data=cast(HexBytes, symbol))
            decimals, *_ = eth_abi.abi.decode(types=["uint256"], data=cast(HexBytes, decimals))

            return cast(str, name), cast(str, symbol), cast(int, decimals)

    def get_name(self, w3: Web3, func_prototype: str) -> str:
        result = w3.eth.call(
            {
                "to": self.address,
                "data": encode_function_calldata(
                    function_prototype=func_prototype,
                    function_arguments=None,
                ),
            }
        )

        try:
            name, *_ = eth_abi.abi.decode(types=["string"], data=result)
            return cast(str, name)
        except DecodingError:
            name, *_ = eth_abi.abi.decode(types=["bytes32"], data=result)
            return cast(HexBytes, name).decode("utf-8", errors="ignore").strip("\x00")

    def get_symbol(self, w3: Web3, func_prototype: str) -> str:
        result = w3.eth.call(
            {
                "to": self.address,
                "data": encode_function_calldata(
                    function_prototype=func_prototype,
                    function_arguments=None,
                ),
            }
        )

        try:
            symbol, *_ = eth_abi.abi.decode(types=["string"], data=result)
            return cast(str, symbol)
        except DecodingError:
            symbol, *_ = eth_abi.abi.decode(types=["bytes32"], data=result)
            return cast(HexBytes, symbol).decode("utf-8", errors="ignore").strip("\x00")

    def get_decimals(self, w3: Web3, func_prototype: str) -> int:
        result, *_ = raw_call(
            w3=w3,
            address=self.address,
            calldata=encode_function_calldata(
                function_prototype=func_prototype,
                function_arguments=None,
            ),
            return_types=["uint256"],
        )
        return cast(int, result)

    def __init__(
        self,
        address: str,
        oracle_address: str | None = None,
        silent: bool = False,
    ) -> None:
        w3 = config.get_web3()
        self.address = to_checksum_address(address)

        self.name = self.UNKNOWN_NAME
        self.symbol = self.UNKNOWN_SYMBOL
        self.decimals = self.UNKNOWN_DECIMALS
        try:
            self.name, self.symbol, self.decimals = self.get_name_symbol_decimals_batched(w3=w3)
        except (Web3Exception, DecodingError):
            for func_prototype in ("name()", "NAME()"):
                try:
                    self.name = self.get_name(w3=w3, func_prototype=func_prototype)
                except (Web3Exception, DecodingError):
                    continue
                break

            for func_prototype in ("symbol()", "SYMBOL()"):
                try:
                    self.symbol = self.get_symbol(w3=w3, func_prototype=func_prototype)
                except (Web3Exception, DecodingError):
                    continue
                break

            for func_prototype in ("decimals()", "DECIMALS()"):
                try:
                    self.decimals = self.get_decimals(w3=w3, func_prototype=func_prototype)
                except (Web3Exception, DecodingError):
                    continue
                break

        if all(
            [
                self.name == self.UNKNOWN_NAME,
                self.symbol == self.UNKNOWN_SYMBOL,
                self.decimals == self.UNKNOWN_DECIMALS,
            ]
        ) and not w3.eth.get_code(self.address):
            raise DegenbotValueError("No contract deployed at this address") from None

        self._price_oracle: ChainlinkPriceContract | None
        self._price_oracle = (
            ChainlinkPriceContract(address=oracle_address) if oracle_address else None
        )

        AllTokens(chain_id=w3.eth.chain_id)[self.address] = self

        self._cached_approval: dict[tuple[int, ChecksumAddress, ChecksumAddress], int] = {}
        self._cached_balance: dict[tuple[int, ChecksumAddress], int] = {}
        self._cached_total_supply: dict[int, int] = {}

        if not silent:  # pragma: no cover
            logger.info(f"• {self.symbol} ({self.name})")

    def __repr__(self) -> str:  # pragma: no cover
        return f"Erc20Token(address={self.address}, symbol='{self.symbol}', name='{self.name}', decimals={self.decimals})"  # noqa:E501

    def _get_approval_cachable(
        self,
        owner: ChecksumAddress,
        spender: ChecksumAddress,
        block_number: int,
    ) -> int:
        with contextlib.suppress(KeyError):
            return self._cached_approval[block_number, owner, spender]

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
        if TYPE_CHECKING:
            assert isinstance(approval, int)
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
        with contextlib.suppress(KeyError):
            return self._cached_balance[block_number, address]

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
        if TYPE_CHECKING:
            assert isinstance(balance, int)
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
        with contextlib.suppress(KeyError):
            return self._cached_total_supply[block_number]

        total_supply, *_ = eth_abi.abi.decode(
            types=["uint256"],
            data=config.get_web3().eth.call(
                transaction={"to": self.address, "data": Web3.keccak(text="totalSupply()")[:4]},
                block_identifier=block_number,
            ),
        )
        if TYPE_CHECKING:
            assert isinstance(total_supply, int)
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

    @property
    def price(self) -> float:
        if self._price_oracle is not None:
            return self._price_oracle.price
        else:  # pragma: no cover
            raise NoPriceOracle(f"{self} does not have a price oracle.")


class EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE(Erc20Token):
    """
    An adapter for pools using the 'all Es' placeholder address to represent native Ether.
    """

    address = to_checksum_address("0xEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE")
    symbol = "ETH"
    name = "Ether Placeholder"
    decimals = 18

    def __init__(self) -> None:
        self._cached_balance: dict[tuple[int, ChecksumAddress], int] = {}
        AllTokens(chain_id=config.get_web3().eth.chain_id)[self.address] = self

    def _get_balance_cachable(
        self,
        address: ChecksumAddress,
        block_number: int,
    ) -> int:
        with contextlib.suppress(KeyError):
            return self._cached_balance[block_number, address]

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
