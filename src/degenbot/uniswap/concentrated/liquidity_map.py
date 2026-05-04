"""
Immutable snapshot of a concentrated-liquidity pool's tick data.

The simulator consumes a *snapshot* so that:
- It is deterministic: same snapshot → same result.
- Sparse-map fetches do not mutate the pool's internal state.
- The pool retries outside the loop when data is missing.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol, Self

from degenbot.exceptions.liquidity_pool import LiquidityMapWordMissing
from degenbot.uniswap.v3_libraries.tick_bitmap import (
    gen_ticks,
    next_initialized_tick_within_one_word,
)

if TYPE_CHECKING:
    from collections.abc import Generator, Mapping


class _HasPoolLiquidityMap(Protocol):
    """Duck-type for V3/V4 pools that expose tick mapping attributes."""

    @property
    def tick_data(self) -> Mapping[int, object]: ...

    @property
    def tick_bitmap(self) -> Mapping[int, object]: ...

    @property
    def tick_spacing(self) -> int: ...

    @property
    def sparse_liquidity_map(self) -> bool: ...


class _HasTickData(Protocol):
    """Duck-type for state objects carrying tick bitmap + data."""

    @property
    def tick_data(self) -> Mapping[int, object]: ...

    @property
    def tick_bitmap(self) -> Mapping[int, object]: ...


class _HasLiquidityNet(Protocol):
    """Duck-type for tick entries that carry a ``liquidity_net`` field."""

    @property
    def liquidity_net(self) -> int: ...


class MissingLiquidityData(LiquidityMapWordMissing):
    """
    Raised when the simulator needs a tick bitmap word that is not in the snapshot.

    The caller (pool or state manager) must fetch the missing data and retry.
    """


@dataclass(frozen=True, slots=True)
class LiquidityMapSnapshot:
    """
    Frozen view of tick bitmap + tick data + spacing.

    Supports both V3-style and V4-style inner types through duck-typing.
    The simulator never mutates these dicts.
    """

    tick_data: Mapping[int, object]
    tick_bitmap: Mapping[int, object]
    tick_spacing: int
    sparse: bool

    @classmethod
    def from_pool(cls, pool: _HasPoolLiquidityMap) -> Self:
        """Build a snapshot from any pool that exposes V3/V4-style attributes."""
        return cls(
            tick_data=pool.tick_data,
            tick_bitmap=pool.tick_bitmap,
            tick_spacing=pool.tick_spacing,
            sparse=pool.sparse_liquidity_map,
        )

    @classmethod
    def from_state(cls, state: _HasTickData, *, tick_spacing: int, sparse: bool) -> Self:
        """Build a snapshot from a state object."""
        return cls(
            tick_data=state.tick_data,
            tick_bitmap=state.tick_bitmap,
            tick_spacing=tick_spacing,
            sparse=sparse,
        )

    def next_initialized_tick(
        self,
        *,
        tick: int,
        zero_for_one: bool,
    ) -> tuple[int, bool]:
        """Return the next initialized tick along the swap path.

        Raises ``MissingLiquidityData`` if the required bitmap word is absent
        in a sparse mapping.
        """
        # Detect V4-style vs V3-style by presence of `_next_initialized_tick_within_one_word`
        # attribute on the bitmap entries. Both follow the same function signature for
        # `next_initialized_tick_within_one_word` — we prefer V4 implementations when both are
        # importable because they work on the subclasses, but practically they are compatible.
        if self.sparse:
            # Use the sparse path — may raise MissingLiquidityData
            try:
                return next_initialized_tick_within_one_word(
                    tick_bitmap=self.tick_bitmap,  # type: ignore[arg-type]
                    tick_data=self.tick_data,  # type: ignore[arg-type]
                    tick=tick,
                    tick_spacing=self.tick_spacing,
                    less_than_or_equal=zero_for_one,
                )
            except LiquidityMapWordMissing as exc:
                raise MissingLiquidityData(word=exc.word) from exc
        # Use the full-map optimized path
        return next(
            gen_ticks(
                tick_data=self.tick_data,  # type: ignore[arg-type]
                starting_tick=tick,
                tick_spacing=self.tick_spacing,
                less_than_or_equal=zero_for_one,
            )
        )

    def ticks_along_path(
        self,
        *,
        tick_start: int,
        zero_for_one: bool,
    ) -> Generator[tuple[int, bool], None, None]:
        """Yield all ticks along the swap path (full map only).

        Only valid when ``self.sparse`` is ``False``.
        """
        if self.sparse:
            msg = "ticks_along_path requires a non-sparse map"
            raise RuntimeError(msg)
        yield from gen_ticks(
            tick_data=self.tick_data,  # type: ignore[arg-type]
            starting_tick=tick_start,
            tick_spacing=self.tick_spacing,
            less_than_or_equal=zero_for_one,
        )
