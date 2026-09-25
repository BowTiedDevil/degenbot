"""Test helpers for constructing Bot instances with AnvilFork providers."""

from degenbot.bot import Bot
from degenbot.provider import AlloyProvider


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
    resolved_chain_id = chain_id if chain_id is not None else provider.chain_id
    return Bot(
        chain_id=resolved_chain_id,
        database=database_path,
        provider=provider,
    )
