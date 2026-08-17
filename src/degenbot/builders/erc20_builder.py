"""ERC-20 token builder that fetches on-chain metadata."""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING

from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import EtherPlaceholder
from degenbot.erc20.erc20 import Erc20Token
from degenbot.exceptions.base import DegenbotValueError
from degenbot.logging import logger

if TYPE_CHECKING:
    from collections.abc import Sequence

    from degenbot.bot import RustBot, RustBotIo
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
        py_bot: RustBot,
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
        io: RustBotIo | None = None,  # ruff:ignore[unused-method-argument] - ignored compat shim (S5 strips it)
    ) -> Erc20Token:
        """Construct an ERC-20 token, delegating metadata resolution to the Rust core.

        The DB-first + on-chain resolution, write-back and `BotState`
        registration are Rust-owned (VK3YDM-S2): `RustBot.build_erc20_token`
        runs `build_erc20_metadata` over the attached `ConstructionIo`. This
        Python shell keeps only the *companion* concerns: the `TokenRegistry`
        idempotent short-circuit (35NMBX Guard 1), the `EtherPlaceholder`
        special case, and the `Erc20Token._from_py_token` display wrapper.
        The `io` argument is a retained compat shim (ignored — the RustBot owns
        the `ConstructionIo`); it is stripped when `RustBotIo` retires
        (VK3YDM-S5).

        Returns:
            The computed value.

        Raises:
            DegenbotValueError: No contract is deployed at the address, or the
                token metadata could not be resolved on-chain.

        """
        address = get_checksum_address(address)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"

        # Check registry first
        if (existing := self._tokens.get(token_address=address, chain_id=chain_id)) is not None:
            # ADR-006: ensure the token is registered in the shared RustBot
            # (Rust BotState.tokens) — a token pre-registered in the Python
            # registry might not be in the RustBot yet. Pool companions recover
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

        # DB-first + on-chain + write-back + register: Rust-owned (VK3YDM-S2).
        # `RustBot.build_erc20_token` resolves name/symbol/decimals (DB row -> on-
        # chain batched read -> alternate-prototype fallback -> UNKNOWN, with a
        # blank-row write-back) and registers into the shared Rust
        # `BotState.tokens` (ADR-006) in one core call over the attached
        # `ConstructionIo`. Returns a thin `RustErc20Token` handle.
        try:
            py_token = self._py_bot.build_erc20_token(address, chain_id)
        except RuntimeError as exc:
            # Preserve the documented DegenbotValueError contract: the binding
            # flattens core build failures to RuntimeError (map_builder_err),
            # so re-raise as DegenbotValueError, stripping the binding's
            # "pool build decode failure: " prefix.
            raise DegenbotValueError(
                message=str(exc).removeprefix("pool build decode failure: ")
            ) from exc
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
        io: RustBotIo | None = None,
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
                # ADR-006: ensure the token is registered in the shared RustBot.
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
        io: RustBotIo | None = None,
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
        # (RustBotIo is the only executor; the Python parity-gate fallback is retired).
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
        io: RustBotIo | None = None,
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
        io: RustBotIo | None = None,
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
        io: RustBotIo | None = None,
    ) -> int:
        """Retrieve the native ETH balance for the given address.

        Returns:
            The computed value.

        """
        address = get_checksum_address(address)
        assert io is not None

        block_number = _resolve_block_number(io, block_identifier)
        return io.get_balance(address, block=block_number)


# --- Package-level helpers (RustBotIo equivalents of Erc20Token.fetch_*) ---


def _resolve_block_number(io: RustBotIo, block_identifier: BlockIdentifier | None) -> int:
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
