"""Protocol for pool construction and state updates.

Each builder owns:
- The I/O choreography (DB lookup → RPC fetch → decode → construct)
- Pool registration in the Pool Registry
- State updates via pool.external_update()

Builders do NOT own:
- Pool type resolution (Bot's job)
- Connection management (received via ConnectionManager)
- Database lifecycle (received via DatabaseSessionManager)
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:
    from typing import Any

    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool


class PoolBuilder(Protocol):
    """Protocol for pool construction and state updates."""

    def build(
        self,
        address: str,
        *,
        chain_id: int | None = None,
        state_block: int | None = None,
        silent: bool = False,
        **kwargs: Any,
    ) -> AbstractLiquidityPool: ...

    def update(
        self,
        pool: AbstractLiquidityPool,
        *,
        block_number: int | None = None,
    ) -> bool: ...
