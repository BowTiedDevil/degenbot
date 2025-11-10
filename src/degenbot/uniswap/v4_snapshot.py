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
from sqlalchemy import select
from web3 import Web3
from web3.types import LogReceipt

from degenbot.checksum_cache import get_checksum_address
from degenbot.config import settings
from degenbot.connection import async_connection_manager, connection_manager
from degenbot.database import db_session
from degenbot.database.models.base import ExchangeTable
from degenbot.database.models.pools import UniswapV4PoolTable
from degenbot.database.operations import get_scoped_sqlite_session
from degenbot.exceptions.liquidity_pool import UnknownPoolId
from degenbot.functions import fetch_logs_retrying, fetch_logs_retrying_async
from degenbot.logging import logger
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import KeyedDefaultDict
from degenbot.uniswap.abi import UNISWAP_V4_POOL_MANAGER_ABI
from degenbot.uniswap.v4_types import (
    UniswapV4BitmapAtWord,
    UniswapV4LiquidityAtTick,
    UniswapV4LiquidityEvent,
    UniswapV4PoolLiquidityMappingUpdate,
)

type PoolManagerAddress = ChecksumAddress
type PoolId = str
type ManagedPoolIdentifier = tuple[PoolManagerAddress, PoolId]


class LiquidityMap(TypedDict):
    tick_bitmap: dict[int, UniswapV4BitmapAtWord]
    tick_data: dict[int, UniswapV4LiquidityAtTick]


class UniswapV4LiquiditySnapshotSource(Protocol):
    """
    A minimal protocol allowing the UniswapV4LiquiditySnapshot class to retrieve pool data from a
    generic source.
    """

    storage_kind: str
    chain_id: int

    # Any class implementing the protocol must implement these methods, transforming data as
    # necessary to return the specified types.
    def get_liquidity_map(
        self, pool_manager: ChecksumAddress, pool_id: bytes | str
    ) -> LiquidityMap | None: ...
    def get_newest_block(self) -> BlockNumber | None: ...
    def get_pools(self) -> set[PoolId]: ...


class MonolithicJsonFileSnapshot:
    """
    A pool liquidity source backed by a single JSON file with this structure:
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
    }
    """

    storage_kind = "file"

    def __init__(self, path: pathlib.Path | str) -> None:
        path = pathlib.Path(path).expanduser().absolute()
        self._path = path
        self._file_snapshot: dict[PoolId, Any] = pydantic_core.from_json(path.read_bytes())
        self.chain_id: int = self._file_snapshot["chain_id"]

    def get_liquidity_map(
        self,
        pool_manager: ChecksumAddress,  # noqa: ARG002
        pool_id: bytes | str,
    ) -> LiquidityMap | None:
        pool_id = HexBytes(pool_id).to_0x_hex()

        if pool_id not in self._file_snapshot:
            return None

        return LiquidityMap(
            tick_bitmap={
                int(k): UniswapV4BitmapAtWord(**v)
                for k, v in self._file_snapshot[pool_id]["tick_bitmap"].items()
            },
            tick_data={
                int(k): UniswapV4LiquidityAtTick(**v)
                for k, v in self._file_snapshot[pool_id]["tick_data"].items()
            },
        )

    def get_newest_block(self) -> BlockNumber | None:
        newest_block = self._file_snapshot.get("snapshot_block")
        if newest_block is None:
            return None
        return int(newest_block)

    def get_pools(self) -> set[PoolId]:
        # all top-level keys except metadata entries
        return {
            get_checksum_address(key)
            for key in self._file_snapshot
            if key not in ("chain_id", "snapshot_block")
        }


class DatabaseSnapshot:
    """
    Snapshot source backed by built-in SQLite database using the ORM abstractions defined
    in `degenbot.database`.

    If a path to an SQLite database is not provided, the default location specified in
    `degenbot.settings` will be used.
    """

    storage_kind = "db"

    def __init__(self, chain_id: ChainId, database_path: pathlib.Path | None = None) -> None:
        if database_path is None:
            self.session = db_session
            self.database_path = settings.database.path
        else:
            self.session = get_scoped_sqlite_session(database_path)()
            self.database_path = database_path

        self.chain_id = chain_id

    def get_liquidity_map(
        self,
        pool_manager: ChecksumAddress,  # noqa: ARG002
        pool_id: bytes | str,
    ) -> LiquidityMap | None:
        pool_in_db = self.session.scalar(
            select(UniswapV4PoolTable).where(
                UniswapV4PoolTable.pool_hash == HexBytes(pool_id).to_0x_hex()
            )
        )
        if pool_in_db is None:
            return None

        return LiquidityMap(
            tick_bitmap={
                int(initialization_map.word): UniswapV4BitmapAtWord(
                    bitmap=initialization_map.bitmap
                )
                for initialization_map in pool_in_db.initialization_maps
            },
            tick_data={
                int(liquidity_position.tick): UniswapV4LiquidityAtTick(
                    liquidity_gross=liquidity_position.liquidity_gross,
                    liquidity_net=liquidity_position.liquidity_net,
                )
                for liquidity_position in pool_in_db.liquidity_positions
            },
        )

    def get_newest_block(self) -> BlockNumber | None:
        last_update_blocks = set(
            db_session.scalars(
                select(ExchangeTable.last_update_block).where(
                    ExchangeTable.chain_id == self.chain_id,
                    ExchangeTable.name.like("%!_v4", escape="!"),
                )
            ).all()
        )

        if not last_update_blocks or None in last_update_blocks:
            return None

        return max(
            last_update_block
            for last_update_block in last_update_blocks
            if isinstance(last_update_block, int)
        )

    def get_pools(self) -> set[PoolId]:
        return set(self.session.scalars(select(UniswapV4PoolTable.pool_hash)).all())


