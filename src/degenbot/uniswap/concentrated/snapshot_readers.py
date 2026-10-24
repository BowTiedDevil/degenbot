"""Database-backed snapshot readers and the liquidity-snapshot facade shared by Uniswap V3/V4.

Snapshot sources route through the Rust ``degenbot-db`` core crate (ADR-005
three-layer architecture); file-based snapshot sources are not supported.

The two concentrated-liquidity families differ in exactly two things: how a
pool identity is spelled (V3 keys a pool by one checksummed address, V4 by a
``(pool manager, pool id)`` pair) and which Rust read seam that spelling calls.
Both are declared once per family as a `DatabaseSnapshotSeams` set, so the
source and facade implementations below are the only implementations.
"""

from __future__ import annotations

from collections import defaultdict, deque
from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol, TypedDict, cast

from degenbot.db import DatabaseSnapshot as _EngineSnapshot
from degenbot.logging import logger
from degenbot.types.concrete import KeyedDefaultDict
from degenbot.uniswap.concentrated.types import LiquidityAtTick

if TYPE_CHECKING:
    import pathlib
    from collections.abc import Callable
    from typing import Any, ClassVar, Self

    from degenbot.types.aliases import BlockNumber, ChainId


class LiquidityMap(TypedDict):
    """LiquidityMap class.

    ``tick_bitmap`` carries only the bitmap WORD positions the snapshot knows
    (the load-bearing key set); the per-word bitmap value is retired (the
    bitmap derives from ``tick_data``).
    """

    tick_bitmap: set[int]
    tick_data: dict[int, LiquidityAtTick]


class LiquidityEvent(Protocol):
    """The fields a liquidity event exposes to the snapshot update flow."""

    block_number: int
    liquidity: int
    tick_lower: int
    tick_upper: int


class LiquiditySnapshotSource(Protocol):
    """A minimal protocol for retrieving pool data from a generic source.

    Any class implementing the protocol must implement these methods,
    transforming data as necessary to return the specified types.
    """

    storage_kind: str
    chain_id: int

    def get_newest_block(self) -> BlockNumber | None:
        """Return newest block.

        Returns:
            The newest block number, or None if unavailable.

        """
        ...

    def get_pools(self) -> set[Any]:
        """Return pools.

        Returns:
            The set of pool keys.

        """
        ...


def _liquidity_map_from_raw(raw: dict[str, Any]) -> LiquidityMap:
    return LiquidityMap(
        tick_bitmap={int(word) for word in raw.get("tick_bitmap", ())},
        tick_data={int(tick): LiquidityAtTick(**entry) for tick, entry in raw["tick_data"].items()},
    )


def _identity_pool_key(pool: str) -> str:
    return pool


@dataclass(frozen=True, slots=True)
class DatabaseSnapshotSeams[K]:
    """The per-family Rust read seams and key adapters of a database snapshot source.

    ``degenbot-db`` exposes one reader per pool-identity spelling, so a family
    substitutes its own seams rather than the source branching on the family.
    Every read seam receives the lazily-opened Rust handle. ``all_maps_key``
    converts one raw bulk-read key into the family's snapshot key, and
    ``db_pool_key`` normalizes one stored pool key as ``get_pools`` reports it
    (identity by default, since only V3 re-checksums stored keys).
    """

    liquidity_map: Callable[[_EngineSnapshot, K], dict[str, Any] | None]
    all_liquidity_maps: Callable[[_EngineSnapshot], dict[Any, dict[int, tuple[int, int]]]]
    newest_block: Callable[[_EngineSnapshot], BlockNumber | None]
    pools: Callable[[_EngineSnapshot], set[str]]
    all_maps_key: Callable[[Any], K]
    db_pool_key: Callable[[str], str] = _identity_pool_key


def _merge_tick_maps(
    pool_snapshot: LiquidityMap,
    *,
    tick_data: dict[int, LiquidityAtTick],
    tick_bitmap: set[int],
) -> None:
    # A liquidity update merges into the map the pool already has. What a pool
    # with no snapshot means is the caller's call, because the exception type
    # is family-specific (UnknownPool for V3, UnknownPoolId for V4).
    pool_snapshot["tick_bitmap"].update(tick_bitmap)
    pool_snapshot["tick_data"].update(tick_data)


