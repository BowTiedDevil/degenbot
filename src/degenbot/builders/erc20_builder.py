"""ERC-20 token builder that fetches on-chain metadata."""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING

from degenbot.abi import AbiDecodeError
from degenbot.checksum_cache import get_checksum_address
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

if TYPE_CHECKING:
    from collections.abc import Sequence

    from degenbot.bot import PyBot, PyBotIo
    from degenbot.database import Erc20TokenRow
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
        io: PyBotIo | None = None,
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
            token: Erc20Token = EtherPlaceholder._from_py_token(py_token)  # ruff:ignore[private-member-access]
            token = self._tokens.get_or_add(
                token_address=token.address, chain_id=chain_id, token=token
            )
            if not silent:
                logger.info(f"• {token.symbol} ({token.name})")
            return token

        # Try DB first — route the construction-time read through the Rust
        # `PyBotIo.fetch_erc20_token` seam (QVMWQC), which opens a
        # `degenbot-db` read handle from `io.database_path`. Falls back to
        # skipping the DB read when no `io`/`database_path` is configured
        # (mirrors the prior `contextlib.suppress(Exception)` skip).
        token_from_db = None
        if io is not None:
            with contextlib.suppress(Exception):
                token_from_db = io.fetch_erc20_token(chain_id=chain_id, address=address)

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
                with contextlib.suppress(Exception):
                    io.update_erc20_token_metadata(
                        chain_id=chain_id,
                        address=address,
                        name=name,
                        symbol=symbol,
                        decimals=decimals,
                    )

        py_token = self._py_bot.register_token(address, name, symbol, decimals, chain_id)
        token = Erc20Token._from_py_token(py_token)  # ruff:ignore[private-member-access]

        # Register idempotently (35NMBX Guard 1): a concurrent worker may have
        # built + registered this same token first; use the canonical instance
        # so a path sharing it is not lossily skipped.
        token = self._tokens.get_or_add(token_address=token.address, chain_id=chain_id, token=token)

        if not silent:
            logger.info(f"• {token.symbol} ({token.name})")

        return token

    def _register_from_metadata(
        self,
        address: str,
        name: str,
        symbol: str,
        decimals: int,
        *,
        chain_id: ChainId,
        silent: bool,
    ) -> Erc20Token:
        """Register a token in the Rust BotState + Python registry from metadata.

        Returns:
            The canonical token instance (35NMBX Guard 1 ``get_or_add`` path).

        """
        py_token = self._py_bot.register_token(address, name, symbol, decimals, chain_id)
        token = Erc20Token._from_py_token(py_token)  # ruff:ignore[private-member-access]
        token = self._tokens.get_or_add(token_address=token.address, chain_id=chain_id, token=token)
        if not silent:
            logger.info(f"• {token.symbol} ({token.name})")
        return token

    def build_many(
        self,
        addresses: Sequence[str],
        *,
        chain_id: ChainId | None = None,
        silent: bool = False,
        io: PyBotIo | None = None,
    ) -> list[Erc20Token]:
        """Build MANY tokens, batching metadata reads into ONE Multicall3 read.

        CDJEPJ-2: preserves :meth:`build`'s per-token semantics — registry fast
        path, Ether placeholder, DB lookup, alternate-prototype fallback — but
        when 2+ tokens need a network metadata fetch it issues ONE
        ``io.fetch_erc20_metadata_batch([...])`` instead of N separate
        ``fetch_erc20_metadata`` round-trips. A token whose batched metadata
        came back ``None`` falls back to per-token :meth:`build` (so the
        contract-deployed check + alternate-prototype fallback still apply).

        Returns:
            One :class:`Erc20Token` per input address, in order.

        """
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"

        resolved: list[Erc20Token | None] = []
        # (result_index, address, token_from_db) still needing a network fetch.
        network_needed: list[tuple[int, str, Erc20TokenRow | None]] = []

        for raw_address in addresses:
            address = get_checksum_address(raw_address)
            if (existing := self._tokens.get(token_address=address, chain_id=chain_id)) is not None:
                # ADR-006: ensure the token is registered in the shared PyBot.
                if self._py_bot.get_token(address) is None:
                    self._py_bot.register_token(
                        address,
                        existing.name,
                        existing.symbol,
                        existing.decimals,
                        chain_id,
                    )
                resolved.append(existing)
                continue
            if address in EtherPlaceholder.addresses:
                resolved.append(
                    self._register_from_metadata(
                        address,
                        "Ether Placeholder",
                        "ETH",
                        18,
                        chain_id=chain_id,
                        silent=silent,
                    )
                )
                continue
            token_from_db = None
            if io is not None:
                with contextlib.suppress(Exception):
                    token_from_db = io.fetch_erc20_token(chain_id=chain_id, address=address)
            if (
                token_from_db is not None
                and token_from_db.name is not None
                and token_from_db.symbol is not None
                and token_from_db.decimals is not None
            ):
                resolved.append(
                    self._register_from_metadata(
                        address,
                        str(token_from_db.name),
                        str(token_from_db.symbol),
                        int(token_from_db.decimals),
                        chain_id=chain_id,
                        silent=silent,
                    )
                )
                continue
            network_needed.append((len(resolved), address, token_from_db))
            resolved.append(None)

        if network_needed:
            assert io is not None, "io required to fetch network token metadata"
            metas = io.fetch_erc20_metadata_batch([addr for _, addr, _ in network_needed])
            for (idx, address, token_from_db), meta in zip(network_needed, metas, strict=True):
                if meta is None:
                    # Fall back to the full per-token build (which performs the
                    # contract-deployed check + alternate-prototype fallback).
                    resolved[idx] = self.build(address, chain_id=chain_id, silent=silent, io=io)
                else:
                    name, symbol, decimals = meta
                    # Write back to DB if the record exists but was missing data.
                    if (
                        token_from_db is not None
                        and token_from_db.name is None
                        and token_from_db.symbol is None
                        and token_from_db.decimals is None
                        and io is not None
                    ):
                        with contextlib.suppress(Exception):
                            io.update_erc20_token_metadata(
                                chain_id=chain_id,
                                address=address,
                                name=name,
                                symbol=symbol,
                                decimals=decimals,
                            )
                    resolved[idx] = self._register_from_metadata(
                        address, name, symbol, int(decimals), chain_id=chain_id, silent=silent
                    )

        return [t for t in resolved if t is not None]

    def get_token_balance(  # ruff:ignore[no-self-use]
        self,
        token: Erc20Token,
        address: str,
        block_identifier: BlockIdentifier | None = None,
        *,
        io: PyBotIo | None = None,
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

        # ADR-005 slice 14d: delegate the balanceOf choreography to Rust
        # (PyBotIo is the only executor; the Python parity-gate fallback is retired).
        balance = io.fetch_token_balance(token.address, address, block=block_number)

        token.set_cached_balance(address, block_number, balance)
        return balance

    def get_token_approval(  # ruff:ignore[no-self-use]
        self,
        token: Erc20Token,
        owner: str,
        spender: str,
        block_identifier: BlockIdentifier | None = None,
        *,
        io: PyBotIo | None = None,
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
        approval = io.fetch_token_allowance(token.address, owner, spender, block=block_number)

        token.set_cached_approval(block_number, owner, spender, approval)
        return approval

    def get_token_total_supply(  # ruff:ignore[no-self-use]
        self,
        token: Erc20Token,
        block_identifier: BlockIdentifier | None = None,
        *,
        io: PyBotIo | None = None,
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
        total_supply = int(io.fetch_token_total_supply(token.address, block=block_number))

        token.set_cached_total_supply(block_number, total_supply)
        return total_supply

    def get_ether_balance(  # ruff:ignore[no-self-use]
        self,
        chain_id: ChainId,  # ruff:ignore[unused-method-argument]
        address: str,
        block_identifier: BlockIdentifier | None = None,
        *,
        io: PyBotIo | None = None,
    ) -> int:
        """Retrieve the native ETH balance for the given address.

        Returns:
            The computed value.

        """
        address = get_checksum_address(address)
        assert io is not None

        block_number = _resolve_block_number(io, block_identifier)
        return io.get_balance(address, block=block_number)


# --- Package-level helpers (PyBotIo equivalents of Erc20Token.fetch_*) ---


def _resolve_block_number(io: PyBotIo, block_identifier: BlockIdentifier | None) -> int:
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


def _fetch_name_symbol_decimals_batched(*, address: str, io: PyBotIo) -> tuple[str, str, int]:
    """Fetch token name/symbol/decimals via Rust batch fetch.

    The 3-call choreography is Rust-owned (``PyBotIo.fetch_erc20_metadata``,
    ADR-005 slice 14c). PyBotIo is the only executor; the Python parity-gate
    fallback is retired.

    Returns:
        The computed value.

    Raises:
        AbiDecodeError: If the batched fetch failed (provider revert or
            decode failure).

    """
    result = io.fetch_erc20_metadata(address)
    if result is None:
        msg = "batched fetch failed (provider revert or decode failure)"
        raise AbiDecodeError(message=msg)
    return result


def _fetch_name(*, address: str, io: PyBotIo, func_prototype: str = "name()") -> str:
    """Fetch token name via Rust string-field fetch.

    Delegates to ``PyBotIo.fetch_erc20_string_field`` (ADR-005 slice 14h).
    PyBotIo is the only executor; the Python parity-gate fallback is retired.

    Returns:
        The computed value.

    Raises:
        AbiDecodeError: If the field could not be decoded as string or bytes32.

    """
    try:
        return io.fetch_erc20_string_field(address, func_prototype)
    except ValueError as exc:
        raise AbiDecodeError(message=str(exc)) from exc


def _fetch_symbol(*, address: str, io: PyBotIo, func_prototype: str = "symbol()") -> str:
    """Fetch token symbol via Rust string-field fetch.

    Delegates to ``PyBotIo.fetch_erc20_string_field`` (ADR-005 slice 14h).
    PyBotIo is the only executor; the Python parity-gate fallback is retired.

    Returns:
        The computed value.

    Raises:
        AbiDecodeError: If the field could not be decoded as string or bytes32.

    """
    try:
        return io.fetch_erc20_string_field(address, func_prototype)
    except ValueError as exc:
        raise AbiDecodeError(message=str(exc)) from exc


def _fetch_decimals(*, address: str, io: PyBotIo, func_prototype: str = "decimals()") -> int:
    """Fetch token decimals via Rust uint-field fetch.

    Delegates to ``PyBotIo.fetch_erc20_uint_field`` (ADR-005 slice 14h).
    PyBotIo is the only executor; the Python parity-gate fallback is retired.

    Returns:
        The computed value.

    Raises:
        AbiDecodeError: If the field could not be decoded as uint256.

    """
    try:
        return io.fetch_erc20_uint_field(address, func_prototype)
    except ValueError as exc:
        raise AbiDecodeError(message=str(exc)) from exc
