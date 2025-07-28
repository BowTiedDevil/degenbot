import pathlib
from collections import defaultdict
from typing import Any, TypedDict, cast

import pydantic_core
import tqdm
from eth_typing import ABIEvent, ChecksumAddress, HexAddress, HexStr
from eth_utils.abi import event_abi_to_log_topic
from hexbytes import HexBytes
from web3 import Web3
from web3.contract.base_contract import BaseContractEvent
from web3.types import EventData, LogReceipt
from web3.utils import get_abi_element

from degenbot import connection_manager, get_checksum_address
from degenbot.functions import fetch_logs_retrying
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


class UniswapV4LiquiditySnapshot:
    """
    Retrieve and maintain liquidity positions for Uniswap V4 pools.
    """

    def __init__(
        self,
        path: pathlib.Path | str,
        chain_id: ChainId | None = None,
    ) -> None:
        def _get_pool_map(pool_identifier: ManagedPoolIdentifier) -> LiquidityMap:
            """
            Get the liquidity map for the pool, identified by an ordered tuple of the pool manager
            address and the pool ID.
            """

            _pool_manager, _pool_id = pool_identifier

            if self._file_snapshot is not None and _pool_id in self._file_snapshot:
                return LiquidityMap(
                    tick_bitmap={
                        int(k): UniswapV4BitmapAtWord(**v)
                        for k, v in self._file_snapshot[_pool_id]["tick_bitmap"].items()
                    },
                    tick_data={
                        int(k): UniswapV4LiquidityAtTick(**v)
                        for k, v in self._file_snapshot[_pool_id]["tick_data"].items()
                    },
                )
            if self._dir_path is not None and (
                (pool_map_file := self._dir_path / f"{_pool_id}.json").exists()
            ):
                pool_liquidity_snapshot = pydantic_core.from_json(pool_map_file.read_bytes())
                return LiquidityMap(
                    tick_bitmap={
                        int(k): UniswapV4BitmapAtWord(**v)
                        for k, v in pool_liquidity_snapshot["tick_bitmap"].items()
                    },
                    tick_data={
                        int(k): UniswapV4LiquidityAtTick(**v)
                        for k, v in pool_liquidity_snapshot["tick_data"].items()
                    },
                )

            return LiquidityMap(
                tick_bitmap={},
                tick_data={},
            )

        path = pathlib.Path(path)

        self.newest_block: BlockNumber
        self._dir_path: pathlib.Path | None = None
        self._file_snapshot: dict[str, Any] | None = None

        if path.is_file():
            self._file_snapshot = pydantic_core.from_json(path.read_bytes())
            self.pool_manager = self._file_snapshot.pop("pool_manager")
            self.newest_block = self._file_snapshot.pop("snapshot_block")
        if path.is_dir():
            metadata_path = path / "_metadata.json"
            metadata = pydantic_core.from_json(metadata_path.read_bytes())
            self.pool_manager = metadata["pool_manager"]
            self.newest_block = metadata["snapshot_block"]
            self._dir_path = path.absolute()

        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id

        logger.info(
            f"Loaded Uniswap V4 LP snapshot: {len(self._file_snapshot) if self._file_snapshot else len(tuple(path.glob('0x*.json')))} pools @ block {self.newest_block}"  # noqa:E501
        )

        self._liquidity_events: dict[ManagedPoolIdentifier, list[UniswapV4LiquidityEvent]] = (
            defaultdict(list)
        )
        self._liquidity_snapshot: dict[ManagedPoolIdentifier, LiquidityMap] = KeyedDefaultDict(
            _get_pool_map
        )

    @property
    def chain_id(self) -> int:
        return self._chain_id

    @property
    def pools(self) -> set[ManagedPoolIdentifier]:
        return set(self._liquidity_snapshot.keys())

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
        ) -> tuple[ChecksumAddress, str, UniswapV4LiquidityEvent]:
            """
            Decode an event log and convert to an address, pool ID, and a `UniswapV4LiquidityEvent`
            for processing with `UniswapV4Pool.update_liquidity_map`.
            """

            # ref: https://github.com/Uniswap/v4-core/blob/main/src/interfaces/IPoolManager.sol
            # event ModifyLiquidity(
            #     id,
            #     msg.sender,
            #     params.tickLower,
            #     params.tickUpper,
            #     params.liquidityDelta,
            #     params.salt
            # );

            decoded_event: EventData = event.process_log(log)
            pool_manager_address = get_checksum_address(decoded_event["address"])
            pool_id = HexBytes(decoded_event["args"]["id"]).to_0x_hex()
            tx_index = decoded_event["transactionIndex"]
            log_index = decoded_event["logIndex"]
            liquidity_block = decoded_event["blockNumber"]
            liquidity = decoded_event["args"]["liquidityDelta"]
            tick_lower = decoded_event["args"]["tickLower"]
            tick_upper = decoded_event["args"]["tickUpper"]

            return (
                pool_manager_address,
                pool_id,
                UniswapV4LiquidityEvent(
                    block_number=liquidity_block,
                    tx_index=tx_index,
                    log_index=log_index,
                    liquidity=liquidity,
                    tick_lower=tick_lower,
                    tick_upper=tick_upper,
                ),
            )

        logger.info(f"Updating Uniswap V4 snapshot from block {self.newest_block} to {to_block}")

        v4_pool_manager = Web3().eth.contract(abi=UNISWAP_V4_POOL_MANAGER_ABI)
        event = v4_pool_manager.events.ModifyLiquidity
        event_instance = event()
        event_abi = cast(
            "ABIEvent",
            get_abi_element(
                abi=v4_pool_manager.abi,
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
            pool_manager_address, pool_id, liquidity_event = _process_liquidity_event_log(
                event_instance, event_log
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
    ) -> dict[int, UniswapV4BitmapAtWord]:
        """
        Consume the tick initialization bitmaps for the pool.
        """

        pool_key: ManagedPoolIdentifier = (
            get_checksum_address(pool_manager),
            HexBytes(pool_id).to_0x_hex(),
        )
        tick_bitmap = self._liquidity_snapshot[pool_key]["tick_bitmap"]
        self._liquidity_snapshot[pool_key]["tick_bitmap"] = {}

        return tick_bitmap

    def tick_data(
        self,
        pool_manager: HexAddress | bytes,
        pool_id: HexStr | bytes,
    ) -> dict[int, UniswapV4LiquidityAtTick]:
        """
        Consume the liquidity mapping for the pool.
        """

        pool_key: ManagedPoolIdentifier = (
            get_checksum_address(pool_manager),
            HexBytes(pool_id).to_0x_hex(),
        )
        tick_data = self._liquidity_snapshot[pool_key]["tick_data"]
        self._liquidity_snapshot[pool_key]["tick_data"] = {}

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
        self._liquidity_snapshot[pool_key]["tick_bitmap"].update(tick_bitmap)
        self._liquidity_snapshot[pool_key]["tick_data"].update(tick_data)
