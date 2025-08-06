import pathlib
from collections import defaultdict
from typing import TYPE_CHECKING, Any, Literal, Protocol, TypedDict, cast

import pydantic_core
import tqdm
import tqdm.asyncio
from eth_typing import ABIEvent, ChecksumAddress, HexAddress
from eth_utils.abi import event_abi_to_log_topic
from hexbytes import HexBytes
from sqlalchemy import select
from web3 import Web3
from web3.contract.base_contract import BaseContractEvent
from web3.types import EventData, LogReceipt
from web3.utils import get_abi_element

from degenbot.checksum_cache import get_checksum_address
from degenbot.config import settings
from degenbot.connection import async_connection_manager, connection_manager
from degenbot.database.models.base import MetadataTable
from degenbot.database.models.pools import AbstractUniswapV3Pool, LiquidityPoolTable
from degenbot.database.operations import default_session, get_scoped_sqlite_session
from degenbot.exceptions.liquidity_pool import UnknownPool
from degenbot.functions import fetch_logs_retrying, fetch_logs_retrying_async
from degenbot.logging import logger
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import KeyedDefaultDict
from degenbot.uniswap.abi import UNISWAP_V3_POOL_ABI
from degenbot.uniswap.v3_types import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3LiquidityEvent,
    UniswapV3PoolLiquidityMappingUpdate,
)


class LiquidityMap(TypedDict):
    tick_bitmap: dict[int, UniswapV3BitmapAtWord]
    tick_data: dict[int, UniswapV3LiquidityAtTick]


class SnapshotSource(Protocol):
    """
    A minimal protocol allowing the UniswapV3LiquiditySnapshot class to retrieve pool data from a
    generic source.
    """

    storage_kind: Literal["dir", "file", "db"]

    # Any class implementing the protocol must implement these methods, transforming data as
    # necessary to return the specified types.
    def get_liquidity_map(self, pool_address: ChecksumAddress) -> LiquidityMap | None: ...
    def get_newest_block(self) -> BlockNumber: ...
    def get_pools(self) -> set[ChecksumAddress]: ...


class MonolithicJsonFileSnapshot(SnapshotSource):
    """
    A pool liquidity source backed by a single JSON file with this structure:
    {
        "snapshot_block": int,
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
    }
    """

    storage_kind = "file"

    def __init__(self, path: pathlib.Path | str) -> None:
        path = pathlib.Path(path).expanduser().absolute()
        self._path = path
        self._file_snapshot: dict[str, Any] = pydantic_core.from_json(path.read_bytes())

    def get_liquidity_map(self, pool_address: ChecksumAddress) -> LiquidityMap | None:
        if pool_address not in self._file_snapshot:
            return None

        return LiquidityMap(
            tick_bitmap={
                int(k): UniswapV3BitmapAtWord(**v)
                for k, v in self._file_snapshot[pool_address]["tick_bitmap"].items()
            },
            tick_data={
                int(k): UniswapV3LiquidityAtTick(**v)
                for k, v in self._file_snapshot[pool_address]["tick_data"].items()
            },
        )

    def get_newest_block(self) -> BlockNumber:
        return int(self._file_snapshot["snapshot_block"])

    def get_pools(self) -> set[ChecksumAddress]:
        # all top-level keys except 'snapshot_block'
        return {get_checksum_address(key) for key in self._file_snapshot if key != "snapshot_block"}


