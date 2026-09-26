"""Token registry: the Python companion side of the session's token identities.

An ERC-20 token's canonical identity is its address in the session's chain, so
the registry is a straight delegate: ask the Rust `SessionObjectRegistry` which
token object the session holds (or should hold) for an address, and keep the
`Erc20Token` companion that presents it. See
:mod:`degenbot.registry.session` for why the companion cache is not a second
identity authority.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.exceptions.base import DegenbotValueError
from degenbot.registry.session import CompanionCache, SessionObjects

if TYPE_CHECKING:
    from collections.abc import Iterator

    from degenbot._ffi import Bot
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types.aliases import ChainId


class TokenRegistry:
    """ERC-20 token companions, keyed by the session's token identities."""

    def __init__(self, *, py_bot: Bot) -> None:
        """Bind to the session that owns the token identities.

        Args:
            py_bot: The session's Rust `Bot` handle. Required: identity is the
                session's, so a registry cannot stand up without one.

        """
        self._session = SessionObjects(py_bot)
        self._companions: CompanionCache[Erc20Token] = CompanionCache()

    def get(
        self,
        token_address: str,
        chain_id: ChainId,
    ) -> Erc20Token | None:
        """Retrieve a token by chain and address.

        Returns:
            The registered token, or None if not found.

        """
        handle = self._session.resolve_token(chain_id=chain_id, address=token_address)
        return None if handle is None else self._companions.resolve(handle)

    def add(
        self,
        token_address: str,
        chain_id: ChainId,
        token: Erc20Token,
    ) -> None:
        """Register a token.

        Raises:
            DegenbotValueError: A companion is already registered for this
                token identity.

        """
        handle = self._session.get_or_create_token(chain_id=chain_id, address=token_address)
        if self._companions.resolve(handle) is not None:
            msg = f"Token is already registered at key {handle.key}"
            raise DegenbotValueError(message=msg)
        self._companions.store(handle, token)

    def get_or_add(
        self,
        token_address: str,
        chain_id: ChainId,
        token: Erc20Token,
    ) -> Erc20Token:
        """Idempotently register a token, returning the stored instance.

        If a concurrent registration worker already built this token, return the
        canonical stored instance instead of raising (35NMBX Guard 1) — a
        distinct path sharing this token is not lossily skipped.

        Returns:
            The stored token instance (the existing canonical one on a duplicate).

        """
        handle = self._session.get_or_create_token(chain_id=chain_id, address=token_address)
        return self._companions.get_or_store(handle, token)

    def remove(
        self,
        token_address: str,
        chain_id: ChainId,
    ) -> None:
        """Drop the companion for this token.

        The session's identity for the token is session-lifetime and stays, so a
        later build of the same address re-files a companion under the same
        canonical identity. No live token state is touched: the Rust
        `BotState` token entry belongs to `register_token` / `build_erc20_token`.
        """
        handle = self._session.resolve_token(chain_id=chain_id, address=token_address)
        if handle is not None:
            self._companions.drop(handle)

    def list_all(self) -> Iterator[Erc20Token]:
        """Yield every registered token.

        Yields:
            Each token companion filed in this registry.

        """
        for entry in self._companions.entries():
            yield entry.item

    def reset(self) -> None:
        """Drop every token companion. The session's identities are untouched."""
        self._companions.clear()

    def __len__(self) -> int:
        """Count the registered tokens.

        Returns:
            The number of token companions filed.

        """
        return len(self._companions)
