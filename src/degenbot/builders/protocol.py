"""Protocol for pool construction and state updates.

Each builder owns:
- The I/O choreography (DB lookup → RPC fetch → decode → construct)
- Pool registration in the Pool Registry
- State updates via pool.external_update()

Builders do NOT own:
- Pool type resolution (Bot's job)
- I/O routing (received via RustBotIo — the single Rust-backed executor)
- Database lifecycle (received via DatabaseSessionManager)
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:
    from degenbot.bot import RustBotIo
    from degenbot.builders.request import BuildRequest
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool


class PoolBuilder(Protocol):
    """Protocol for pool construction and state updates.

    Builders must accept ``address`` as the first positional-or-keyword
    parameter in ``build()``, and ``pool`` as the first positional-or-keyword
    parameter in ``update()``. Optional parameters are forwarded via
    ``request: BuildPoolRequest`` — builders read the fields they recognize
    and ignore the rest.

    The ``io`` argument is a :class:`~degenbot._ffi.RustBotIo` — the single
    construction-I/O executor (Rust-backed, 65 methods). There is no
    sync/async fork; ``AsyncBot`` and the async builders are retired.
    """

    def build(
        self,
        address: str,
        *,
        chain_id: int | None = None,
        io: RustBotIo,
        request: BuildRequest,
    ) -> AbstractLiquidityPool:
        """Build a pool from on-chain data."""
        ...

    @staticmethod
    def update(
        pool: AbstractLiquidityPool,
        *,
        io: RustBotIo | None = None,
        block_number: int | None = None,
    ) -> bool:
        """Update the pool state from on-chain data."""
        ...
