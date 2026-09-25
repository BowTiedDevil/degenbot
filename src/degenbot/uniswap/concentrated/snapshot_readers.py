"""Generic snapshot readers and liquidity-snapshot facade shared by Uniswap V3/V4."""

from __future__ import annotations

import pathlib
from collections import defaultdict, deque
from typing import TYPE_CHECKING, Protocol, TypedDict

import pydantic_core

from degenbot.checksum_cache import get_checksum_address
from degenbot.db import DatabaseSnapshot as _EngineSnapshot
from degenbot.logging import logger
from degenbot.types.concrete import KeyedDefaultDict
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick

if TYPE_CHECKING:
    from collections.abc import Callable
    from typing import Any, ClassVar, Self

    from degenbot.types.aliases import BlockNumber, ChainId


class LiquidityMap(TypedDict):
    """LiquidityMap class."""

    tick_bitmap: dict[int, BitmapAtWord]
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
        tick_bitmap={
            int(word): BitmapAtWord(**entry) for word, entry in raw["tick_bitmap"].items()
        },
        tick_data={int(tick): LiquidityAtTick(**entry) for tick, entry in raw["tick_data"].items()},
    )


class MonolithicJsonFileSnapshotBase:
    """A pool liquidity source backed by a single JSON file.

    {
        "snapshot_block": int,
        "chain_id": int,
        "0xPoolKey1": {
            "tick_bitmap": {
                <word>: {
                    'bitmap': <value>,
                    'block': <value>,
                },
                ...
            },
            "tick_data": {
                <tick>: {
                    'liquidity_gross: <value>,
                    'liquidity_net': <value>,
                    'block: <value>,
                }
            }
        },
        "0xPoolKey2": { ... },
        "0xPoolKey3": { ... },
        ...
    }.
    """

    storage_kind = "file"

    def __init__(self, path: pathlib.Path | str) -> None:
        """Initialize the instance."""
        path = pathlib.Path(path).expanduser().absolute()
        self._path = path
        self._file_snapshot: dict[str, Any] = pydantic_core.from_json(path.read_bytes())
        self.chain_id: int = self._file_snapshot["chain_id"]

    def _liquidity_map_for(self, pool_key: str) -> LiquidityMap | None:
        if pool_key not in self._file_snapshot:
            return None
        return _liquidity_map_from_raw(self._file_snapshot[pool_key])

    def get_newest_block(self) -> BlockNumber | None:
        """Return newest block.

        Returns:
            The snapshot block number, or None if unavailable.

        """
        newest_block = self._file_snapshot.get("snapshot_block")
        if newest_block is None:
            return None
        return int(newest_block)

    def get_pools(self) -> set[str]:
        """Return pools.

        Returns:
            The set of pool keys from the snapshot.

        """
        # all top-level keys except metadata entries
        return {
            get_checksum_address(key)
            for key in self._file_snapshot
            if key not in {"chain_id", "snapshot_block"}
        }


class IndividualJsonFileSnapshotBase:
    """Snapshot source backed by a directory of JSON files with this tree structure.

        /path/to/snapshots/
        |-- _metadata.json              -> { "block": int, "chain_id": int }
        |-- 0xPoolKey1.json             -> { "tick_bitmap": {...}, "tick_data": {...} }
        |-- 0xPoolKey2.json             -> { "tick_bitmap": {...}, "tick_data": {...} }
        |-- 0xPoolKey3.json             -> { "tick_bitmap": {...}, "tick_data": {...} }

    Each pool file contains the same structure as the monolithic snapshot's per-pool entries.
    """

    storage_kind = "dir"

    def __init__(self, path: pathlib.Path | str) -> None:
        """Initialize the instance."""
        dir_path = pathlib.Path(path).expanduser().absolute()
        assert dir_path.exists()
        assert dir_path.is_dir()
        self._dir = dir_path

        metadata_path = self._dir / "_metadata.json"
        self._metadata: dict[str, Any] = pydantic_core.from_json(metadata_path.read_bytes())
        self.chain_id: int = self._metadata["chain_id"]

    def get_newest_block(self) -> BlockNumber | None:
        """Return newest block.

        Returns:
            The block number from metadata, or None if unavailable.

        """
        newest_block = self._metadata.get("block")
        if newest_block is None:
            return None
        return int(newest_block)

    def get_pools(self) -> set[str]:
        """Return pools.

        Returns:
            The set of pool keys from file stems.

        """
        return {get_checksum_address(pool_file.stem) for pool_file in self._dir.glob("0x*.json")}

    def get_liquidity_map(self, pool_address: str) -> LiquidityMap | None:
        """Return liquidity map.

        Returns:
            The liquidity map for the pool, or None if not found.

        """
        pool_path = self._dir / f"{pool_address}.json"
        if not pool_path.exists():
            return None
        return _liquidity_map_from_raw(pydantic_core.from_json(pool_path.read_bytes()))


class DatabaseSnapshotBase[K]:
    """Snapshot source backed by built-in SQLite database.

    Routes every read through the Rust `degenbot-db` core crate via the
    `_EngineSnapshot` PyO3 seam (ADR-005 three-layer architecture). The
    explicit file path is retained as the authority for the lazy Rust handle.
    """

    storage_kind = "db"

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

    def _seam_liquidity_map(self, pool_key: K) -> dict[str, Any] | None:
        raise NotImplementedError(pool_key)

    def _seam_all_liquidity_maps(self) -> dict[Any, dict[int, tuple[int, int]]]:
        raise NotImplementedError

    def _seam_newest_block(self) -> BlockNumber | None:
        raise NotImplementedError

    def _seam_pools(self) -> set[str]:
        raise NotImplementedError

    def _all_maps_key(self, raw_key: object) -> K:
        raise NotImplementedError(raw_key)

    def _db_pool_key(self, pool: str) -> str:
        raise NotImplementedError(pool)

    def _liquidity_map_for_key(self, pool_key: K) -> LiquidityMap | None:
        raw = self._seam_liquidity_map(pool_key)
        if raw is None:
            return None
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
            self._all_maps_key(raw_key): ticks
            for raw_key, ticks in self._seam_all_liquidity_maps().items()
        }

    def get_newest_block(self) -> BlockNumber | None:
        """Return newest block.

        Returns:
            The newest block number across all exchanges, or None if unavailable.

        """
        return self._seam_newest_block()

    def get_pools(self) -> set[str]:
        """Return pools.

        Returns:
            The set of pool keys from the database.

        """
        return {self._db_pool_key(pool) for pool in self._seam_pools()}


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
            return tuple(
                self._update_type(
                    block_number=event.block_number,
                    liquidity=event.liquidity,
                    tick_lower=event.tick_lower,
                    tick_upper=event.tick_upper,
                )
                for event in self._liquidity_events[pool_key]
            )
        finally:
            self._liquidity_events[pool_key].clear()

    def _drain_tick_bitmap(self, pool_key: K) -> dict[int, BitmapAtWord] | None:
        pool_snapshot = self._liquidity_snapshot[pool_key]
        if pool_snapshot is None:
            return None
        tick_bitmap = pool_snapshot["tick_bitmap"].copy()
        pool_snapshot["tick_bitmap"] = {}
        return tick_bitmap

    def _drain_tick_data(self, pool_key: K) -> dict[int, LiquidityAtTick] | None:
        pool_snapshot = self._liquidity_snapshot[pool_key]
        if pool_snapshot is None:
            return None
        tick_data = pool_snapshot["tick_data"].copy()
        pool_snapshot["tick_data"] = {}
        return tick_data
