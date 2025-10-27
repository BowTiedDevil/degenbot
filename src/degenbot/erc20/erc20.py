import contextlib
from typing import TYPE_CHECKING, cast

import eth_abi.abi
from eth_abi.exceptions import DecodingError
from eth_typing import ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session, scoped_session
from web3 import AsyncBaseProvider, AsyncWeb3, Web3
from web3.exceptions import Web3Exception
from web3.types import BlockIdentifier, TxParams

from degenbot.chainlink import ChainlinkPriceContract
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import async_connection_manager, connection_manager
from degenbot.database import db_session
from degenbot.database.models import Erc20TokenTable
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.erc20 import NoPriceOracle
from degenbot.functions import (
    encode_function_calldata,
    get_number_for_block_identifier,
    get_number_for_block_identifier_async,
    raw_call,
)
from degenbot.logging import logger
from degenbot.registry import token_registry
from degenbot.types.abstract import AbstractErc20Token
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import BoundedCache

if TYPE_CHECKING:
    from hexbytes import HexBytes


def get_token_from_database(
    token: ChecksumAddress,
    chain_id: int,
    session: Session | scoped_session[Session] = db_session,
) -> Erc20TokenTable | None:
    return session.scalar(
        select(Erc20TokenTable).where(
            Erc20TokenTable.address == token,
            Erc20TokenTable.chain == chain_id,
        )
    )


def set_token_decimals(
    token: Erc20TokenTable,
    decimals: int,
    session: Session | scoped_session[Session] = db_session,
) -> None:
    token.decimals = decimals
    session.commit()


def set_token_name(
    token: Erc20TokenTable,
    name: str,
    session: Session | scoped_session[Session] = db_session,
) -> None:
    token.name = name
    session.commit()


def set_token_symbol(
    token: Erc20TokenTable,
    symbol: str,
    session: Session | scoped_session[Session] = db_session,
) -> None:
    token.symbol = symbol
    session.commit()


