"""Protocol for pool construction and state updates.

Each builder owns:
- The I/O choreography (DB lookup → RPC fetch → decode → construct)
- Pool registration in the Pool Registry
- State updates via pool.external_update()

Builders do NOT own:
- Pool type resolution (Bot's job)
- I/O routing (received via PoolIO / AsyncPoolIOProtocol)
- Database lifecycle (received via DatabaseSessionManager)
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:
    from degenbot.builders.pool_io import AsyncPoolIO, PoolIO
    from degenbot.builders.request import BuildRequest
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool


class PoolBuilder(Protocol):
    """Protocol for pool construction and state updates.

    Builders must accept ``address`` as the first positional-or-keyword
    parameter in ``build()``, and ``pool`` as the first positional-or-keyword
    parameter in ``update()``. Optional parameters are forwarded via
    ``request: BuildPoolRequest`` — builders read the fields they recognize
    and ignore the rest.
    """

    def build(
        self,
        address: str,
        *,
        chain_id: int | None = None,
        io: PoolIO,
        request: BuildRequest,
    ) -> AbstractLiquidityPool: ...

    def update(
        self,
        pool: AbstractLiquidityPool,
        *,
        io: PoolIO | None = None,
        block_number: int | None = None,
    ) -> bool: ...


class AsyncPoolBuilder(Protocol):
    """Async counterpart of PoolBuilder.

    Satisfies the same interface but with async build/update methods.
    Used by AsyncBot.
    """

    async def build(
        self,
        address: str,
        *,
        chain_id: int | None = None,
        io: AsyncPoolIO,
        request: BuildRequest,
    ) -> AbstractLiquidityPool: ...

    async def update(
        self,
        pool: AbstractLiquidityPool,
        *,
        io: AsyncPoolIO | None = None,
        block_number: int | None = None,
    ) -> bool: ...
