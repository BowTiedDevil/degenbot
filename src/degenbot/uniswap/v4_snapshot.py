"""Uniswap V4 pool snapshot and subscription handler."""

import asyncio
import pathlib
from collections import defaultdict
from typing import Any, Protocol, TypedDict

import pydantic_core
import tqdm
import tqdm.asyncio
from eth_abi.abi import decode as abi_decode
from eth_typing import ChecksumAddress, HexAddress, HexStr
from hexbytes import HexBytes
from sqlalchemy.orm import Session, scoped_session

from degenbot.checksum_cache import get_checksum_address
from degenbot.crypto import event_topic
from degenbot.database.operations import get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.db import PyDatabaseSnapshot
from degenbot.exceptions.pool import UnknownPoolId
from degenbot.logging import logger
from degenbot.provider import AlloyProvider, AsyncAlloyProvider
from degenbot.provider.log_fetching import fetch_logs_retrying, fetch_logs_retrying_async
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import KeyedDefaultDict
from degenbot.types.rpc_types import LogReceipt
from degenbot.uniswap.abi import UNISWAP_V4_POOL_MANAGER_ABI
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v4_types import (
    UniswapV4LiquidityEvent,
    UniswapV4PoolLiquidityMappingUpdate,
)

type PoolManagerAddress = ChecksumAddress
type PoolId = str
type ManagedPoolIdentifier = tuple[PoolManagerAddress, PoolId]


class LiquidityMap(TypedDict):
    """LiquidityMap class."""

    tick_bitmap: dict[int, BitmapAtWord]
    tick_data: dict[int, LiquidityAtTick]


class UniswapV4LiquiditySnapshotSource(Protocol):
    """A minimal protocol allowing the UniswapV4LiquiditySnapshot class to retrieve pool data.

    from a generic source.
    """

    storage_kind: str
    chain_id: int

    # Any class implementing the protocol must implement these methods, transforming data as
    # necessary to return the specified types.
    def get_liquidity_map(
        self,
        pool_manager: ChecksumAddress,
        pool_id: bytes | str,
    ) -> LiquidityMap | None:
        """Return liquidity map."""
        ...

    def get_newest_block(self) -> BlockNumber | None:
        """Return newest block."""
        ...

    def get_pools(self) -> set[PoolId]:
        """Return pools."""
        ...


class MonolithicJsonFileSnapshot:
    """A pool liquidity source backed by a single JSON file with this structure.

    {
        "snapshot_block": int,
        "chain_id": int,
        "0xPoolId1": {
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
        "0xPoolId2": { ... },
        "0xPoolId3": { ... },
        ...
    }.
    """

    storage_kind = "file"

    def __init__(self, path: pathlib.Path | str) -> None:
        """Initialize the instance."""
        path = pathlib.Path(path).expanduser().absolute()
        self._path = path
        self._file_snapshot: dict[PoolId, Any] = pydantic_core.from_json(path.read_bytes())
        self.chain_id: int = self._file_snapshot["chain_id"]

    def get_liquidity_map(
        self,
        pool_manager: ChecksumAddress,  # noqa: ARG002
        pool_id: bytes | str,
    ) -> LiquidityMap | None:
        """Return liquidity map.

        Returns:
            The liquidity map for the pool, or None if the pool is not found.

        """
        pool_id = HexBytes(pool_id).to_0x_hex()

        if pool_id not in self._file_snapshot:
            return None

        return LiquidityMap(
            tick_bitmap={
                int(k): BitmapAtWord(**v)
                for k, v in self._file_snapshot[pool_id]["tick_bitmap"].items()
            },
            tick_data={
                int(k): LiquidityAtTick(**v)
                for k, v in self._file_snapshot[pool_id]["tick_data"].items()
            },
        )

    def get_newest_block(self) -> BlockNumber | None:
        """Return newest block.

        Returns:
            The block number of the newest snapshot, or None if unavailable.

        """
        newest_block = self._file_snapshot.get("snapshot_block")
        if newest_block is None:
            return None
        return int(newest_block)

    def get_pools(self) -> set[PoolId]:
        """Return pools.

        Returns:
            Set of pool IDs from all top-level keys except metadata entries.

        """
        # all top-level keys except metadata entries
        return {
            get_checksum_address(key)
            for key in self._file_snapshot
            if key not in {"chain_id", "snapshot_block"}
        }