class UniswapV4LiquiditySnapshot:
    """
    Retrieve and maintain liquidity positions for Uniswap V4 pools.
    """

    UNISWAP_V4_MODIFYLIQUIDITY_EVENT_HASH = HexBytes(
        Web3().eth.contract(abi=UNISWAP_V4_POOL_MANAGER_ABI).events.ModifyLiquidity().topic
    )

    def __init__(self, source: UniswapV4LiquiditySnapshotSource) -> None:
        self._source = source
        self._chain_id = source.chain_id

        if (source_block := source.get_newest_block()) is None:
            msg = "The provided source is uninitialized."
            raise ValueError(msg)
        self.newest_block: BlockNumber = source_block

        self._liquidity_events: dict[
            tuple[ChecksumAddress, PoolId], list[UniswapV4LiquidityEvent]
        ] = defaultdict(list)
        self._liquidity_snapshot: dict[
            tuple[ChecksumAddress, PoolId],
            LiquidityMap | None,
        ] = KeyedDefaultDict(
            lambda key: self._source.get_liquidity_map(
                get_checksum_address(key[0]),
                HexBytes(key[1]).to_0x_hex(),
            )
        )

        logger.info(f"Loaded Uniswap V4 LP snapshot from {source.storage_kind} source")

    @property
    def chain_id(self) -> int:
        return self._chain_id

    @property
    def pools(self) -> set[ManagedPoolIdentifier]:
        return {(pool_manager, pool_id) for pool_manager, pool_id in self._liquidity_snapshot}

    def _process_liquidity_event_log(
        self,
        log: LogReceipt,
    ) -> tuple[ChecksumAddress, PoolId, UniswapV4LiquidityEvent]:
        """
        Decode an event log and convert to an address, pool ID, and a `UniswapV4LiquidityEvent`
        for processing with `UniswapV4Pool.update_liquidity_map`.
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
        blocks_per_request: int | None = None,
    ) -> None:
        """
        Fetch liquidity events from the block following the last-known event to the target block
        using `eth_getLogs`. Blocks per request will be capped at `blocks_per_request`.
        """

        logger.info(f"Updating Uniswap V4 snapshot from block {self.newest_block} to {to_block}")

        event_logs = fetch_logs_retrying(
            w3=connection_manager.get_web3(self.chain_id),
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
            pool_manager_address, pool_id, liquidity_event = self._process_liquidity_event_log(
                event_log
            )
            self._liquidity_events[(pool_manager_address, pool_id)].append(liquidity_event)

        self.newest_block = to_block

    async def fetch_new_events_async(
        self,
        to_block: BlockNumber,
        blocks_per_request: int | None = None,
    ) -> None:
        """
        Async version of fetch_new_events.

        Fetch liquidity events from the block following the last-known event to the target block
        using `eth_getLogs` via the async provider. Blocks per request will be capped at
        `blocks_per_request`.
        """

        logger.info(f"Updating Uniswap V4 snapshot from block {self.newest_block} to {to_block}")

        event_logs = await fetch_logs_retrying_async(
            w3=async_connection_manager.get_web3(self.chain_id),
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
            pool_manager_address, pool_id, liquidity_event = self._process_liquidity_event_log(
                event_log
            )
            self._liquidity_events[(pool_manager_address, pool_id)].append(liquidity_event)

        self.newest_block = to_block

    def pending_updates(
        self,
        pool_manager: HexAddress | bytes,
        pool_id: HexStr | bytes,
    ) -> tuple[UniswapV4PoolLiquidityMappingUpdate, ...]:
        """
        Consume and return all pending liquidity events for this pool.
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
    ) -> dict[int, UniswapV4BitmapAtWord] | None:
        """
        Consume the tick initialization bitmaps for the pool.
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
    ) -> dict[int, UniswapV4LiquidityAtTick] | None:
        """
        Consume the liquidity mapping for the pool.
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
        tick_data: dict[int, UniswapV4LiquidityAtTick],
        tick_bitmap: dict[int, UniswapV4BitmapAtWord],
    ) -> None:
        """
        Update the liquidity mapping for the pool.
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
