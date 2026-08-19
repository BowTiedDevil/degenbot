"""Uniswap V3 pool snapshot and subscription handler."""

from __future__ import annotations

import pathlib
from collections import defaultdict, deque
from typing import TYPE_CHECKING, Any, Protocol, TypedDict

import pydantic_core

from degenbot._ffi.db import DatabaseSnapshot as _EngineSnapshot
from degenbot.checksum_cache import get_checksum_address
from degenbot.database.operations import get_scoped_sqlite_session
from degenbot.exceptions.pool import UnknownPool
from degenbot.logging import logger
from degenbot.types.concrete import KeyedDefaultDict
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v3_types import UniswapV3LiquidityEvent, UniswapV3PoolLiquidityMappingUpdate

if TYPE_CHECKING:
    from sqlalchemy.orm import Session, scoped_session

    from degenbot.database.session_manager import DatabaseSessionManager
    from degenbot.types.aliases import BlockNumber, ChainId
    from degenbot.types.chain import ChecksummedAddress, HexAddress


class LiquidityMap(TypedDict):
    """LiquidityMap class."""

    tick_bitmap: dict[int, BitmapAtWord]
    tick_data: dict[int, LiquidityAtTick]


class UniswapV3LiquiditySnapshotSource(Protocol):
    """A minimal protocol for retrieving pool data from a generic source.

    Any class implementing the protocol must implement these methods,
    transforming data as necessary to return the specified types.
    """

    storage_kind: str
    chain_id: int

    # Any class implementing the protocol must implement these methods, transforming data as
    # necessary to return the specified types.
    def get_liquidity_map(self, pool_address: ChecksummedAddress) -> LiquidityMap | None:
        """Return liquidity map.

        Returns:
            The liquidity map for the pool, or None if not found.

        """
        ...

    def get_newest_block(self) -> BlockNumber | None:
        """Return newest block.

        Returns:
            The newest block number, or None if unavailable.

        """
        ...

    def get_pools(self) -> set[ChecksummedAddress]:
        """Return pools.

        Returns:
            The set of pool addresses.

        """
        ...


class MonolithicJsonFileSnapshot:
    """A pool liquidity source backed by a single JSON file.

    {
        "snapshot_block": int,
        "chain_id": int,
        "0xPoolAddress1": {
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
        "0xPoolAddress2": { ... },
        "0xPoolAddress3": { ... },
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

    def get_liquidity_map(self, pool_address: ChecksummedAddress) -> LiquidityMap | None:
        """Return liquidity map.

        Returns:
            The liquidity map for the pool, or None if not found.

        """
        if pool_address not in self._file_snapshot:
            return None

        return LiquidityMap(
            tick_bitmap={
                int(k): BitmapAtWord(**v)
                for k, v in self._file_snapshot[pool_address]["tick_bitmap"].items()
            },
            tick_data={
                int(k): LiquidityAtTick(**v)
                for k, v in self._file_snapshot[pool_address]["tick_data"].items()
            },
        )

    def get_newest_block(self) -> BlockNumber | None:
        """Return newest block.

        Returns:
            The snapshot block number, or None if unavailable.

        """
        newest_block = self._file_snapshot.get("snapshot_block")
        if newest_block is None:
            return None
        return int(newest_block)

    def get_pools(self) -> set[ChecksummedAddress]:
        """Return pools.

        Returns:
            The set of pool addresses from the snapshot.

        """
        # all top-level keys except metadata entries
        return {
            get_checksum_address(key)
            for key in self._file_snapshot
            if key not in {"chain_id", "snapshot_block"}
        }


class IndividualJsonFileSnapshot:
    """Snapshot source backed by a directory of JSON files with this tree structure.

        /path/to/snapshots/
        ├── _metadata.json              -> { "block": int, "chain_id": int }
        ├── 0xPoolAddress1.json         -> { "tick_bitmap": {...}, "tick_data": {...} }
        ├── 0xPoolAddress2.json         -> { "tick_bitmap": {...}, "tick_data": {...} }
        └── 0xPoolAddress3.json         -> { "tick_bitmap": {...}, "tick_data": {...} }

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

    def get_pools(self) -> set[ChecksummedAddress]:
        """Return pools.

        Returns:
            The set of pool addresses from file stems.

        """
        return {get_checksum_address(pool_file.stem) for pool_file in self._dir.glob("0x*.json")}

    def get_liquidity_map(self, pool_address: ChecksummedAddress) -> LiquidityMap | None:
        """Return liquidity map.

        Returns:
            The liquidity map for the pool, or None if not found.

        """
        pool_path = self._dir / f"{pool_address}.json"
        if not pool_path.exists():
            return None

        pool_liquidity_snapshot = pydantic_core.from_json(pool_path.read_bytes())
        return LiquidityMap(
            tick_bitmap={
                int(k): BitmapAtWord(**v) for k, v in pool_liquidity_snapshot["tick_bitmap"].items()
            },
            tick_data={
                int(k): LiquidityAtTick(**v)
                for k, v in pool_liquidity_snapshot["tick_data"].items()
            },
        )


