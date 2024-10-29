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

from degenbot.chainlink import ChainlinkPriceContract
from degenbot.config import connection_manager
from degenbot.exceptions import DegenbotValueError, NoPriceOracle
from degenbot.functions import encode_function_calldata, get_number_for_block_identifier, raw_call
from degenbot.logging import logger
from degenbot.registry.all_tokens import token_registry
from degenbot.types import AbstractErc20Token, BoundedCache


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

            (name,) = eth_abi.abi.decode(types=["string"], data=cast(HexBytes, name))
            (symbol,) = eth_abi.abi.decode(types=["string"], data=cast(HexBytes, symbol))
            (decimals,) = eth_abi.abi.decode(types=["uint256"], data=cast(HexBytes, decimals))

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
            (name,) = eth_abi.abi.decode(types=["string"], data=result)
            return cast(str, name)
        except DecodingError:
            (name,) = eth_abi.abi.decode(types=["bytes32"], data=result)
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
            (symbol,) = eth_abi.abi.decode(types=["string"], data=result)
            return cast(str, symbol)
        except DecodingError:
            (symbol,) = eth_abi.abi.decode(types=["bytes32"], data=result)
            return cast(HexBytes, symbol).decode("utf-8", errors="ignore").strip("\x00")

    def get_decimals(self, w3: Web3, func_prototype: str) -> int:
        (result,) = raw_call(
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
        *,
        chain_id: int | None = None,
        oracle_address: str | None = None,
        silent: bool = False,
    ) -> None:
        self.address = to_checksum_address(address)

        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id
        w3 = self.w3

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
            raise DegenbotValueError(message="No contract deployed at this address") from None

        self._price_oracle: ChainlinkPriceContract | None
        self._price_oracle = (
            ChainlinkPriceContract(address=oracle_address, chain_id=self.chain_id)
            if oracle_address
            else None
        )

        token_registry.add(token_address=self.address, chain_id=self.chain_id, token=self)

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

        (approval,) = eth_abi.abi.decode(
            types=["uint256"],
            data=self.w3.eth.call(
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

        block_number = (
            get_number_for_block_identifier(block_identifier, self.w3)
            if block_identifier is None
            else cast(int, block_identifier)
        )

        return self._get_approval_cachable(
            to_checksum_address(owner),
            to_checksum_address(spender),
            block_number=block_number,
        )

    def _get_balance_cachable(
        self,
        address: ChecksumAddress,
        block_number: int,
    ) -> int:
        with contextlib.suppress(KeyError):
            return self._cached_balance[block_number, address]

        (balance,) = eth_abi.abi.decode(
            types=["uint256"],
            data=self.w3.eth.call(
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

        block_number = (
            get_number_for_block_identifier(block_identifier, self.w3)
            if block_identifier is None
            else cast(int, block_identifier)
        )

        return self._get_balance_cachable(
            address=to_checksum_address(address),
            block_number=block_number,
        )

    def _get_total_supply_cachable(self, block_number: int) -> int:
        with contextlib.suppress(KeyError):
            return self._cached_total_supply[block_number]

        (total_supply,) = eth_abi.abi.decode(
            types=["uint256"],
            data=self.w3.eth.call(
                transaction={
                    "to": self.address,
                    "data": Web3.keccak(text="totalSupply()")[:4],
                },
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

        block_number = (
            get_number_for_block_identifier(
                block_identifier,
                self.w3,
            )
            if block_identifier is None
            else cast(int, block_identifier)
        )

        return self._get_total_supply_cachable(block_number=block_number)

    @property
    def chain_id(self) -> int:
        return self._chain_id

    @property
    def price(self) -> float:
        if self._price_oracle is None:
            raise NoPriceOracle
        return self._price_oracle.price

    @property
    def w3(self) -> Web3:
        return connection_manager.get_web3(self.chain_id)


class EtherPlaceholder(Erc20Token):
    """
    An adapter for pools using the 'all Es' placeholder address to represent native Ether.
    """

    address = to_checksum_address("0xEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE")
    symbol = "ETH"
    name = "Ether Placeholder"
    decimals = 18

    def __init__(
        self,
        *,
        chain_id: int | None = None,
    ) -> None:
        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id
        self._cached_balance: dict[tuple[int, ChecksumAddress], int] = {}
        token_registry.add(token_address=self.address, chain_id=self._chain_id, token=self)

    def _get_balance_cachable(
        self,
        address: ChecksumAddress,
        block_number: int,
    ) -> int:
        with contextlib.suppress(KeyError):
            return self._cached_balance[block_number, address]

        balance = self.w3.eth.get_balance(
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
        block_number = (
            get_number_for_block_identifier(block_identifier, self.w3)
            if block_identifier is None
            else cast(int, block_identifier)
        )
        return self._get_balance_cachable(
            address=to_checksum_address(address),
            block_number=block_number,
        )
