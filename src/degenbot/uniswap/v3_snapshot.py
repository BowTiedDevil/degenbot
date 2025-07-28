import pathlib
from collections import defaultdict
from typing import Any, TypedDict, cast

import pydantic_core
import tqdm
from eth_typing import ABIEvent, ChecksumAddress, HexAddress
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


class UniswapV3LiquiditySnapshot:
    """
    Retrieve and maintain liquidity positions for Uniswap V3 pools.
    """

    def __init__(
        self,
        path: pathlib.Path | str,
        chain_id: ChainId | None = None,
    ) -> None:
        def _get_pool_map(pool_address: HexAddress | bytes) -> LiquidityMap:
            """
            Get the liquidity map for the pool.
            """

            pool_address = get_checksum_address(pool_address)

            if self._file_snapshot is not None and pool_address in self._file_snapshot:
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
            if self._dir_path is not None and (
                (pool_map_file := self._dir_path / f"{pool_address}.json").exists()
            ):
                pool_liquidity_snapshot = pydantic_core.from_json(pool_map_file.read_bytes())
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

            return LiquidityMap(
                tick_bitmap={},
                tick_data={},
            )

        path = pathlib.Path(path).expanduser()
        assert path.exists()

        self.newest_block: BlockNumber
        self._dir_path: pathlib.Path | None = None
        self._file_snapshot: dict[str, Any] | None = None

        if path.is_file():
            self._file_snapshot = pydantic_core.from_json(path.read_bytes())
            self.newest_block = self._file_snapshot.pop("snapshot_block")
        if path.is_dir():
            self._dir_path = path
            metadata_path = path / "_metadata.json"
            assert metadata_path.exists()
            self.newest_block = pydantic_core.from_json(metadata_path.read_bytes())["block"]

        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id

        logger.info(
            f"Loaded Uniswap V3 LP snapshot: {len(self._file_snapshot) if self._file_snapshot else len(tuple(path.glob('0x*.json')))} pools @ block {self.newest_block}"  # noqa:E501
        )

        self._liquidity_events: dict[ChecksumAddress, list[UniswapV3LiquidityEvent]] = defaultdict(
            list
        )
        self._liquidity_snapshot: dict[ChecksumAddress, LiquidityMap] = KeyedDefaultDict(
            _get_pool_map
        )

    @property
    def chain_id(self) -> int:
        return self._chain_id

    @property
    def pools(self) -> set[ChecksumAddress]:
        if self._file_snapshot is not None:
            return {get_checksum_address(pool) for pool in self._file_snapshot} | {
                get_checksum_address(pool) for pool in self._liquidity_events
            }
        if self._dir_path is not None:
            return {
                get_checksum_address(pool_filename.stem)
                for pool_filename in self._dir_path.glob("0x*.json")
            }
        raise RuntimeError  # pragma: no cover

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

    def tick_bitmap(self, pool_address: str | bytes) -> dict[int, UniswapV3BitmapAtWord]:
        """
        Consume the tick initialization bitmaps for the pool.
        """

        pool_address = get_checksum_address(pool_address)
        tick_bitmap = self._liquidity_snapshot[pool_address]["tick_bitmap"]
        self._liquidity_snapshot[pool_address]["tick_bitmap"] = {}
        return tick_bitmap

    def tick_data(self, pool_address: str | bytes) -> dict[int, UniswapV3LiquidityAtTick]:
        """
        Consume the liquidity mapping for the pool.
        """

        pool_key = get_checksum_address(pool_address)
        tick_data = self._liquidity_snapshot[pool_key]["tick_data"]
        self._liquidity_snapshot[pool_key]["tick_data"] = {}
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
        self._liquidity_snapshot[pool_key]["tick_bitmap"].update(tick_bitmap)
        self._liquidity_snapshot[pool_key]["tick_data"].update(tick_data)
