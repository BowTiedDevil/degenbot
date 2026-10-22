"""Shared concentrated-liquidity companion surface.

``UniswapV3Pool`` and ``UniswapV4Pool`` carried duplicated copies of the
same concentrated liquidity surface — the scalar/tick-map reads and the
``swap_is_viable`` predicate. This module collapses that surface into one base
so the read path is written once. Writes are not on this surface: state moves
only through the pump's own door (``Bot.dispatch_log`` →
``LogDispatcher`` decode → apply), never through a companion method.

What stays on the subclasses: identity (address/factory/tokens/fee/
pool_key/hook/protocol-fees), the family calc mixins, and ``__eq__``/
``__hash__``. This base does no family math (ADR-005: the driver shell is
not a co-implementation) and adds no cross-family Rust seam (ADR-014);
the companions stay Python (ADR-023).
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, cast

from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.uniswap.concentrated.types import LiquidityAtTick

if TYPE_CHECKING:
    from degenbot._ffi import Pool
    from degenbot.types.aliases import BlockNumber
    from degenbot.types.chain import ChecksummedAddress
    from degenbot.uniswap.types import UniswapPoolSwapVector


# A concentrated liquidity state (the family NamedTuple the ``state`` property builds).
type CLState = Any


class ConcentratedLiquidityCompanion(AbstractLiquidityPool):
    """Shared concentrated liquidity companion surface.

    Wraps a Rust-owned ``Pool`` handle.

    Subclasses: ``UniswapV3Pool`` / ``UniswapV4Pool`` (and anything built
    over the concentrated liquidity handle, e.g. ``AerodromeV3Pool``).
    The subclass state mixins supply ``tick_spacing`` and the identity;
    this base supplies the Rust-owned scalar/tick-map reads + the
    write-back delegation + the unified sparse-word backfill gate.
    """

    _py_pool: Pool
    name: str
    _initial_state_block: int
    address: ChecksummedAddress

    @property
    # Both concentrated liquidity families override this.
    def tick_spacing(self) -> int:  # pragma: no cover
        """Tick spacing (overridden: V3 state mixin, V4 pool key)."""
        msg = "tick_spacing must be provided by the family"
        raise NotImplementedError(msg)

    # --- Rust-owned scalar / tick-map reads --------------------------------

    @property
    def liquidity(self) -> int:
        """Liquidity. Returns: The current active liquidity (from Rust via the handle)."""
        return self._py_pool.concentrated_liquidity().liquidity

    @property
    def sqrt_price_x96(self) -> int:
        """Sqrt price x96. Returns: The current sqrt price as a Q64.96 value (from Rust)."""
        return cast("int", self._py_pool.concentrated_liquidity().sqrt_price_x96)

    @property
    def tick(self) -> int:
        """Tick. Returns: The current tick (from Rust via the handle)."""
        return self._py_pool.concentrated_liquidity().tick

    @property
    def tick_bitmap(self) -> set[int]:
        """Tick bitmap WORD positions known for the pool.

        The load-bearing key set only — the per-word bitmap VALUE is retired
        (it derives from the ``tick_data`` rows). Rust surfaces the words the
        pool knows: every word its ``tick_data`` keys land in, plus (Sparse
        only) the Rust ``known_bitmap_words`` recorded by the registration
        seed / fetch-merge, so a checked-but-empty word survives as a known
        word (the fetch loop breaks). Tracked pools know exactly their tick
        rows' words.
        """
        return {int(word) for word in self._py_pool.concentrated_liquidity().tick_bitmap}

    @property
    def tick_data(self) -> dict[int, LiquidityAtTick]:
        """Tick data.

        Returns a deep-copy snapshot of the Rust-side tick data
        (``{tick: (liquidity_gross, liquidity_net, block)}`` lifted into
        immutable ``LiquidityAtTick`` rows).
        """
        raw = self._py_pool.concentrated_liquidity().tick_data
        return {
            int(tick): (
                LiquidityAtTick(
                    liquidity_net=int(row[1]),
                    liquidity_gross=int(row[0]),
                    block=int(row[2]),
                )
                if not isinstance(row, LiquidityAtTick)
                else row
            )
            for tick, row in raw.items()
        }

    @property
    def update_block(self) -> BlockNumber:  # type: ignore[name-defined]
        """Update block. Returns: The block number of the most recent state update (from Rust)."""
        return self._py_pool.concentrated_liquidity().update_block

    @property
    def initial_state_block(self) -> int:
        """Block number at which the pool's initial state was captured.

        Returns:
            The block number from construction (DB snapshot or RPC fetch).

        """
        return self._initial_state_block

    # --- shared concentrated liquidity state predicate ---------------------

    def swap_is_viable(  # ruff:ignore[no-self-use]
        self,
        state: CLState,
        vector: UniswapPoolSwapVector,  # ruff:ignore[unused-method-argument]
    ) -> bool:
        """Swap is viable.

        Returns:
            True if a swap can proceed with the given state, False otherwise.

        """
        if state.liquidity == 0:
            return False
        return cast("bool", state.sqrt_price_x96 > 1)


__all__ = ["ConcentratedLiquidityCompanion"]