class IndividualJsonFileSnapshot(SnapshotSource):
    """
    Snapshot source backed by a directory of JSON files with this tree structure:

        /path/to/snapshots/
        ├── _metadata.json              -> { "block": int }
        ├── 0xPoolAddress1.json         -> { "tick_bitmap": {...}, "tick_data": {...} }
        ├── 0xPoolAddress2.json         -> { "tick_bitmap": {...}, "tick_data": {...} }
        └── 0xPoolAddress3.json         -> { "tick_bitmap": {...}, "tick_data": {...} }

    Each pool file contains the same structure as the monolithic snapshot's per-pool entries.
    """

    storage_kind = "dir"

    def __init__(self, path: pathlib.Path | str) -> None:
        dir_path = pathlib.Path(path).expanduser().absolute()
        assert dir_path.exists()
        assert dir_path.is_dir()
        self._dir = dir_path

        metadata_path = self._dir / "_metadata.json"
        self._metadata = pydantic_core.from_json(metadata_path.read_bytes())

    def get_newest_block(self) -> BlockNumber:
        return int(self._metadata["block"])

    def get_pools(self) -> set[ChecksumAddress]:
        return {get_checksum_address(pool_file.stem) for pool_file in self._dir.glob("0x*.json")}

    def get_liquidity_map(self, pool_address: ChecksumAddress) -> LiquidityMap | None:
        pool_path = self._dir / f"{pool_address}.json"
        if not pool_path.exists():
            return None

        pool_liquidity_snapshot = pydantic_core.from_json(pool_path.read_bytes())
        return LiquidityMap(
            tick_bitmap={
                int(k): UniswapV3BitmapAtWord(**v)
                for k, v in pool_liquidity_snapshot["tick_bitmap"].items()
            },
            tick_data={
                int(k): UniswapV3LiquidityAtTick(**v)
                for k, v in pool_liquidity_snapshot["tick_data"].items()
            },
        )


class DatabaseSnapshot:
    """
    Snapshot source backed by built-in SQLite database using the ORM abstractions defined
    in `degenbot.database`.

    If a path to an SQLite database is not provided, the default location specified in
    `degenbot.settings` will be used.
    """

    storage_kind = "db"

    def __init__(self, database_path: pathlib.Path | None = None) -> None:
        if database_path is None:
            self.session = default_session
            self.database_path = settings.database.path
        else:
            self.session = get_scoped_sqlite_session(database_path)
            self.database_path = database_path

    def get_liquidity_map(self, pool_address: ChecksumAddress) -> LiquidityMap | None:
        pool_in_db = self.session.scalar(
            select(LiquidityPoolTable).where(LiquidityPoolTable.address == pool_address)
        )
        if pool_in_db is None:
            return None

        if TYPE_CHECKING:
            assert isinstance(pool_in_db, AbstractUniswapV3Pool)

        return LiquidityMap(
            tick_bitmap={
                int(initialization_map.word): UniswapV3BitmapAtWord(
                    bitmap=initialization_map.bitmap
                )
                for initialization_map in pool_in_db.initialization_maps
            },
            tick_data={
                int(liquidity_position.tick): UniswapV3LiquidityAtTick(
                    liquidity_gross=liquidity_position.liquidity_gross,
                    liquidity_net=liquidity_position.liquidity_net,
                )
                for liquidity_position in pool_in_db.liquidity_positions
            },
        )

    def get_newest_block(self) -> BlockNumber:
        metadata = self.session.scalar(
            select(MetadataTable).where(MetadataTable.key == "liquidity_map")
        )
        assert metadata is not None  # TODO: throw real exception here
        assert metadata.value is not None  # TODO: throw real exception here
        block = pydantic_core.from_json(metadata.value)["block"]
        return int(block)

    def get_pools(self) -> set[ChecksumAddress]:
        return {
            get_checksum_address(pool)
            for pool in self.session.scalars(select(AbstractUniswapV3Pool.address)).all()
        }


