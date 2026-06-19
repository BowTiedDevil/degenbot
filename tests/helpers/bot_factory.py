"""Test helpers for constructing Bot instances with AnvilFork providers."""

from pathlib import Path

from degenbot.bot import Bot
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.provider import ProviderAdapter


def make_bot_with_provider(
    provider: ProviderAdapter,
    chain_id: int | None = None,
    database_path: str = ":memory:",
) -> Bot:
    """Create a single-chain Bot around the given provider (ADR-006 D5).

    The chain identity derives from ``provider.chain_id`` (or an explicit
    ``chain_id`` override) and is set on the config; ``Bot.__init__`` then
    enforces the provider's ``eth_chainId`` matches it.
    """
    resolved_chain_id = chain_id if chain_id is not None else provider.chain_id
    config = DegenbotConfig(
        database=DatabaseSettings(path=Path(database_path)),
        rpc={},
        default_chain_id=resolved_chain_id,
    )
    return Bot(config=config, provider=provider)
