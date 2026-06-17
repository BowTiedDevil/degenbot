"""ERC-20 token builder that fetches on-chain metadata."""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, cast

import eth_abi.abi
import sqlalchemy.exc
from eth_abi.exceptions import DecodingError
from web3 import Web3
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
from degenbot.exceptions.base import DegenbotValueError
from degenbot.logging import logger
from degenbot.provider.call_helpers import encode_function_calldata

if TYPE_CHECKING:
    from hexbytes import HexBytes
    from web3.types import BlockIdentifier

    from degenbot.builders.pool_io import PoolIO
    from degenbot.database.session_manager import DatabaseSessionManager
    from degenbot.degenbot_rs import PyBot
    from degenbot.registry import TokenRegistry
    from degenbot.types.aliases import ChainId


class Erc20Builder:
    """Builds Erc20Token instances from DB lookups and RPC calls.

    Owns the full I/O choreography: check registry → check DB → fetch
    from chain → construct token → register.
    """

    def __init__(
        self,
        *,
        default_chain_id: ChainId | None = None,
        db: DatabaseSessionManager,
        tokens: TokenRegistry,
        py_bot: PyBot,
    ) -> None:
        """Initialize the instance."""
        self._default_chain_id = default_chain_id
        self._db = db
        self._tokens = tokens
        self._py_bot = py_bot

    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        silent: bool = False,
        io: PoolIO | None = None,
    ) -> Erc20Token:
        """Fetch token metadata from DB/RPC and construct an I/O-free Erc20Token.

        Returns:
            The computed value.

        Raises:
            DegenbotValueError: If the operation fails.

        """
        address = get_checksum_address(address)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"

        # Check registry first
        if (existing := self._tokens.get(token_address=address, chain_id=chain_id)) is not None:
            return existing

        # Check for Ether placeholder
        if address in EtherPlaceholder.addresses:
            py_token = self._py_bot.register_token(
                address, "Ether Placeholder", "ETH", 18, chain_id
            )
            token: Erc20Token = EtherPlaceholder(py_token)
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

            if not io.get_code(address):
                raise DegenbotValueError(message="No contract deployed at this address")

            try:
                fetched_name, fetched_symbol, fetched_decimals = (
                    _fetch_name_symbol_decimals_batched(address=address, io=io)
                )
            except (Web3Exception, DecodingError):
                # Fallback: try individual calls with alternate prototypes
                for func_prototype in ("name()", "NAME()"):
                    try:
                        fetched_name = _fetch_name(
                            address=address, io=io, func_prototype=func_prototype
                        )
                        break
                    except (Web3Exception, DecodingError):
                        continue
                else:
                    fetched_name = UNKNOWN_NAME

                for func_prototype in ("symbol()", "SYMBOL()"):
                    try:
                        fetched_symbol = _fetch_symbol(
                            address=address, io=io, func_prototype=func_prototype
                        )
                        break
                    except (Web3Exception, DecodingError):
                        continue
                else:
                    fetched_symbol = UNKNOWN_SYMBOL

                for func_prototype in ("decimals()", "DECIMALS()"):
                    try:
                        fetched_decimals = _fetch_decimals(
                            address=address, io=io, func_prototype=func_prototype
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

        py_token = self._py_bot.register_token(address, name, symbol, decimals, chain_id)
        token = Erc20Token(py_token)

        # Register (no self-registration)
        self._tokens.add(token_address=token.address, chain_id=chain_id, token=token)

        if not silent:
            logger.info(f"• {token.symbol} ({token.name})")

        return token

    def get_token_balance(  # noqa: PLR6301
        self,
        token: Erc20Token,
        address: str,
        block_identifier: BlockIdentifier | None = None,
        *,
        io: PoolIO | None = None,
    ) -> int:
        """Retrieve the ERC-20 balance for the given address.

        Returns:
            The computed value.

        """
        address = get_checksum_address(address)
        assert token.chain_id is not None
        assert io is not None

        block_number = _resolve_block_number(io, block_identifier)

        # Check cache
        if (balance := token.get_cached_balance(address, block_number)) is not None:
            return balance

        (balance,) = eth_abi.abi.decode(
            types=["uint256"],
            data=io.call(
                to=token.address,
                data=Web3.keccak(text="balanceOf(address)")[:4]
                + eth_abi.abi.encode(types=["address"], args=[address]),
                block=block_number,
            ),
        )

        token.set_cached_balance(address, block_number, cast("int", balance))
        return cast("int", balance)

    def get_token_approval(  # noqa: PLR6301
        self,
        token: Erc20Token,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
        *,
        io: PoolIO | None = None,
    ) -> int:
        """Retrieve the amount that can be spent by `spender` on behalf of `owner`.

        Returns:
            The computed value.

        """
        owner = get_checksum_address(owner)
        spender = get_checksum_address(spender)
        assert token.chain_id is not None
        assert io is not None

        block_number = _resolve_block_number(io, block_identifier)

        # Check cache
        if (approval := token.get_cached_approval(block_number, owner, spender)) is not None:
            return approval

        (approval,) = eth_abi.abi.decode(
            types=["uint256"],
            data=io.call(
                to=token.address,
                data=Web3.keccak(text="allowance(address,address)")[:4]
                + eth_abi.abi.encode(types=["address", "address"], args=[owner, spender]),
                block=block_number,
            ),
        )

        token.set_cached_approval(block_number, owner, spender, cast("int", approval))
        return cast("int", approval)

    def get_token_total_supply(  # noqa: PLR6301
        self,
        token: Erc20Token,
        block_identifier: BlockIdentifier | None = None,
        *,
        io: PoolIO | None = None,
    ) -> int:
        """Retrieve the total supply for this token.

        Returns:
            The computed value.

        """
        assert token.chain_id is not None
        assert io is not None

        block_number = _resolve_block_number(io, block_identifier)

        # Check cache
        if (total_supply := token.get_cached_total_supply(block_number)) is not None:
            return total_supply

        (total_supply,) = eth_abi.abi.decode(
            types=["uint256"],
            data=io.call(
                to=token.address,
                data=Web3.keccak(text="totalSupply()")[:4],
                block=block_number,
            ),
        )
        total_supply = int(total_supply)

        token.set_cached_total_supply(block_number, total_supply)
        return total_supply

    def get_ether_balance(  # noqa: PLR6301
        self,
        chain_id: ChainId,  # noqa: ARG002
        address: str,
        block_identifier: BlockIdentifier | None = None,
        *,
        io: PoolIO | None = None,
    ) -> int:
        """Retrieve the native ETH balance for the given address.

        Returns:
            The computed value.

        """
        address = get_checksum_address(address)
        assert io is not None

        block_number = _resolve_block_number(io, block_identifier)
        return io.get_balance(address, block=block_number)


# --- Package-level helpers (PoolIO equivalents of Erc20Token.fetch_*) ---


def _resolve_block_number(io: PoolIO, block_identifier: BlockIdentifier | None) -> int:
    """Resolve a block identifier to a block number.

    Returns:
        The computed value.

    """
    if block_identifier is None:
        return io.get_block_number()
    if isinstance(block_identifier, int):
        return block_identifier
    # For string identifiers like 'latest', 'earliest', 'pending'
    return io.get_block_number()


def _fetch_name_symbol_decimals_batched(*, address: str, io: PoolIO) -> tuple[str, str, int]:
    """Fetch token name, symbol, and decimals via batched RPC calls.

    Returns:
        The computed value.

    """
    name_calldata = encode_function_calldata(
        function_prototype="name()",
        function_arguments=None,
    )
    symbol_calldata = encode_function_calldata(
        function_prototype="symbol()",
        function_arguments=None,
    )
    decimals_calldata = encode_function_calldata(
        function_prototype="decimals()",
        function_arguments=None,
    )

    name_result = io.call(to=address, data=name_calldata)
    symbol_result = io.call(to=address, data=symbol_calldata)
    decimals_result = io.call(to=address, data=decimals_calldata)

    (name,) = eth_abi.abi.decode(types=["string"], data=name_result)
    (symbol,) = eth_abi.abi.decode(types=["string"], data=symbol_result)
    (decimals,) = eth_abi.abi.decode(types=["uint256"], data=decimals_result)

    return cast("str", name), cast("str", symbol), cast("int", decimals)


def _fetch_name(*, address: str, io: PoolIO, func_prototype: str = "name()") -> str:
    """Fetch token name via RPC call.

    Returns:
        The computed value.

    """
    result = io.call(
        to=address,
        data=encode_function_calldata(
            function_prototype=func_prototype,
            function_arguments=None,
        ),
    )

    try:
        (name,) = eth_abi.abi.decode(types=["string"], data=result)
        return cast("str", name)
    except DecodingError:
        (name,) = eth_abi.abi.decode(types=["bytes32"], data=result)
        return cast("HexBytes", name).decode("utf-8", errors="ignore").strip("\x00")


def _fetch_symbol(*, address: str, io: PoolIO, func_prototype: str = "symbol()") -> str:
    """Fetch token symbol via RPC call.

    Returns:
        The computed value.

    """
    result = io.call(
        to=address,
        data=encode_function_calldata(
            function_prototype=func_prototype,
            function_arguments=None,
        ),
    )

    try:
        (symbol,) = eth_abi.abi.decode(types=["string"], data=result)
        return cast("str", symbol)
    except DecodingError:
        (symbol,) = eth_abi.abi.decode(types=["bytes32"], data=result)
        return cast("HexBytes", symbol).decode("utf-8", errors="ignore").strip("\x00")


def _fetch_decimals(*, address: str, io: PoolIO, func_prototype: str = "decimals()") -> int:
    """Fetch token decimals via RPC call.

    Returns:
        The computed value.

    """
    (result,) = eth_abi.abi.decode(
        types=["uint256"],
        data=io.call(
            to=address,
            data=encode_function_calldata(
                function_prototype=func_prototype,
                function_arguments=None,
            ),
        ),
    )
    return cast("int", result)