class DatabaseSnapshotBase[K]:
    """Snapshot source backed by built-in SQLite database.

    Routes every read through the Rust `degenbot-db` core crate via the
    `_EngineSnapshot` PyO3 seam (ADR-005 three-layer architecture). The
    explicit file path is retained as the authority for the lazy Rust handle.
    A concrete source class supplies its family's `_seams`; everything else is
    family-agnostic and implemented here.
    """

    storage_kind = "db"

    _seams: ClassVar[DatabaseSnapshotSeams[Any]]

    def __init__(self, chain_id: ChainId, *, database_path: pathlib.Path) -> None:
        """Initialize the instance from an explicit file-backed database path."""
        self.database_path = database_path

        self.chain_id = chain_id
        # Open the Rust read handle only when snapshot data is first requested.
        self._rust_snapshot: _EngineSnapshot | None = None
        self._closed = False

    def _rust(self) -> _EngineSnapshot:
        if self._closed:
            msg = "Database snapshot is closed"
            raise RuntimeError(msg)
        if self._rust_snapshot is None:
            self._rust_snapshot = _EngineSnapshot(
                chain_id=self.chain_id,
                database_path=str(self.database_path),
            )
        return self._rust_snapshot

    def close(self) -> None:
        """Release the Rust handle and its SQLite connection.

        Closing is idempotent. A closed snapshot cannot lazily reopen its
        database handle.
        """
        # This wrapper is the handle's sole public owner; dropping the final
        # reference deallocates the PyO3 object and closes its Rust connection.
        self._rust_snapshot = None
        self._closed = True

    def __enter__(self) -> Self:
        """Enter a context that closes the Rust handle on exit.

        Returns:
            This open snapshot.

        Raises:
            RuntimeError: If the snapshot has already been closed.

        """
        if self._closed:
            msg = "Database snapshot is closed"
            raise RuntimeError(msg)
        return self

    def __exit__(self, *exc: object) -> None:
        """Release the Rust handle when leaving the context."""
        self.close()

    def _liquidity_map_for_key(self, pool_key: K) -> LiquidityMap | None:
        raw = self._seams.liquidity_map(self._rust(), pool_key)
        if raw is None:
            return None
        # The Db stores no per-tick block, so stamp the snapshot's bulk-sync
        # block onto every row that does not carry one. ``0`` is a legal
        # stamp, never a sentinel. The Db carries no per-word bitmap value, so
        # there is nothing to stamp for ``tick_bitmap`` (keys only).
        source_block = self._seams.newest_block(self._rust())
        for entry in raw["tick_data"].values():
            entry.setdefault("block", source_block)
        return _liquidity_map_from_raw(raw)

    def get_all_liquidity_maps(self) -> dict[K, dict[int, tuple[int, int]]]:
        """Return all tick data as plain dicts.

        Delegates the bulk read to the Rust core (GIL released during the
        SQLite scan). Returns {pool_key: {tick_index: (liquidity_gross,
        liquidity_net)}}.

        Returns:
            A dict mapping pool keys to tick data dicts.

        """
        return {
            self._seams.all_maps_key(raw_key): ticks
            for raw_key, ticks in self._seams.all_liquidity_maps(self._rust()).items()
        }

    def get_newest_block(self) -> BlockNumber | None:
        """Return newest block.

        Returns:
            The newest block number across all exchanges, or None if unavailable.

        """
        return self._seams.newest_block(self._rust())

    def get_pools(self) -> set[str]:
        """Return pools.

        Returns:
            The set of pool keys from the database.

        """
        return {self._seams.db_pool_key(pool) for pool in self._seams.pools(self._rust())}


class LiquiditySnapshotBase[K, U, E: LiquidityEvent, S: LiquiditySnapshotSource]:
    """Retrieve and maintain liquidity positions for concentrated-liquidity pools."""

    _family_label: ClassVar[str]
    _update_type: ClassVar[Callable[..., Any]]

    def __init__(self, source: S) -> None:
        """Initialize the instance.

        Raises:
            ValueError: If the provided source is uninitialized.

        """
        self._source = source
        self._chain_id = source.chain_id

        if (source_block := source.get_newest_block()) is None:
            msg = "The provided source is uninitialized."
            raise ValueError(msg)
        self.newest_block: BlockNumber = source_block

        self._liquidity_events: dict[K, deque[E]] = defaultdict(deque)
        self._liquidity_snapshot: dict[K, LiquidityMap | None] = KeyedDefaultDict(
            self._source_liquidity_map,
        )

        logger.info(
            f"Loaded Uniswap {self._family_label} LP snapshot from {source.storage_kind} source"
        )

    def _source_liquidity_map(self, pool_key: K) -> LiquidityMap | None:
        raise NotImplementedError(pool_key)

    @property
    def chain_id(self) -> int:
        """Chain id."""
        return self._chain_id

    @property
    def pools(self) -> set[Any]:
        """Pools."""
        return self._source.get_pools()

    def _pending_updates(self, pool_key: K) -> tuple[U, ...]:
        try:
            return cast(
                "tuple[U, ...]",
                tuple(
                    self._update_type(
                        block_number=event.block_number,
                        liquidity=event.liquidity,
                        tick_lower=event.tick_lower,
                        tick_upper=event.tick_upper,
                    )
                    for event in self._liquidity_events[pool_key]
                ),
            )
        finally:
            self._liquidity_events[pool_key].clear()

    def _drain_tick_bitmap(self, pool_key: K) -> set[int] | None:
        pool_snapshot = self._liquidity_snapshot[pool_key]
        if pool_snapshot is None:
            return None
        tick_bitmap = set(pool_snapshot["tick_bitmap"])
        pool_snapshot["tick_bitmap"] = set()
        return tick_bitmap

    def _drain_tick_data(self, pool_key: K) -> dict[int, LiquidityAtTick] | None:
        pool_snapshot = self._liquidity_snapshot[pool_key]
        if pool_snapshot is None:
            return None
        tick_data = pool_snapshot["tick_data"].copy()
        pool_snapshot["tick_data"] = {}
        return tick_data
