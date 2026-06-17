"""Shared dependency context for pool builders.

Collected from Bot's constructor wiring so builders receive a single
parameter instead of 5-6 individual dependencies. Adding a new builder
requires zero additional wiring in Bot.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from degenbot.builders.erc20_builder import Erc20Builder
    from degenbot.database.session_manager import DatabaseSessionManager
    from degenbot.degenbot_rs import PyBot
    from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
    from degenbot.types.aliases import ChainId


@dataclasses.dataclass(slots=True, frozen=True)
class BuilderContext:
    """Shared dependencies for all pool builders.

    Bot creates one ``BuilderContext`` and passes it to all builders.
    Each builder unpacks what it needs. ``Erc20Builder`` is a leaf
    dependency — it is constructed before the context and passed in.

    ``managed_pools`` is optional because only V3/V4 builders need it.
    """

    db: DatabaseSessionManager
    pools: PoolRegistry
    tokens: TokenRegistry
    erc20_builder: Erc20Builder
    py_bot: PyBot
    default_chain_id: ChainId | None = None
    managed_pools: ManagedPoolRegistry | None = None
