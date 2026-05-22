"""
Shared dependency context for async pool builders.

Mirrors BuilderContext but with async-compatible types.
AsyncBot creates one AsyncBuilderContext and passes it to all async builders.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from degenbot.builders.async_erc20_builder import AsyncErc20Builder
    from degenbot.database.session_manager import DatabaseSessionManager
    from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
    from degenbot.types.aliases import ChainId


@dataclasses.dataclass(slots=True, frozen=True)
class AsyncBuilderContext:
    """
    Shared dependencies for all async pool builders.

    AsyncBot creates one ``AsyncBuilderContext`` and passes it to all
    async builders. Each builder unpacks what it needs.

    ``managed_pools`` is optional because only V3/V4 builders need it.
    """

    db: DatabaseSessionManager
    pools: PoolRegistry
    tokens: TokenRegistry
    erc20_builder: AsyncErc20Builder
    default_chain_id: ChainId | None = None
    managed_pools: ManagedPoolRegistry | None = None