class Erc20Token(AbstractErc20Token):
    """
    An ERC-20 token contract.
    """

    UNKNOWN_NAME = "Unknown"
    UNKNOWN_SYMBOL = "UNKN"
    UNKNOWN_DECIMALS = 18

    def __init__(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        oracle_address: str | None = None,
        silent: bool = False,
        state_cache_depth: int = 8,
    ) -> None:
        self.address = get_checksum_address(address)
        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id

        token_from_db = get_token_from_database(
            token=self.address,
            chain_id=self.chain_id,
        )

        self.decimals = self.UNKNOWN_DECIMALS
        self.name = self.UNKNOWN_NAME
        self.symbol = self.UNKNOWN_SYMBOL

        # Attempt to load values from the DB
        if token_from_db is not None:
            if token_from_db.decimals is not None:
                self.decimals = token_from_db.decimals
            if token_from_db.name is not None:
                self.name = token_from_db.name
            if token_from_db.symbol is not None:
                self.symbol = token_from_db.symbol

        # Look up values from the contract if all defaults are still set
        if (
            self.decimals == self.UNKNOWN_DECIMALS
            and self.name == self.UNKNOWN_NAME
            and self.symbol == self.UNKNOWN_SYMBOL
        ):
            w3 = self.w3

            if not w3.eth.get_code(self.address):
                raise DegenbotValueError(message="No contract deployed at this address")

            try:
                self.name, self.symbol, self.decimals = self.get_name_symbol_decimals_batched(w3=w3)
            except (Web3Exception, DecodingError):
                for func_prototype in ("name()", "NAME()"):
                    try:
                        self.name = self.get_name(w3=w3, func_prototype=func_prototype)
                    except (Web3Exception, DecodingError):
                        continue
                    else:
                        break

                for func_prototype in ("symbol()", "SYMBOL()"):
                    try:
                        self.symbol = self.get_symbol(w3=w3, func_prototype=func_prototype)
                    except (Web3Exception, DecodingError):
                        continue
                    else:
                        break

                for func_prototype in ("decimals()", "DECIMALS()"):
                    try:
                        self.decimals = self.get_decimals(w3=w3, func_prototype=func_prototype)
                    except (Web3Exception, DecodingError):
                        continue
                    else:
                        break

            if token_from_db is not None:
                if self.decimals != self.UNKNOWN_DECIMALS:
                    set_token_decimals(token=token_from_db, decimals=self.decimals)
                if self.name != self.UNKNOWN_NAME:
                    set_token_name(token=token_from_db, name=self.name)
                if self.symbol != self.UNKNOWN_SYMBOL:
                    set_token_symbol(token=token_from_db, symbol=self.symbol)

        self._price_oracle = (
            ChainlinkPriceContract(address=oracle_address, chain_id=self.chain_id)
            if oracle_address
            else None
        )

        token_registry.add(token_address=self.address, chain_id=self.chain_id, token=self)

        self._state_cache_depth = state_cache_depth
        self._cached_approval: dict[tuple[int, ChecksumAddress, ChecksumAddress], int] = {}
        self._cached_balance: dict[ChecksumAddress, BoundedCache[BlockNumber, int]] = {}
        self._cached_total_supply: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth,
        )

        if not silent:  # pragma: no cover
            logger.info(f"• {self.symbol} ({self.name})")

    def __repr__(self) -> str:  # pragma: no cover
        return f"{self.__class__.__name__}(address={self.address}, symbol='{self.symbol}', name='{self.name}', decimals={self.decimals})"  # noqa:E501

    def get_name_symbol_decimals_batched(self, w3: Web3) -> tuple[str, str, int]:
        with w3.batch_requests() as batch:
            batch.add_mapping(
                {
                    w3.eth.call: [
                        TxParams(
                            to=self.address,
                            data=encode_function_calldata(
                                function_prototype="name()",
                                function_arguments=None,
                            ),
                        ),
                        TxParams(
                            to=self.address,
                            data=encode_function_calldata(
                                function_prototype="symbol()",
                                function_arguments=None,
                            ),
                        ),
                        TxParams(
                            to=self.address,
                            data=encode_function_calldata(
                                function_prototype="decimals()",
                                function_arguments=None,
                            ),
                        ),
                    ]
                }
            )

            name, symbol, decimals = batch.execute()

            (name,) = eth_abi.abi.decode(types=["string"], data=cast("HexBytes", name))
            (symbol,) = eth_abi.abi.decode(types=["string"], data=cast("HexBytes", symbol))
            (decimals,) = eth_abi.abi.decode(types=["uint256"], data=cast("HexBytes", decimals))

            return cast("str", name), cast("str", symbol), cast("int", decimals)

    def get_name(self, w3: Web3, func_prototype: str) -> str:
        result = w3.eth.call(
            TxParams(
                to=self.address,
                data=encode_function_calldata(
                    function_prototype=func_prototype,
                    function_arguments=None,
                ),
            )
        )

        try:
            (name,) = eth_abi.abi.decode(types=["string"], data=result)
            return cast("str", name)
        except DecodingError:
            (name,) = eth_abi.abi.decode(types=["bytes32"], data=result)
            return cast("HexBytes", name).decode("utf-8", errors="ignore").strip("\x00")

    def get_symbol(self, w3: Web3, func_prototype: str) -> str:
        result = w3.eth.call(
            TxParams(
                to=self.address,
                data=encode_function_calldata(
                    function_prototype=func_prototype,
                    function_arguments=None,
                ),
            )
        )

        try:
            (symbol,) = eth_abi.abi.decode(types=["string"], data=result)
            return cast("str", symbol)
        except DecodingError:
            (symbol,) = eth_abi.abi.decode(types=["bytes32"], data=result)
            return cast("HexBytes", symbol).decode("utf-8", errors="ignore").strip("\x00")

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
        return cast("int", result)

    def get_approval(
        self,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """
        Retrieve the amount that can be spent by `spender` on behalf of `owner`.
        """

        owner = get_checksum_address(owner)
        spender = get_checksum_address(spender)

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else get_number_for_block_identifier(
                block_identifier,
                self.w3,
            )
        )

        with contextlib.suppress(KeyError):
            return self._cached_approval[block_number, owner, spender]

        approval: int
        (approval,) = eth_abi.abi.decode(
            types=["uint256"],
            data=self.w3.eth.call(
                transaction=TxParams(
                    to=self.address,
                    data=Web3.keccak(text="allowance(address,address)")[:4]
                    + eth_abi.abi.encode(types=["address", "address"], args=[owner, spender]),
                ),
                block_identifier=block_number,
            ),
        )
        self._cached_approval[block_number, owner, spender] = approval
        return approval

    async def get_approval_async(
        self,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """
        Retrieve the amount that can be spent by `spender` on behalf of `owner`.
        """

        owner = get_checksum_address(owner)
        spender = get_checksum_address(spender)

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else await get_number_for_block_identifier_async(
                block_identifier,
                self.async_w3,
            )
        )

        with contextlib.suppress(KeyError):
            return self._cached_approval[block_number, owner, spender]

        approval: int
        (approval,) = eth_abi.abi.decode(
            types=["uint256"],
            data=await self.async_w3.eth.call(
                transaction=TxParams(
                    to=self.address,
                    data=Web3.keccak(text="allowance(address,address)")[:4]
                    + eth_abi.abi.encode(types=["address", "address"], args=[owner, spender]),
                ),
                block_identifier=block_number,
            ),
        )
        self._cached_approval[block_number, owner, spender] = approval
        return approval

    def get_balance(
        self,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """
        Retrieve the ERC-20 balance for the given address.
        """

        address = get_checksum_address(address)

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else get_number_for_block_identifier(
                block_identifier,
                self.w3,
            )
        )

        with contextlib.suppress(KeyError):
            return self._cached_balance[address][block_number]

        balance: int
        (balance,) = eth_abi.abi.decode(
            types=["uint256"],
            data=self.w3.eth.call(
                transaction=TxParams(
                    to=self.address,
                    data=Web3.keccak(text="balanceOf(address)")[:4]
                    + eth_abi.abi.encode(types=["address"], args=[address]),
                ),
                block_identifier=block_number,
            ),
        )

        if address not in self._cached_balance:
            self._cached_balance[address] = BoundedCache(max_items=self._state_cache_depth)

        self._cached_balance[address][block_number] = balance
        return balance

    async def get_balance_async(
        self,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """
        Retrieve the ERC-20 balance for the given address.
        """

        address = get_checksum_address(address)

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else await get_number_for_block_identifier_async(
                block_identifier,
                self.async_w3,
            )
        )

        with contextlib.suppress(KeyError):
            return self._cached_balance[address][block_number]

        balance: int
        (balance,) = eth_abi.abi.decode(
            types=["uint256"],
            data=await self.async_w3.eth.call(
                transaction=TxParams(
                    to=self.address,
                    data=Web3.keccak(text="balanceOf(address)")[:4]
                    + eth_abi.abi.encode(types=["address"], args=[address]),
                ),
                block_identifier=block_number,
            ),
        )

        if address not in self._cached_balance:
            self._cached_balance[address] = BoundedCache(max_items=self._state_cache_depth)

        self._cached_balance[address][block_number] = balance
        return balance

    def get_total_supply(self, block_identifier: BlockIdentifier | None = None) -> int:
        """
        Retrieve the total supply for this token.
        """

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else get_number_for_block_identifier(
                block_identifier,
                self.w3,
            )
        )

        with contextlib.suppress(KeyError):
            return self._cached_total_supply[block_number]

        total_supply: int
        (total_supply,) = eth_abi.abi.decode(
            types=["uint256"],
            data=self.w3.eth.call(
                transaction=TxParams(
                    to=self.address,
                    data=Web3.keccak(text="totalSupply()")[:4],
                ),
                block_identifier=block_number,
            ),
        )
        self._cached_total_supply[block_number] = total_supply
        return total_supply

    async def get_total_supply_async(self, block_identifier: BlockIdentifier | None = None) -> int:
        """
        Retrieve the total supply for this token.
        """

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else await get_number_for_block_identifier_async(
                block_identifier,
                self.async_w3,
            )
        )

        with contextlib.suppress(KeyError):
            return self._cached_total_supply[block_number]

        total_supply: int
        (total_supply,) = eth_abi.abi.decode(
            types=["uint256"],
            data=await self.async_w3.eth.call(
                transaction=TxParams(
                    to=self.address,
                    data=Web3.keccak(text="totalSupply()")[:4],
                ),
                block_identifier=block_number,
            ),
        )
        self._cached_total_supply[block_number] = total_supply
        return total_supply

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

    @property
    def async_w3(self) -> AsyncWeb3[AsyncBaseProvider]:
        return async_connection_manager.get_web3(self.chain_id)