class DatabaseSnapshot:
    """Snapshot source backed by built-in SQLite database.

    Routes every read through the Rust `degenbot-db` core crate via the
    `PyDatabaseSnapshot` PyO3 seam (ADR-005 three-layer architecture). The
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
        self._rust_snapshot: PyDatabaseSnapshot | None = None

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

    def _rust(self) -> PyDatabaseSnapshot:
        if self._rust_snapshot is None:
            self._rust_snapshot = PyDatabaseSnapshot(
                chain_id=self.chain_id,
                database_path=str(self._rust_db_path()),
            )
        return self._rust_snapshot

    def get_liquidity_map(
        self,
        pool_manager: ChecksumAddress,
        pool_id: bytes | str,
    ) -> LiquidityMap | None:
        """Return liquidity map.

        Returns:
            The liquidity map for the pool, or None if not found in the database.

        """
        raw = self._rust().get_liquidity_map_v4(get_checksum_address(pool_manager), pool_id)
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

    def get_all_liquidity_maps(
        self,
    ) -> dict[tuple[ChecksumAddress, str], dict[int, tuple[int, int]]]:
        """Return all V4 tick data as plain dicts.

        Delegates the bulk read to the Rust core (GIL released during the
        SQLite scan). Returns {(pm_address, pool_id_hex): {tick_index:
        (liquidity_gross, liquidity_net)}}.

        Returns:
            A dict mapping (pm_address, pool_id) tuples to tick data dicts.

        """
        result: dict[tuple[ChecksumAddress, str], dict[int, tuple[int, int]]] = {}
        for (pm_address, pool_hash), ticks in self._rust().get_all_liquidity_maps_v4().items():
            result[get_checksum_address(pm_address), pool_hash] = ticks
        return result

    def get_newest_block(self) -> BlockNumber | None:
        """Return newest block.

        Returns:
            The block number of the newest update, or None if unavailable.

        """
        return self._rust().get_newest_block_v4()

    def get_pools(self) -> set[PoolId]:
        """Return pools.

        Returns:
            Set of pool IDs stored in the database.

        """
        return set(self._rust().get_pools_v4())


class UniswapV4LiquiditySnapshot:
    """Retrieve and maintain liquidity positions for Uniswap V4 pools."""

    UNISWAP_V4_MODIFYLIQUIDITY_EVENT_HASH = event_topic(
        next(
            e
            for e in UNISWAP_V4_POOL_MANAGER_ABI
            if e.get("name") == "ModifyLiquidity" and e.get("type") == "event"
        )
    )

    def __init__(self, source: UniswapV4LiquiditySnapshotSource) -> None:
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

        self._liquidity_events: dict[
            tuple[ChecksumAddress, PoolId],
            list[UniswapV4LiquidityEvent],
        ] = defaultdict(list)
        self._liquidity_snapshot: dict[
            tuple[ChecksumAddress, PoolId],
            LiquidityMap | None,
        ] = KeyedDefaultDict(
            lambda key: self._source.get_liquidity_map(
                get_checksum_address(key[0]),
                HexBytes(key[1]).to_0x_hex(),
            ),
        )

        logger.info(f"Loaded Uniswap V4 LP snapshot from {source.storage_kind} source")

    @property
    def chain_id(self) -> int:
        """Chain id."""
        return self._chain_id

    @property
    def pools(self) -> set[ManagedPoolIdentifier]:
        """Pools."""
        return {(pool_manager, pool_id) for pool_manager, pool_id in self._liquidity_snapshot}

    @staticmethod
    def _process_liquidity_event_log(
        log: LogReceipt,
    ) -> tuple[ChecksumAddress, PoolId, UniswapV4LiquidityEvent]:
        """Decode an event log and convert to an address, pool ID, and a.

        `UniswapV4LiquidityEvent` for processing with
        `UniswapV4Pool.update_liquidity_map`.

        Returns:
            A tuple of (pool_manager_address, pool_id, liquidity_event).

        """
        # ref: https://github.com/Uniswap/v4-core/blob/main/src/interfaces/IPoolManager.sol
        # event ModifyLiquidity(
        #     PoolId indexed id,
        #     address indexed sender,
        #     int24 tickLower,
        #     int24 tickUpper,
        #     int256 liquidityDelta,
        #     bytes32 salt,
        # );

        assert not log["removed"]

        tick_lower, tick_upper, liquidity_delta, _ = abi_decode(
            types=["int24", "int24", "int256", "bytes32"],
            data=log["data"],
        )

        return (
            log["address"],  # pool manager address
            log["topics"][1].to_0x_hex(),  # pool ID
            UniswapV4LiquidityEvent(
                block_number=log["blockNumber"],
                tx_index=log["transactionIndex"],
                log_index=log["logIndex"],
                liquidity=liquidity_delta,
                tick_lower=tick_lower,
                tick_upper=tick_upper,
            ),
        )

    def fetch_new_events(
        self,
        to_block: BlockNumber,
        *,
        provider: AlloyProvider,
        blocks_per_request: int | None = None,
    ) -> None:
        """Fetch liquidity events from the block following the last-known event to the target block.

        using `eth_getLogs`. Blocks per request will be capped at `blocks_per_request`.
        """
        logger.info(f"Updating Uniswap V4 snapshot from block {self.newest_block} to {to_block}")

        event_logs = fetch_logs_retrying(
            provider=provider,
            start_block=self.newest_block + 1,
            end_block=to_block,
            max_blocks_per_request=blocks_per_request,
            topic_signature=[
                [
                    self.UNISWAP_V4_MODIFYLIQUIDITY_EVENT_HASH,
                ],  # match topic0: ModifyLiquidity
            ],
        )

        for event_log in tqdm.tqdm(
            event_logs,
            desc="Processing liquidity events",
            unit="event",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            # Ignores zero-amount events
            if any(event_log["data"][64:96]):
                pool_manager_address, pool_id, liquidity_event = self._process_liquidity_event_log(
                    event_log,
                )
                self._liquidity_events[pool_manager_address, pool_id].append(liquidity_event)

        self.newest_block = to_block

    async def fetch_new_events_async(
        self,
        to_block: BlockNumber,
        *,
        provider: AsyncAlloyProvider,
        blocks_per_request: int | None = None,
    ) -> None:
        """Async version of fetch_new_events.

        Fetch liquidity events from the block following the last-known event to the target block
        using `eth_getLogs` via the async provider. Blocks per request will be capped at
        `blocks_per_request`.
        """
        logger.info(f"Updating Uniswap V4 snapshot from block {self.newest_block} to {to_block}")

        event_logs = await fetch_logs_retrying_async(
            provider=provider,
            start_block=self.newest_block + 1,
            end_block=to_block,
            max_blocks_per_request=blocks_per_request,
            topic_signature=[
                [
                    self.UNISWAP_V4_MODIFYLIQUIDITY_EVENT_HASH,
                ],  # match topic0: ModifyLiquidity
            ],
        )

        async for event_log in tqdm.asyncio.tqdm(
            event_logs,
            desc="Processing liquidity events",
            unit="event",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            await asyncio.sleep(0)

            # Ignores zero-amount events
            if any(event_log["data"][64:96]):
                pool_manager_address, pool_id, liquidity_event = self._process_liquidity_event_log(
                    event_log,
                )
                self._liquidity_events[pool_manager_address, pool_id].append(liquidity_event)

        self.newest_block = to_block

    def pending_updates(
        self,
        pool_manager: HexAddress | bytes,
        pool_id: HexStr | bytes,
    ) -> tuple[UniswapV4PoolLiquidityMappingUpdate, ...]:
        """Consume and return all pending liquidity events for this pool.

        Returns:
            Tuple of pending liquidity mapping updates for the pool.

        """
        pool_key = get_checksum_address(pool_manager), HexBytes(pool_id).to_0x_hex()
        pending_events = tuple(self._liquidity_events[pool_key])
        self._liquidity_events[pool_key] = []

        return tuple(
            UniswapV4PoolLiquidityMappingUpdate(
                block_number=event.block_number,
                liquidity=event.liquidity,
                tick_lower=event.tick_lower,
                tick_upper=event.tick_upper,
            )
            for event in pending_events
        )

    def tick_bitmap(
        self,
        pool_manager: HexAddress | bytes,
        pool_id: HexStr | bytes,
    ) -> dict[int, BitmapAtWord] | None:
        """Consume the tick initialization bitmaps for the pool.

        Returns:
            The tick bitmap dict, or None if the pool snapshot is unavailable.

        """
        pool_key: ManagedPoolIdentifier = (
            get_checksum_address(pool_manager),
            HexBytes(pool_id).to_0x_hex(),
        )

        pool_snapshot = self._liquidity_snapshot[pool_key]
        if pool_snapshot is None:
            return None

        tick_bitmap = pool_snapshot["tick_bitmap"].copy()
        pool_snapshot["tick_bitmap"] = {}
        return tick_bitmap

    def tick_data(
        self,
        pool_manager: HexAddress | bytes,
        pool_id: HexStr | bytes,
    ) -> dict[int, LiquidityAtTick] | None:
        """Consume the liquidity mapping for the pool.

        Returns:
            The tick data dict, or None if the pool snapshot is unavailable.

        """
        pool_key: ManagedPoolIdentifier = (
            get_checksum_address(pool_manager),
            HexBytes(pool_id).to_0x_hex(),
        )

        pool_snapshot = self._liquidity_snapshot[pool_key]
        if pool_snapshot is None:
            return None

        tick_data = pool_snapshot["tick_data"].copy()
        pool_snapshot["tick_data"] = {}
        return tick_data

    def update(
        self,
        pool_manager: HexAddress | bytes,
        pool_id: HexStr | bytes,
        tick_data: dict[int, LiquidityAtTick],
        tick_bitmap: dict[int, BitmapAtWord],
    ) -> None:
        """Update the liquidity mapping for the pool.

        Raises:
            UnknownPoolId: If the pool is not found in the snapshot.

        """
        pool_key: ManagedPoolIdentifier = (
            get_checksum_address(pool_manager),
            HexBytes(pool_id).to_0x_hex(),
        )

        pool_snapshot = self._liquidity_snapshot[pool_key]
        if pool_snapshot is None:
            raise UnknownPoolId(pool_id)

        pool_snapshot["tick_bitmap"].update(tick_bitmap)
        pool_snapshot["tick_data"].update(tick_data)