class UniswapV3LiquiditySnapshot:
    """
    Retrieve and maintain liquidity positions for Uniswap V3 pools.
    """

    def __init__(
        self,
        source: SnapshotSource,
        chain_id: ChainId | None = None,
    ) -> None:
        self._source = source
        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id
        self.newest_block = source.get_newest_block()

        self._liquidity_events: dict[ChecksumAddress, list[UniswapV3LiquidityEvent]] = defaultdict(
            list
        )
        self._liquidity_snapshot: dict[ChecksumAddress, LiquidityMap | None] = KeyedDefaultDict(
            lambda key: self._source.get_liquidity_map(get_checksum_address(key))
        )

        logger.info(f"Loaded Uniswap V3 LP snapshot from {source.storage_kind} source")

    @property
    def chain_id(self) -> int:
        return self._chain_id

    @property
    def pools(self) -> set[ChecksumAddress]:
        return self._source.get_pools()

    def fetch_new_events(
        self,
        to_block: BlockNumber,
        blocks_per_request: int | None = None,
    ) -> None:
        """
        Fetch liquidity events from the block following the last-known event to the target block
        using `eth_getLogs`. Blocks per request will be capped at `blocks_per_request`.
        """

        def _process_liquidity_event_log(
            event: BaseContractEvent, log: LogReceipt
        ) -> tuple[ChecksumAddress, UniswapV3LiquidityEvent]:
            """
            Decode an event log and convert to an address and a `UniswapV3LiquidityEvent` for
            processing with `UniswapV3Pool.update_liquidity_map`.
            """

            # ref: https://github.com/Uniswap/v3-core/blob/main/contracts/interfaces/pool/IUniswapV3PoolEvents.sol
            #
            # event Mint(
            #     address sender,
            #     address indexed owner,
            #     int24 indexed tickLower,
            #     int24 indexed tickUpper,
            #     uint128 amount,
            #     uint256 amount0,
            #     uint256 amount1
            # );
            # event Burn(
            #     address indexed owner,
            #     int24 indexed tickLower,
            #     int24 indexed tickUpper,
            #     uint128 amount,
            #     uint256 amount0,
            #     uint256 amount1
            # );

            decoded_event: EventData = event.process_log(log)
            pool_address = get_checksum_address(decoded_event["address"])
            tx_index = decoded_event["transactionIndex"]
            log_index = decoded_event["logIndex"]
            liquidity_block = decoded_event["blockNumber"]
            liquidity = decoded_event["args"]["amount"]
            if decoded_event["event"] == "Burn":
                liquidity = -liquidity  # liquidity removal
            tick_lower = decoded_event["args"]["tickLower"]
            tick_upper = decoded_event["args"]["tickUpper"]

            return (
                pool_address,
                UniswapV3LiquidityEvent(
                    block_number=liquidity_block,
                    tx_index=tx_index,
                    log_index=log_index,
                    liquidity=liquidity,
                    tick_lower=tick_lower,
                    tick_upper=tick_upper,
                ),
            )

        logger.info(f"Updating Uniswap V3 snapshot from block {self.newest_block} to {to_block}")

        v3pool = Web3().eth.contract(abi=UNISWAP_V3_POOL_ABI)
        for event in (
            v3pool.events.Mint,
            v3pool.events.Burn,
        ):
            event_instance = event()
            event_abi = cast(
                "ABIEvent",
                get_abi_element(
                    abi=v3pool.abi,
                    abi_element_identifier=event.event_name,
                ),
            )

            event_logs = fetch_logs_retrying(
                w3=connection_manager.get_web3(self.chain_id),
                start_block=self.newest_block + 1,
                end_block=to_block,
                max_blocks_per_request=blocks_per_request,
                topic_signature=[HexBytes(event_abi_to_log_topic(event_abi))],
            )

            for event_log in tqdm.tqdm(
                event_logs,
                desc=f"Processing {event.event_name} events",
                unit="event",
                bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            ):
                pool_address, liquidity_event = _process_liquidity_event_log(
                    event_instance, event_log
                )
                self._liquidity_events[pool_address].append(liquidity_event)

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

        def _process_liquidity_event_log(
            event: BaseContractEvent, log: LogReceipt
        ) -> tuple[ChecksumAddress, UniswapV3LiquidityEvent]:
            """
            Decode an event log and convert to an address and a `UniswapV3LiquidityEvent` for
            processing with `UniswapV3Pool.update_liquidity_map`.
            """
            decoded_event: EventData = event.process_log(log)
            pool_address = get_checksum_address(decoded_event["address"])
            tx_index = decoded_event["transactionIndex"]
            log_index = decoded_event["logIndex"]
            liquidity_block = decoded_event["blockNumber"]
            liquidity = decoded_event["args"]["amount"]
            if decoded_event["event"] == "Burn":
                liquidity = -liquidity  # liquidity removal
            tick_lower = decoded_event["args"]["tickLower"]
            tick_upper = decoded_event["args"]["tickUpper"]

            return (
                pool_address,
                UniswapV3LiquidityEvent(
                    block_number=liquidity_block,
                    tx_index=tx_index,
                    log_index=log_index,
                    liquidity=liquidity,
                    tick_lower=tick_lower,
                    tick_upper=tick_upper,
                ),
            )

        logger.info(
            f"(async) Updating Uniswap V3 snapshot from block {self.newest_block} to {to_block}"
        )

        v3pool = Web3().eth.contract(abi=UNISWAP_V3_POOL_ABI)
        for event in (
            v3pool.events.Mint,
            v3pool.events.Burn,
        ):
            event_instance = event()
            event_abi = cast(
                "ABIEvent",
                get_abi_element(
                    abi=v3pool.abi,
                    abi_element_identifier=event.event_name,
                ),
            )

            event_logs = await fetch_logs_retrying_async(
                w3=async_connection_manager.get_web3(self.chain_id),
                start_block=self.newest_block + 1,
                end_block=to_block,
                max_blocks_per_request=blocks_per_request,
                topic_signature=[HexBytes(event_abi_to_log_topic(event_abi))],
            )

            async for event_log in tqdm.asyncio.tqdm(
                event_logs,
                desc=f"Processing {event.event_name} events",
                unit="event",
                bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            ):
                pool_address, liquidity_event = _process_liquidity_event_log(
                    event_instance, event_log
                )
                self._liquidity_events[pool_address].append(liquidity_event)

        self.newest_block = to_block

    def pending_updates(
        self,
        pool_address: HexAddress,
    ) -> tuple[UniswapV3PoolLiquidityMappingUpdate, ...]:
        """
        Consume pending liquidity updates for the pool, sorted chronologically.
        """

        pool_key = get_checksum_address(pool_address)
        pending_events = tuple(self._liquidity_events[pool_key])
        self._liquidity_events[pool_key] = []

        # Mint/Burn events must be applied in chronological order to ensure effective liquidity
        # invariant checks. Sort events by block, then by log order within the block.
        sorted_events = sorted(
            pending_events,
            key=lambda event: (event.block_number, event.tx_index, event.log_index),
        )

        return tuple(
            UniswapV3PoolLiquidityMappingUpdate(
                block_number=event.block_number,
                liquidity=event.liquidity,
                tick_lower=event.tick_lower,
                tick_upper=event.tick_upper,
            )
            for event in sorted_events
        )

    def tick_bitmap(self, pool_address: str | bytes) -> dict[int, UniswapV3BitmapAtWord] | None:
        """
        Consume the tick initialization bitmaps for the pool.
        """
        pool_address = get_checksum_address(pool_address)

        pool_snapshot = self._liquidity_snapshot[pool_address]
        if pool_snapshot is None:
            return None

        tick_bitmap = pool_snapshot["tick_bitmap"].copy()
        pool_snapshot["tick_bitmap"] = {}
        return tick_bitmap

    def tick_data(self, pool_address: str | bytes) -> dict[int, UniswapV3LiquidityAtTick] | None:
        """
        Consume the liquidity mapping for the pool.
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
        tick_data: dict[int, UniswapV3LiquidityAtTick],
        tick_bitmap: dict[int, UniswapV3BitmapAtWord],
    ) -> None:
        """
        Update the liquidity mapping for the pool.
        """

        pool_key = get_checksum_address(pool)

        pool_snapshot = self._liquidity_snapshot[pool_key]
        if pool_snapshot is None:
            raise UnknownPool(pool_key)

        pool_snapshot["tick_bitmap"].update(tick_bitmap)
        pool_snapshot["tick_data"].update(tick_data)
