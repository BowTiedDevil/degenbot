"""ERC-20 token builder that fetches on-chain metadata."""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, cast

from degenbot.abi import AbiDecodeError, decode, encode
from degenbot.checksum_cache import get_checksum_address
from degenbot.crypto import function_selector
from degenbot.erc20 import EtherPlaceholder
from degenbot.erc20.erc20 import (
    UNKNOWN_DECIMALS,
    UNKNOWN_NAME,
    UNKNOWN_SYMBOL,
    Erc20Token,
)
from degenbot.exceptions import RpcError
from degenbot.exceptions.base import DegenbotValueError
from degenbot.logging import logger
from degenbot.provider.call_helpers import encode_function_calldata

if TYPE_CHECKING:
    from hexbytes import HexBytes

    from degenbot.bot import PyBot
    from degenbot.builders.pool_io import PoolIO
    from degenbot.database.session_manager import DatabaseSessionManager
    from degenbot.registry import TokenRegistry
    from degenbot.types.aliases import ChainId
    from degenbot.types.rpc_types import BlockIdentifier


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
            # ADR-006: ensure the token is registered in the shared PyBot
            # (Rust BotState.tokens) — a token pre-registered in the Python
            # registry might not be in the PyBot yet. Pool companions recover
            # tokens via py_pool.get_token0/get_token1, which look up the
            # identity's token address in the Rust BotState.tokens registry.
            if self._py_bot.get_token(address) is None:
                self._py_bot.register_token(
                    address,
                    existing.name,
                    existing.symbol,
                    existing.decimals,
                    chain_id,
                )
            return existing

        # Check for Ether placeholder
        if address in EtherPlaceholder.addresses:
            py_token = self._py_bot.register_token(
                address,
                "Ether Placeholder",
                "ETH",
                18,
                chain_id,
            )
            token: Erc20Token = EtherPlaceholder._from_py_token(py_token)  # noqa: SLF001
            self._tokens.add(token_address=token.address, chain_id=chain_id, token=token)
            if not silent:
                logger.info(f"• {token.symbol} ({token.name})")
            return token

        # Try DB first — route the construction-time read through the Rust
        # `PyBotIo.fetch_erc20_token` seam (QVMWQC), which opens a
        # `degenbot-db` read handle from `io.database_path`. Falls back to
        # skipping the DB read when no `io`/`database_path` is configured
        # (mirrors the prior `contextlib.suppress(Exception)` skip).
        token_from_db = None
        fetch_fn = getattr(io, "fetch_erc20_token", None) if io is not None else None
        if fetch_fn is not None:
            with contextlib.suppress(Exception):
                token_from_db = fetch_fn(chain_id=chain_id, address=address)

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
            except (RpcError, AbiDecodeError):
                # Fallback: try individual calls with alternate prototypes
                for func_prototype in ("name()", "NAME()"):
                    try:
                        fetched_name = _fetch_name(
                            address=address,
                            io=io,
                            func_prototype=func_prototype,
                        )
                        break
                    except (RpcError, AbiDecodeError):
                        continue
                else:
                    fetched_name = UNKNOWN_NAME

                for func_prototype in ("symbol()", "SYMBOL()"):
                    try:
                        fetched_symbol = _fetch_symbol(
                            address=address,
                            io=io,
                            func_prototype=func_prototype,
                        )
                        break
                    except (RpcError, AbiDecodeError):
                        continue
                else:
                    fetched_symbol = UNKNOWN_SYMBOL

                for func_prototype in ("decimals()", "DECIMALS()"):
                    try:
                        fetched_decimals = _fetch_decimals(
                            address=address,
                            io=io,
                            func_prototype=func_prototype,
                        )
                        break
                    except (RpcError, AbiDecodeError):
                        continue
                else:
                    fetched_decimals = UNKNOWN_DECIMALS

            name = name or fetched_name
            symbol = symbol or fetched_symbol
            decimals = decimals or fetched_decimals

            # Write back to DB if the record exists but was missing data.
            # Route the construction-time write-back through the Rust
            # `PyBotIo.update_erc20_token_metadata` seam (QVMWQC), which opens
            # a `degenbot-db` write handle + `UPDATE`s the row, replacing the
            # SQLAlchemy `session.commit()` dirty-tracking path.
            if (
                token_from_db is not None
                and token_from_db.name is None
                and token_from_db.symbol is None
                and token_from_db.decimals is None
                and io is not None
            ):
                update_fn = getattr(io, "update_erc20_token_metadata", None)
                if update_fn is not None:
                    with contextlib.suppress(Exception):
                        update_fn(
                            chain_id=chain_id,
                            address=address,
                            name=name,
                            symbol=symbol,
                            decimals=decimals,
                        )

        py_token = self._py_bot.register_token(address, name, symbol, decimals, chain_id)
        token = Erc20Token._from_py_token(py_token)  # noqa: SLF001

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

        # ADR-005 slice 14d: when io is a PyBotIo (Bot's build path),
        # delegate the balanceOf choreography to Rust. SyncPoolIO fallback
        # keeps the Python implementation as a parity gate.
        fetch_token_balance = getattr(io, "fetch_token_balance", None)
        if fetch_token_balance is not None:
            balance = fetch_token_balance(token.address, address, block=block_number)
        else:
            (balance,) = decode(
                types=["uint256"],
                data=io.call(
                    to=token.address,
                    data=function_selector("balanceOf(address)")
                    + encode(types=["address"], args=[address]),
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

        # ADR-005 slice 14d: same delegation seam as `get_token_balance`.
        fetch_token_allowance = getattr(io, "fetch_token_allowance", None)
        if fetch_token_allowance is not None:
            approval = fetch_token_allowance(token.address, owner, spender, block=block_number)
        else:
            (approval,) = decode(
                types=["uint256"],
                data=io.call(
                    to=token.address,
                    data=function_selector("allowance(address,address)")
                    + encode(types=["address", "address"], args=[owner, spender]),
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

        # ADR-005 slice 14d: same delegation seam as `get_token_balance`.
        fetch_token_total_supply = getattr(io, "fetch_token_total_supply", None)
        if fetch_token_total_supply is not None:
            total_supply = fetch_token_total_supply(token.address, block=block_number)
        else:
            (total_supply,) = decode(
                types=["uint256"],
                data=io.call(
                    to=token.address,
                    data=function_selector("totalSupply()"),
                    block=block_number,
                ),
            )
            total_supply = int(total_supply)
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

    When ``io`` is a Rust :class:`~degenbot._ffi.PyBotIo` (the Bot's
    build-path adapter -- ADR-005 slice 14a), the encode -> call (x3) -> decode
    choreography is delegated to ``PyBotIo.fetch_erc20_metadata`` (Rust, slice
    14c), not run in Python. Raw ``SyncPoolIO`` callers fork / offline tests
    still exercise the Python implementation below -- a behavior-preserving
    parity gate against the Rust impl. Returns `None` from the Rust impl on
    provider error / decode failure (mirrors the caller's `except
    (RpcError, AbiDecodeError)` fallback contract); a Python-raised error
    from the fallback path is surfaced untouched.

    Returns:
        The computed value.

    Raises:
        AbiDecodeError: If the Rust batched path failed (provider revert or
            decode failure) -- re-raised so the caller's `except
            (RpcError, AbiDecodeError)` fallback kicks in identically.

    """
    # ADR-005 slice 14c: route through PyBotIo when available -- the
    # choreography is Rust-owned. `hasattr` keeps the SyncPoolIO fallback path
    # working without importing PyBotIo (which lives in the Rust ext).
    fetch_metadata = getattr(io, "fetch_erc20_metadata", None)
    if fetch_metadata is not None:
        result = fetch_metadata(address)
        if result is None:
            msg = "batched fetch failed (provider revert or decode failure)"
            raise AbiDecodeError(message=msg)
        return cast("tuple[str, str, int]", result)

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

    (name,) = decode(types=["string"], data=name_result)
    (symbol,) = decode(types=["string"], data=symbol_result)
    (decimals,) = decode(types=["uint256"], data=decimals_result)

    return cast("str", name), cast("str", symbol), cast("int", decimals)


def _fetch_name(*, address: str, io: PoolIO, func_prototype: str = "name()") -> str:
    """Fetch token name via RPC call.

    When ``io`` is a Rust :class:`~degenbot._ffi.PyBotIo`, delegates
    to ``PyBotIo.fetch_erc20_string_field`` (Rust, slice 14h).

    Returns:
        The computed value.

    Raises:
        AbiDecodeError: If the field could not be decoded as string or bytes32.

    """
    fetch_string_field = getattr(io, "fetch_erc20_string_field", None)
    if fetch_string_field is not None:
        try:
            return fetch_string_field(address, func_prototype)
        except ValueError as exc:
            raise AbiDecodeError(message=str(exc)) from exc

    result = io.call(
        to=address,
        data=encode_function_calldata(
            function_prototype=func_prototype,
            function_arguments=None,
        ),
    )

    try:
        (name,) = decode(types=["string"], data=result)
        return cast("str", name)
    except AbiDecodeError:
        (name,) = decode(types=["bytes32"], data=result)
        return cast("HexBytes", name).decode("utf-8", errors="ignore").strip("\x00")


def _fetch_symbol(*, address: str, io: PoolIO, func_prototype: str = "symbol()") -> str:
    """Fetch token symbol via RPC call.

    When ``io`` is a Rust :class:`~degenbot._ffi.PyBotIo`, delegates
    to ``PyBotIo.fetch_erc20_string_field`` (Rust, slice 14h).

    Returns:
        The computed value.

    Raises:
        AbiDecodeError: If the field could not be decoded as string or bytes32.

    """
    fetch_string_field = getattr(io, "fetch_erc20_string_field", None)
    if fetch_string_field is not None:
        try:
            return fetch_string_field(address, func_prototype)
        except ValueError as exc:
            raise AbiDecodeError(message=str(exc)) from exc

    result = io.call(
        to=address,
        data=encode_function_calldata(
            function_prototype=func_prototype,
            function_arguments=None,
        ),
    )

    try:
        (symbol,) = decode(types=["string"], data=result)
        return cast("str", symbol)
    except AbiDecodeError:
        (symbol,) = decode(types=["bytes32"], data=result)
        return cast("HexBytes", symbol).decode("utf-8", errors="ignore").strip("\x00")


def _fetch_decimals(*, address: str, io: PoolIO, func_prototype: str = "decimals()") -> int:
    """Fetch token decimals via RPC call.

    When ``io`` is a Rust :class:`~degenbot._ffi.PyBotIo`, delegates
    to ``PyBotIo.fetch_erc20_uint_field`` (Rust, slice 14h).

    Returns:
        The computed value.

    Raises:
        AbiDecodeError: If the field could not be decoded as uint256.

    """
    fetch_uint_field = getattr(io, "fetch_erc20_uint_field", None)
    if fetch_uint_field is not None:
        try:
            return cast("int", fetch_uint_field(address, func_prototype))
        except ValueError as exc:
            raise AbiDecodeError(message=str(exc)) from exc

    (result,) = decode(
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