class DatabaseSnapshot:
    """Snapshot source backed by built-in SQLite database.

    Routes every read through the Rust `degenbot-db` core crate via the
    `_EngineSnapshot` PyO3 seam (ADR-005 three-layer architecture). The
    `session` / `database_path` are retained for explicit-deps construction +
    migration tooling; reads no longer use the SQLAlchemy session.
    """

    storage_kind = "db"
    session: DatabaseSessionManager | scoped_session[Session]

    def __init__(
        self,
        chain_id: ChainId,
        *,
        db: DatabaseSessionManager | None = None,
        database_path: pathlib.Path | None = None,
    ) -> None:
        """Initialize the instance.

        Raises:
            ValueError: If neither db nor database_path is provided.

        """
        if db is not None:
            self.session = db
            self.database_path = database_path or pathlib.Path()
        else:
            if database_path is None:
                msg = "Either db or database_path must be provided"
                raise ValueError(msg)
            self.session = get_scoped_sqlite_session(database_path)
            self.database_path = database_path

        self.chain_id = chain_id
        # Lazily-constructed Rust read handle (opened on first read so a
        # `db=-only` construction with no resolvable path doesn't fail until
        # a read is actually attempted).
        self._rust_snapshot: _EngineSnapshot | None = None

    def _rust_db_path(self) -> pathlib.Path:
        """Resolve the SQLite file path the Rust reader will open.

        Prefer an explicit `database_path`; fall back to the bound engine's
        file URL (the `db=bot.db` case where no path was passed).

        Returns:
            The resolved database file path.

        Raises:
            ValueError: If no file path can be resolved (e.g. an in-memory engine).

        """
        if self.database_path and self.database_path.name:
            return self.database_path
        engine = getattr(self.session, "_engine", None)
        if engine is not None:
            db_path = engine.url.database
            if db_path and db_path != ":memory:":
                return pathlib.Path(db_path)
        msg = "database_path is required for Rust-backed snapshot reads"
        raise ValueError(msg)

    def _rust(self) -> _EngineSnapshot:
        if self._rust_snapshot is None:
            self._rust_snapshot = _EngineSnapshot(
                chain_id=self.chain_id,
                database_path=str(self._rust_db_path()),
            )
        return self._rust_snapshot

    def get_liquidity_map(self, pool_address: ChecksummedAddress) -> LiquidityMap | None:
        """Return liquidity map.

        Returns:
            The liquidity map for the pool, or None if not found.

        """
        raw = self._rust().get_liquidity_map_v3(get_checksum_address(pool_address))
        if raw is None:
            return None
        return LiquidityMap(
            tick_bitmap={
                int(word): BitmapAtWord(**entry) for word, entry in raw["tick_bitmap"].items()
            },
            tick_data={
                int(tick): LiquidityAtTick(**entry) for tick, entry in raw["tick_data"].items()
            },
        )

    def get_all_liquidity_maps(self) -> dict[ChecksummedAddress, dict[int, tuple[int, int]]]:
        """Return all V3 tick data as plain dicts.

        Delegates the bulk read to the Rust core (GIL released during the
        SQLite scan). Returns {pool_address: {tick_index: (liquidity_gross, liquidity_net)}}.

        Returns:
            A dict mapping pool addresses to tick data dicts.

        """
        return {
            get_checksum_address(addr): ticks
            for addr, ticks in self._rust().get_all_liquidity_maps_v3().items()
        }

    def get_newest_block(self) -> BlockNumber | None:
        """Return newest block.

        Returns:
            The newest block number across all V3 exchanges, or None if unavailable.

        """
        return self._rust().get_newest_block_v3()

    def get_pools(self) -> set[ChecksummedAddress]:
        """Return pools.

        Returns:
            The set of V3 pool addresses from the database.

        """
        return {get_checksum_address(p) for p in self._rust().get_pools_v3()}


