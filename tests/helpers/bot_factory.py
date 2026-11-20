"""Test helpers for constructing ``Bot`` instances and the session/provider pairs.

The Bot half is :func:`make_bot_with_provider` (a single-chain Bot around a real
provider). The other half is the construction vocabulary the facade and
build-path suites had each re-declared: :func:`bot_session` (the explicit
keyword overrides a session under test runs with) and :func:`offline_provider`
(the provider stand-in, alloy-backed because that is the one shape a boot
accepts).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.bot import Bot
from degenbot.provider import AlloyProvider, OfflineProvider

if TYPE_CHECKING:
    import pathlib


def offline_provider(chain_id: int = 1) -> OfflineProvider:
    """The provider stand-in a Bot-construction suite binds.

    A real ``OfflineProvider`` over an in-memory Rust transport (recorded JSON,
    no RPC at construction): alloy-backed, which is what a ``Bot`` boot requires
    of its provider, and answering ``get_chain_id()`` from the recording.

    Args:
        chain_id: The chain the provider answers for ``get_chain_id()``.

    Returns:
        The provider stand-in, ready to hand to ``Bot(provider=...)``.

    """
    return OfflineProvider(
        chain_id=chain_id,
        blocks={"1": {"timestamp": 1, "calls": {}, "code": {}}},
    )


def bot_session(tmp_path: pathlib.Path, chain_id: int = 1) -> dict[str, object]:
    """The explicit keyword overrides a session under test runs with."""
    return {"chain_id": chain_id, "database": str(tmp_path / "test.db")}


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

    Args:
        provider: The provider the session binds.
        chain_id: An explicit chain override; ``None`` takes the provider's.
        database_path: The session's database path.

    Returns:
        The session.

    """
    resolved_chain_id = chain_id if chain_id is not None else provider.get_chain_id()
    return Bot(
        chain_id=resolved_chain_id,
        database=database_path,
        provider=provider,
    )
