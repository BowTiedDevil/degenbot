"""Test helpers for constructing Bot instances with AnvilFork providers."""

from pathlib import Path

from degenbot.bot import Bot
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.provider import ProviderAdapter


def make_bot_for_fork(chain_id: int, database_path: str = ":memory:") -> Bot:
    """
    Create a Bot with a minimal config suitable for fork tests.

    Does NOT register any provider — the caller must do that afterwards.
    """
    config = DegenbotConfig(
        database=DatabaseSettings(path=Path(database_path)),
        rpc={},
    )
    return Bot(config=config)


def make_bot_with_provider(provider: ProviderAdapter, chain_id: int | None = None) -> Bot:
    """
    Create a Bot, register the given provider, and return it.

    If chain_id is not given, it will be read from provider.chain_id after registration.
    """
    bot = make_bot_for_fork(chain_id=chain_id or 1)
    bot.connections.register_provider(provider)
    # Set the default chain so self.connections.default_chain_id works
    resolved_chain_id = chain_id or provider.chain_id
    bot.connections.set_default_chain(resolved_chain_id)
    return bot