class UniswapV3LiquiditySnapshot:
    """Retrieve and maintain liquidity positions for Uniswap V3 pools."""

    def __init__(self, source: UniswapV3LiquiditySnapshotSource) -> None:
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

        self._liquidity_events: dict[ChecksummedAddress, deque[UniswapV3LiquidityEvent]] = (
            defaultdict(
                deque,
            )
        )
        self._liquidity_snapshot: dict[ChecksummedAddress, LiquidityMap | None] = KeyedDefaultDict(
            lambda key: self._source.get_liquidity_map(get_checksum_address(key)),
        )

        logger.info(f"Loaded Uniswap V3 LP snapshot from {source.storage_kind} source")

    @property
    def chain_id(self) -> int:
        """Chain id."""
        return self._chain_id

    @property
    def pools(self) -> set[ChecksummedAddress]:
        """Pools."""
        return self._source.get_pools()

    def pending_updates(
        self,
        pool_address: str,
    ) -> tuple[UniswapV3PoolLiquidityMappingUpdate, ...]:
        """Consume pending liquidity updates for the pool.

        Returns:
            A tuple of liquidity mapping updates for the pool.

        """
        pool_key = get_checksum_address(pool_address)

        try:
            return tuple(
                UniswapV3PoolLiquidityMappingUpdate(
                    block_number=event.block_number,
                    liquidity=event.liquidity,
                    tick_lower=event.tick_lower,
                    tick_upper=event.tick_upper,
                )
                for event in self._liquidity_events[pool_key]
            )
        finally:
            self._liquidity_events[pool_key].clear()

    def tick_bitmap(self, pool_address: str | bytes) -> dict[int, BitmapAtWord] | None:
        """Consume the tick initialization bitmaps for the pool.

        Returns:
            The tick bitmap dict, or None if no snapshot exists.

        """
        pool_address = get_checksum_address(pool_address)

        pool_snapshot = self._liquidity_snapshot[pool_address]
        if pool_snapshot is None:
            return None

        tick_bitmap = pool_snapshot["tick_bitmap"].copy()
        pool_snapshot["tick_bitmap"] = {}
        return tick_bitmap

    def tick_data(self, pool_address: str | bytes) -> dict[int, LiquidityAtTick] | None:
        """Consume the liquidity mapping for the pool.

        Returns:
            The tick data dict, or None if no snapshot exists.

        """
        pool_address = get_checksum_address(pool_address)

        pool_snapshot = self._liquidity_snapshot[pool_address]
        if pool_snapshot is None:
            return None

        tick_data = pool_snapshot["tick_data"].copy()
        pool_snapshot["tick_data"] = {}
        return tick_data

    def update(
        self,
        pool: HexAddress,
        tick_data: dict[int, LiquidityAtTick],
        tick_bitmap: dict[int, BitmapAtWord],
    ) -> None:
        """Update the liquidity mapping for the pool.

        Raises:
            UnknownPool: If the pool has no snapshot.

        """
        pool_key = get_checksum_address(pool)

        pool_snapshot = self._liquidity_snapshot[pool_key]
        if pool_snapshot is None:
            raise UnknownPool(pool_key)

        pool_snapshot["tick_bitmap"].update(tick_bitmap)
        pool_snapshot["tick_data"].update(tick_data)
