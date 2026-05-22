"""Async builder for ERC-20 tokens."""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING

import eth_abi.abi
import sqlalchemy.exc
from eth_abi.exceptions import DecodingError
from web3.exceptions import Web3Exception

from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import EtherPlaceholder
from degenbot.erc20.erc20 import (
    UNKNOWN_DECIMALS,
    UNKNOWN_NAME,
    UNKNOWN_SYMBOL,
    Erc20Token,
    get_token_from_database,
)
from degenbot.logging import logger
from degenbot.provider.call_helpers import encode_function_calldata

if TYPE_CHECKING:
    from web3.types import BlockIdentifier

    from degenbot.builders.pool_io import AsyncPoolIO
    from degenbot.database.session_manager import DatabaseSessionManager
    from degenbot.registry import TokenRegistry
    from degenbot.types.aliases import ChainId


class AsyncErc20Builder:
    """Async counterpart of Erc20Builder.

    Builds Erc20Token instances using AsyncPoolIO for I/O.
    Shares pure decode/register logic with Erc20Builder.
    """

    def __init__(
        self,
        *,
        default_chain_id: ChainId | None = None,
        db: DatabaseSessionManager,
        tokens: TokenRegistry,
    ) -> None:
        self._default_chain_id = default_chain_id
        self._db = db
        self._tokens = tokens

    async def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        silent: bool = False,
        io: AsyncPoolIO | None = None,
    ) -> Erc20Token:
        """Fetch token metadata from DB/RPC and construct an I/O-free Erc20Token."""

        address = get_checksum_address(address)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"

        # Check registry first
        if (existing := self._tokens.get(token_address=address, chain_id=chain_id)) is not None:
            return existing

        # Check for Ether placeholder
        if address in EtherPlaceholder.addresses:
            token: Erc20Token = EtherPlaceholder(address, chain_id=chain_id)
            self._tokens.add(token_address=token.address, chain_id=chain_id, token=token)
            if not silent:
                logger.info(f"• {token.symbol} ({token.name})")
            return token

        # Try DB first
        token_from_db = None
        with contextlib.suppress(Exception), self._db() as session:
            token_from_db = get_token_from_database(
                token=address,
                chain_id=chain_id,
                session=session,
            )

        name: str | None = None
        symbol: str | None = None
        decimals: int | None = None

        if token_from_db is not None:
            if token_from_db.name is not None:
                name = token_from_db.name
            if token_from_db.symbol is not None:
                symbol = token_from_db.symbol
            if token_from_db.decimals is not None:
                decimals = token_from_db.decimals

        # Fetch missing values from chain
        if name is None or symbol is None or decimals is None:
            assert io is not None

            try:
                (
                    fetched_name,
                    fetched_symbol,
                    fetched_decimals,
                ) = await self._fetch_name_symbol_decimals_batched(address=address, io=io)
            except (Web3Exception, DecodingError):
                # Fallback: try individual calls with alternate prototypes
                fetched_name = await self._fetch_with_fallback(
                    address, io, ["name()", "NAME()"], UNKNOWN_NAME
                )
                fetched_symbol = await self._fetch_with_fallback(
                    address, io, ["symbol()", "SYMBOL()"], UNKNOWN_SYMBOL
                )
                fetched_decimals = await self._fetch_with_fallback_int(
                    address, io, ["decimals()", "DECIMALS()"], UNKNOWN_DECIMALS
                )

            name = name or fetched_name
            symbol = symbol or fetched_symbol
            decimals = decimals or fetched_decimals

            # Write back to DB if the record exists but was missing data
            if (
                token_from_db is not None
                and token_from_db.name is None
                and token_from_db.symbol is None
                and token_from_db.decimals is None
            ):
                with contextlib.suppress(sqlalchemy.exc.SQLAlchemyError), self._db() as session:
                    token_from_db.decimals = decimals
                    token_from_db.name = name
                    token_from_db.symbol = symbol
                    session.commit()

        token = Erc20Token(
            address=address,
            chain_id=chain_id,
            name=name,
            symbol=symbol,
            decimals=decimals,
        )

        # Register (no self-registration)
        self._tokens.add(token_address=token.address, chain_id=chain_id, token=token)

        if not silent:
            logger.info(f"• {token.symbol} ({token.name})")

        return token

    @staticmethod
    async def _fetch_name_symbol_decimals_batched(
        address: str,
        io: AsyncPoolIO,
    ) -> tuple[str, str, int]:
        """Fetch name, symbol, decimals from chain in three async calls."""
        name_result = await io.call(
            to=address,
            data=encode_function_calldata("name()", None),
        )
        symbol_result = await io.call(
            to=address,
            data=encode_function_calldata("symbol()", None),
        )
        decimals_result = await io.call(
            to=address,
            data=encode_function_calldata("decimals()", None),
        )

        (name,) = eth_abi.abi.decode(types=["string"], data=name_result)
        (symbol,) = eth_abi.abi.decode(types=["string"], data=symbol_result)
        (decimals,) = eth_abi.abi.decode(types=["uint8"], data=decimals_result)

        return name, symbol, decimals

    @staticmethod
    async def _fetch_with_fallback(
        address: str,
        io: AsyncPoolIO,
        prototypes: list[str],
        default: str,
    ) -> str:
        """Fetch a string field with fallback prototypes."""
        for prototype in prototypes:
            try:
                result = await io.call(
                    to=address,
                    data=encode_function_calldata(prototype, None),
                )
                (value,) = eth_abi.abi.decode(types=["string"], data=result)
                return value  # noqa: TRY300
            except (Web3Exception, DecodingError):
                continue
        return default

    @staticmethod
    async def _fetch_with_fallback_int(
        address: str,
        io: AsyncPoolIO,
        prototypes: list[str],
        default: int,
    ) -> int:
        """Fetch an integer field with fallback prototypes."""
        for prototype in prototypes:
            try:
                result = await io.call(
                    to=address,
                    data=encode_function_calldata(prototype, None),
                )
                (value,) = eth_abi.abi.decode(types=["uint8"], data=result)
                return value  # noqa: TRY300
            except (Web3Exception, DecodingError):
                continue
        return default

    async def get_token_balance(  # noqa: PLR6301
        self,
        token: Erc20Token,
        address: str,
        block_identifier: BlockIdentifier | None = None,
        *,
        io: AsyncPoolIO | None = None,
    ) -> int:
        """Retrieve the ERC-20 balance for the given address."""
        address = get_checksum_address(address)
        assert token.chain_id is not None
        assert io is not None

        block_number = await _resolve_block_number(io, block_identifier)

        # Check cache
        if (balance := token.get_cached_balance(address, block_number)) is not None:
            return balance

        (balance,) = eth_abi.abi.decode(
            types=["uint256"],
            data=await io.call(
                to=token.address,
                data=encode_function_calldata("balanceOf(address)", [address]),
                block=block_number,
            ),
        )

        token.set_cached_balance(address, block_number, int(balance))
        return int(balance)

    async def get_token_approval(  # noqa: PLR6301
        self,
        token: Erc20Token,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
        *,
        io: AsyncPoolIO | None = None,
    ) -> int:
        """Retrieve the amount that can be spent by `spender` on behalf of `owner`."""
        owner = get_checksum_address(owner)
        spender = get_checksum_address(spender)
        assert token.chain_id is not None
        assert io is not None

        block_number = await _resolve_block_number(io, block_identifier)

        # Check cache
        if (approval := token.get_cached_approval(block_number, owner, spender)) is not None:
            return approval

        (approval,) = eth_abi.abi.decode(
            types=["uint256"],
            data=await io.call(
                to=token.address,
                data=encode_function_calldata("allowance(address,address)", [owner, spender]),
                block=block_number,
            ),
        )

        token.set_cached_approval(block_number, owner, spender, int(approval))
        return int(approval)

    async def get_token_total_supply(  # noqa: PLR6301
        self,
        token: Erc20Token,
        block_identifier: BlockIdentifier | None = None,
        *,
        io: AsyncPoolIO | None = None,
    ) -> int:
        """Retrieve the total supply for this token."""
        assert token.chain_id is not None
        assert io is not None

        block_number = await _resolve_block_number(io, block_identifier)

        # Check cache
        if (total_supply := token.get_cached_total_supply(block_number)) is not None:
            return total_supply

        (total_supply,) = eth_abi.abi.decode(
            types=["uint256"],
            data=await io.call(
                to=token.address,
                data=encode_function_calldata("totalSupply()", None),
                block=block_number,
            ),
        )

        token.set_cached_total_supply(block_number, int(total_supply))
        return int(total_supply)

    async def get_ether_balance(  # noqa: PLR6301
        self,
        chain_id: ChainId,  # noqa: ARG002
        address: str,
        block_identifier: BlockIdentifier | None = None,
        *,
        io: AsyncPoolIO | None = None,
    ) -> int:
        """Retrieve the native ETH balance for the given address."""
        address = get_checksum_address(address)
        assert io is not None

        block_number = await _resolve_block_number(io, block_identifier)
        return await io.get_balance(address, block=block_number)


async def _resolve_block_number(io: AsyncPoolIO, block_identifier: BlockIdentifier | None) -> int:
    """Resolve a block identifier to a block number."""
    if block_identifier is None:
        return await io.get_block_number()
    if isinstance(block_identifier, int):
        return block_identifier
    return await io.get_block_number()
