from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, cast

import eth_abi.abi
import sqlalchemy.exc
from eth_abi.exceptions import DecodingError
from web3 import Web3
from web3.exceptions import Web3Exception

from degenbot.checksum_cache import get_checksum_address
from degenbot.connection.connection_manager import ConnectionManager
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.erc20 import EtherPlaceholder
from degenbot.erc20.erc20 import (
    UNKNOWN_DECIMALS,
    UNKNOWN_NAME,
    UNKNOWN_SYMBOL,
    Erc20Token,
    get_token_from_database,
)
from degenbot.exceptions.base import DegenbotValueError
from degenbot.logging import logger
from degenbot.provider.interface import ProviderAdapter
from degenbot.registry import TokenRegistry

if TYPE_CHECKING:
    from web3.types import BlockIdentifier

    from degenbot.types.aliases import ChainId


class Erc20Builder:
    """
    Builds Erc20Token instances from DB lookups and RPC calls.

    Owns the full I/O choreography: check registry → check DB → fetch
    from chain → construct token → register.
    """

    def __init__(
        self,
        *,
        connections: ConnectionManager,
        db: DatabaseSessionManager,
        tokens: TokenRegistry,
    ) -> None:
        self._connections = connections
        self._db = db
        self._tokens = tokens

    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        silent: bool = False,
    ) -> Erc20Token:
        """Fetch token metadata from DB/RPC and construct an I/O-free Erc20Token."""

        address = get_checksum_address(address)
        chain_id = chain_id or self._connections.default_chain_id

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
            provider = self._connections.get_provider(chain_id)

            if not provider.get_code(address):
                raise DegenbotValueError(message="No contract deployed at this address")

            try:
                fetched_name, fetched_symbol, fetched_decimals = (
                    Erc20Token.fetch_name_symbol_decimals_batched(
                        address=address, provider=provider
                    )
                )
            except (Web3Exception, DecodingError):
                # Fallback: try individual calls with alternate prototypes
                for func_prototype in ("name()", "NAME()"):
                    try:
                        fetched_name = Erc20Token.fetch_name(
                            address=address, provider=provider, func_prototype=func_prototype
                        )
                        break
                    except (Web3Exception, DecodingError):
                        continue
                else:
                    fetched_name = UNKNOWN_NAME

                for func_prototype in ("symbol()", "SYMBOL()"):
                    try:
                        fetched_symbol = Erc20Token.fetch_symbol(
                            address=address, provider=provider, func_prototype=func_prototype
                        )
                        break
                    except (Web3Exception, DecodingError):
                        continue
                else:
                    fetched_symbol = UNKNOWN_SYMBOL

                for func_prototype in ("decimals()", "DECIMALS()"):
                    try:
                        fetched_decimals = Erc20Token.fetch_decimals(
                            address=address, provider=provider, func_prototype=func_prototype
                        )
                        break
                    except (Web3Exception, DecodingError):
                        continue
                else:
                    fetched_decimals = UNKNOWN_DECIMALS

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

    def get_token_balance(
        self,
        token: Erc20Token,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the ERC-20 balance for the given address."""

        address = get_checksum_address(address)
        assert token.chain_id is not None
        provider = self._connections.get_provider(token.chain_id)

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else self._resolve_block_number(provider, block_identifier)
        )

        # Check cache
        if (balance := token.get_cached_balance(address, block_number)) is not None:
            return balance

        (balance,) = eth_abi.abi.decode(
            types=["uint256"],
            data=provider.call(
                to=token.address,
                data=Web3.keccak(text="balanceOf(address)")[:4]
                + eth_abi.abi.encode(types=["address"], args=[address]),
                block=block_number,
            ),
        )

        token.set_cached_balance(address, block_number, cast("int", balance))
        return cast("int", balance)

    def get_token_approval(
        self,
        token: Erc20Token,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the amount that can be spent by `spender` on behalf of `owner`."""

        owner = get_checksum_address(owner)
        spender = get_checksum_address(spender)
        assert token.chain_id is not None
        provider = self._connections.get_provider(token.chain_id)

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else self._resolve_block_number(provider, block_identifier)
        )

        # Check cache
        if (approval := token.get_cached_approval(block_number, owner, spender)) is not None:
            return approval

        (approval,) = eth_abi.abi.decode(
            types=["uint256"],
            data=provider.call(
                to=token.address,
                data=Web3.keccak(text="allowance(address,address)")[:4]
                + eth_abi.abi.encode(types=["address", "address"], args=[owner, spender]),
                block=block_number,
            ),
        )

        token.set_cached_approval(block_number, owner, spender, cast("int", approval))
        return cast("int", approval)

    def get_token_total_supply(
        self,
        token: Erc20Token,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the total supply for this token."""

        assert token.chain_id is not None
        provider = self._connections.get_provider(token.chain_id)

        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else self._resolve_block_number(provider, block_identifier)
        )

        # Check cache
        if (total_supply := token.get_cached_total_supply(block_number)) is not None:
            return total_supply

        (total_supply,) = eth_abi.abi.decode(
            types=["uint256"],
            data=provider.call(
                to=token.address,
                data=Web3.keccak(text="totalSupply()")[:4],
                block=block_number,
            ),
        )
        total_supply = int(total_supply)

        token.set_cached_total_supply(block_number, total_supply)
        return total_supply

    def get_ether_balance(
        self,
        chain_id: ChainId,
        address: str,
        block_identifier: BlockIdentifier | None = None,
    ) -> int:
        """Retrieve the native ETH balance for the given address."""
        address = get_checksum_address(address)
        provider = self._connections.get_provider(chain_id)
        block_number = (
            block_identifier
            if isinstance(block_identifier, int)
            else self._resolve_block_number(provider, block_identifier)
        )
        return provider.get_balance(address, block=block_number)

    @staticmethod
    def _resolve_block_number(
        provider: ProviderAdapter, block_identifier: BlockIdentifier | None
    ) -> int:
        """Resolve a block identifier to a block number."""
        if block_identifier is None:
            return provider.get_block_number()
        if isinstance(block_identifier, int):
            return block_identifier
        # For string identifiers like 'latest', 'earliest', 'pending'
        return provider.get_block_number()
