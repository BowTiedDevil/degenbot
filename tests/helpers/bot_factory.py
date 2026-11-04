"""Test helpers for constructing ``Bot`` instances and the session/provider pairs.

The Bot half is :func:`make_bot_with_provider` (a single-chain Bot around a real
provider). The other half is the construction vocabulary the facade and
build-path suites had each re-declared: :func:`bot_session` (the explicit
keyword overrides a session under test runs with) and :func:`fake_provider` (the
provider double, with the kind named at the call site).
"""

from __future__ import annotations

import enum
from typing import TYPE_CHECKING, Any
from unittest.mock import MagicMock

from degenbot.bot import Bot
from degenbot.provider import AlloyProvider, OfflineProvider

if TYPE_CHECKING:
    import pathlib


class ProviderKind(enum.Enum):
    """Which provider stand-in a Bot-construction suite constructs.

    The suites drifted into two kinds, and which one a suite uses is part of its
    setup contract, not an incidental local choice:

    - ``OFFLINE``: a real ``OfflineProvider`` over an in-memory Rust transport
      (recorded JSON, no RPC at construction). The facade tests want a provider
      whose ``get_chain_id()`` is a real answer.
    - ``ALLOY_MOCK``: a ``spec``-ed ``AlloyProvider`` mock. The build-path tests
      inject all pool/token data and only ever read ``get_chain_id``.

    Naming the kind at the call site keeps each suite on the double it had
    while the construction lives in one place.
    """

    OFFLINE = "offline"
    ALLOY_MOCK = "alloy-mock"


def bot_session(tmp_path: pathlib.Path, chain_id: int = 1) -> dict[str, object]:
    """The explicit keyword overrides a session under test runs with."""
    return {"chain_id": chain_id, "database": str(tmp_path / "test.db")}


def fake_provider(kind: ProviderKind, chain_id: int = 1) -> Any:
    """The provider double of ``kind``, reporting ``chain_id``.

    Args:
        kind: Which double to build (see :class:`ProviderKind`).
        chain_id: The chain the provider answers for ``get_chain_id()``.

    Returns:
        The provider double, ready to hand to ``Bot(provider=...)``.

    """
    if kind is ProviderKind.OFFLINE:
        return OfflineProvider(
            chain_id=chain_id,
            blocks={"1": {"timestamp": 1, "calls": {}, "code": {}}},
        )
    mock = MagicMock(spec=AlloyProvider)
    mock.get_chain_id.return_value = chain_id
    return mock


def make_bot_with_provider(
    provider: AlloyProvider,
    chain_id: int | None = None,
    database_path: str = ":memory:",
) -> Bot:
    """Create a single-chain Bot around the given provider (ADR-006 D5).

    The chain identity derives from ``provider.chain_id`` (or an explicit
    ``chain_id`` override) and is passed as the session's explicit override, so
    it outranks every configured layer; ``Bot`` then enforces that the
    provider's ``eth_chainId`` matches.
    """
    resolved_chain_id = chain_id if chain_id is not None else provider.get_chain_id()
    return Bot(
        chain_id=resolved_chain_id,
        database=database_path,
        provider=provider,
    )
