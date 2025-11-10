import asyncio
import pathlib
from collections import defaultdict, deque
from typing import TYPE_CHECKING, Any, Protocol, TypedDict

import pydantic_core
import tqdm
import tqdm.asyncio
from eth_abi.abi import decode as abi_decode
from eth_typing import ChecksumAddress, HexAddress
from hexbytes import HexBytes
from sqlalchemy import select
from web3 import Web3
from web3.types import LogReceipt

from degenbot.checksum_cache import get_checksum_address
from degenbot.config import settings
from degenbot.connection import async_connection_manager, connection_manager
from degenbot.database import db_session
from degenbot.database.models.base import ExchangeTable
from degenbot.database.models.pools import AbstractUniswapV3Pool, LiquidityPoolTable
from degenbot.database.operations import get_scoped_sqlite_session
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

if TYPE_CHECKING:
    from collections.abc import Sequence


class LiquidityMap(TypedDict):
    tick_bitmap: dict[int, UniswapV3BitmapAtWord]
    tick_data: dict[int, UniswapV3LiquidityAtTick]


class UniswapV3LiquiditySnapshotSource(Protocol):
    """
    A minimal protocol allowing the UniswapV3LiquiditySnapshot class to retrieve pool data from a
    generic source.
    """

    storage_kind: str
    chain_id: int

    # Any class implementing the protocol must implement these methods, transforming data as
    # necessary to return the specified types.
    def get_liquidity_map(self, pool_address: ChecksumAddress) -> LiquidityMap | None: ...
    def get_newest_block(self) -> BlockNumber | None: ...
    def get_pools(self) -> set[ChecksumAddress]: ...


class MonolithicJsonFileSnapshot:
    """
    A pool liquidity source backed by a single JSON file with this structure:
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
    }
    """

    storage_kind = "file"

    def __init__(self, path: pathlib.Path | str) -> None:
        path = pathlib.Path(path).expanduser().absolute()
        self._path = path
        self._file_snapshot: dict[str, Any] = pydantic_core.from_json(path.read_bytes())
        self.chain_id: int = self._file_snapshot["chain_id"]

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

    def get_newest_block(self) -> BlockNumber | None:
        newest_block = self._file_snapshot.get("snapshot_block")
        if newest_block is None:
            return None
        return int(newest_block)

    def get_pools(self) -> set[ChecksumAddress]:
        # all top-level keys except metadata entries
        return {
            get_checksum_address(key)
            for key in self._file_snapshot
            if key not in ("chain_id", "snapshot_block")
        }


class IndividualJsonFileSnapshot:
    """
    Snapshot source backed by a directory of JSON files with this tree structure:

        /path/to/snapshots/
        ├── _metadata.json              -> { "block": int, "chain_id": int }
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
        self._metadata: dict[str, Any] = pydantic_core.from_json(metadata_path.read_bytes())
        self.chain_id: int = self._metadata["chain_id"]

    def get_newest_block(self) -> BlockNumber | None:
        newest_block = self._metadata.get("block")
        if newest_block is None:
            return None
        return int(newest_block)

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

    def __init__(self, chain_id: ChainId, database_path: pathlib.Path | None = None) -> None:
        if database_path is None:
            self.session = db_session
            self.database_path = settings.database.path
        else:
            self.session = get_scoped_sqlite_session(database_path)()
            self.database_path = database_path

        self.chain_id = chain_id

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

    def get_newest_block(self) -> BlockNumber | None:
        last_update_blocks: Sequence[int | None] = db_session.scalars(
            select(ExchangeTable.last_update_block).where(
                ExchangeTable.chain_id == self.chain_id,
                ExchangeTable.name.like("%!_v3", escape="!"),
            )
        ).all()

        if not last_update_blocks or None in last_update_blocks:
            return None

        return max(
            last_update_block
            for last_update_block in last_update_blocks
            if isinstance(last_update_block, int)
        )

    def get_pools(self) -> set[ChecksumAddress]:
        return {
            get_checksum_address(pool)
            for pool in self.session.scalars(select(AbstractUniswapV3Pool.address)).all()
        }


class UniswapV3LiquiditySnapshot:
    """
    Retrieve and maintain liquidity positions for Uniswap V3 pools.
    """

    UNISWAP_V3_MINT_EVENT_HASH = HexBytes(
        Web3().eth.contract(abi=UNISWAP_V3_POOL_ABI).events.Mint().topic
    )
    UNISWAP_V3_BURN_EVENT_HASH = HexBytes(
        Web3().eth.contract(abi=UNISWAP_V3_POOL_ABI).events.Burn().topic
    )

    def __init__(self, source: UniswapV3LiquiditySnapshotSource) -> None:
        self._source = source
        self._chain_id = source.chain_id

        if (source_block := source.get_newest_block()) is None:
            msg = "The provided source is uninitialized."
            raise ValueError(msg)
        self.newest_block: BlockNumber = source_block

        self._liquidity_events: dict[ChecksumAddress, deque[UniswapV3LiquidityEvent]] = defaultdict(
            deque
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

    def _process_liquidity_event_log(
        self,
        log: LogReceipt,
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

        assert not log["removed"]

        (tick_lower,) = abi_decode(["int24"], log["topics"][2])
        (tick_upper,) = abi_decode(["int24"], log["topics"][3])

        if log["topics"][0] == self.UNISWAP_V3_BURN_EVENT_HASH:
            # Decode Burn event
            amount, _, _ = abi_decode(
                ["uint128", "uint256", "uint256"],
                log["data"],
            )
            amount = -amount
        else:
            # Decode Mint event
            _, amount, _, _ = abi_decode(
                ["address", "uint128", "uint256", "uint256"],
                log["data"],
            )

        return log["address"], UniswapV3LiquidityEvent(
            block_number=log["blockNumber"],
            liquidity=amount,
            tick_lower=tick_lower,
            tick_upper=tick_upper,
            tx_index=log["transactionIndex"],
            log_index=log["logIndex"],
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

        logger.info(f"Updating Uniswap V3 snapshot from block {self.newest_block} to {to_block}")

        event_logs = fetch_logs_retrying(
            w3=connection_manager.get_web3(self.chain_id),
            start_block=self.newest_block + 1,
            end_block=to_block,
            max_blocks_per_request=blocks_per_request,
            topic_signature=[
                [
                    self.UNISWAP_V3_MINT_EVENT_HASH,
                    self.UNISWAP_V3_BURN_EVENT_HASH,
                ],  # match topic0: Mint OR Burn
            ],
        )

        for event_log in tqdm.tqdm(
            event_logs,
            desc="Processing liquidity events",
            unit="event",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            pool_address, liquidity_event = self._process_liquidity_event_log(event_log)
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

        logger.info(f"Updating Uniswap V3 snapshot from block {self.newest_block} to {to_block}")

        event_logs = await fetch_logs_retrying_async(
            w3=async_connection_manager.get_web3(self.chain_id),
            start_block=self.newest_block + 1,
            end_block=to_block,
            max_blocks_per_request=blocks_per_request,
            topic_signature=[
                [
                    self.UNISWAP_V3_MINT_EVENT_HASH,
                    self.UNISWAP_V3_BURN_EVENT_HASH,
                ]
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
            pool_address, liquidity_event = self._process_liquidity_event_log(event_log)
            self._liquidity_events[pool_address].append(liquidity_event)

        self.newest_block = to_block

    def pending_updates(
        self,
        pool_address: HexAddress,
    ) -> tuple[UniswapV3PoolLiquidityMappingUpdate, ...]:
        """
        Consume pending liquidity updates for the pool.
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
