# TODO: support unwinding updates for re-org


import pathlib
from io import TextIOWrapper
from typing import Any, TextIO

import ujson
from eth_typing import ChecksumAddress
from eth_utils.abi import event_abi_to_log_topic
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes
from web3 import Web3
from web3.contract.base_contract import BaseContractEvent
from web3.types import EventData, FilterParams, LogReceipt
from web3.utils import get_event_abi

from degenbot.config import connection_manager
from degenbot.exceptions import DegenbotValueError
from degenbot.logging import logger
from degenbot.uniswap.abi import UNISWAP_V3_POOL_ABI
from degenbot.uniswap.types import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3LiquidityEvent,
    UniswapV3PoolExternalUpdate,
)


class UniswapV3LiquiditySnapshot:
    """
    Retrieve and maintain liquidity positions for Uniswap V3 pools.
    """

    def __init__(
        self,
        file: TextIO | str,
        chain_id: int | None = None,
    ):
        file_handle: TextIOWrapper
        json_liquidity_snapshot: dict[str, Any]

        match file:
            case TextIOWrapper():
                json_liquidity_snapshot = ujson.load(file)
            case str():
                with pathlib.Path(file).open() as file_handle:
                    json_liquidity_snapshot = ujson.load(file_handle)
            case _:  # pragma: no cover
                raise DegenbotValueError(message=f"Unrecognized file type {type(file)}")

        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id

        self.newest_block = json_liquidity_snapshot.pop("snapshot_block")

        self._liquidity_snapshot: dict[ChecksumAddress, dict[str, Any]] = {
            to_checksum_address(pool_address): {
                "tick_bitmap": {
                    int(k): UniswapV3BitmapAtWord(**v)
                    for k, v in pool_liquidity_snapshot["tick_bitmap"].items()
                },
                "tick_data": {
                    int(k): UniswapV3LiquidityAtTick(**v)
                    for k, v in pool_liquidity_snapshot["tick_data"].items()
                },
            }
            for pool_address, pool_liquidity_snapshot in json_liquidity_snapshot.items()
        }

        logger.info(
            f"Loaded LP snapshot: {len(json_liquidity_snapshot)} pools @ block {self.newest_block}"
        )

        self._liquidity_events: dict[ChecksumAddress, list[UniswapV3LiquidityEvent]] = {}

    @property
    def chain_id(self) -> int:
        return self._chain_id

    def _add_pool_if_missing(self, pool_address: ChecksumAddress) -> None:
        try:
            self._liquidity_events[pool_address]
        except KeyError:
            self._liquidity_events[pool_address] = []

        try:
            self._liquidity_snapshot[pool_address]
        except KeyError:
            self._liquidity_snapshot[pool_address] = {}

    def fetch_new_liquidity_events(
        self,
        to_block: int,
        span: int = 100,
    ) -> None:
        def process_liquidity_event_log(
            event: BaseContractEvent, log: LogReceipt
        ) -> tuple[ChecksumAddress, UniswapV3LiquidityEvent]:
            decoded_event: EventData = event.process_log(log)
            address = to_checksum_address(decoded_event["address"])
            tx_index = decoded_event["transactionIndex"]
            liquidity_block = decoded_event["blockNumber"]
            liquidity = decoded_event["args"]["amount"] * (
                -1 if decoded_event["event"] == "Burn" else 1
            )
            tick_lower = decoded_event["args"]["tickLower"]
            tick_upper = decoded_event["args"]["tickUpper"]

            return address, UniswapV3LiquidityEvent(
                block_number=liquidity_block,
                liquidity=liquidity,
                tick_lower=tick_lower,
                tick_upper=tick_upper,
                tx_index=tx_index,
            )

        logger.info(f"Updating snapshot from block {self.newest_block} to {to_block}")
        w3 = connection_manager.get_web3(self.chain_id)

        v3pool = Web3().eth.contract(abi=UNISWAP_V3_POOL_ABI)
        for event in [v3pool.events.Mint, v3pool.events.Burn]:
            event_instance = event()
            logger.info(f"Processing {event.event_name} events")
            event_abi = get_event_abi(abi=v3pool.abi, event_name=event.event_name)
            start_block = self.newest_block + 1

            while True:
                end_block = min(to_block, start_block + span - 1)

                event_filter_params = FilterParams(
                    fromBlock=start_block,
                    toBlock=end_block,
                    topics=[HexBytes(event_abi_to_log_topic(event_abi))],
                )

                for event_log in w3.eth.get_logs(event_filter_params):
                    pool_address, liquidity_event = process_liquidity_event_log(
                        event_instance, event_log
                    )

                    if liquidity_event.liquidity == 0:  # pragma: no cover
                        continue

                    self._add_pool_if_missing(pool_address)
                    self._liquidity_events[pool_address].append(liquidity_event)

                if end_block == to_block:
                    break
                start_block = end_block + 1

        logger.info(f"Updated snapshot to block {to_block}")
        self.newest_block = to_block

    def get_new_liquidity_updates(self, pool_address: str) -> list[UniswapV3PoolExternalUpdate]:
        pool_address = to_checksum_address(pool_address)
        pool_updates = self._liquidity_events.get(pool_address, [])
        self._liquidity_events[pool_address] = []

        # Liquidity events from a block prior to the current update block will be rejected, so they
        # must be applied in chronological order
        sorted_events = sorted(
            pool_updates,
            key=lambda event: (event.block_number, event.tx_index),
        )

        return [
            UniswapV3PoolExternalUpdate(
                block_number=event.block_number,
                liquidity_change=(
                    event.liquidity,
                    event.tick_lower,
                    event.tick_upper,
                ),
            )
            for event in sorted_events
        ]

    def get_tick_bitmap(self, pool: ChecksumAddress | str) -> dict[int, UniswapV3BitmapAtWord]:
        pool_address = to_checksum_address(pool)

        try:
            tick_bitmap: dict[int, UniswapV3BitmapAtWord] = self._liquidity_snapshot[pool_address][
                "tick_bitmap"
            ]
        except KeyError:
            return {}
        else:
            return tick_bitmap

    def get_tick_data(self, pool: ChecksumAddress | str) -> dict[int, UniswapV3LiquidityAtTick]:
        pool_address = to_checksum_address(pool)

        try:
            tick_data: dict[int, UniswapV3LiquidityAtTick] = self._liquidity_snapshot[pool_address][
                "tick_data"
            ]
        except KeyError:
            return {}
        else:
            return tick_data

    def update_snapshot(
        self,
        pool: ChecksumAddress | str,
        tick_data: dict[int, UniswapV3LiquidityAtTick],
        tick_bitmap: dict[int, UniswapV3BitmapAtWord],
    ) -> None:
        pool_address = to_checksum_address(pool)

        self._add_pool_if_missing(pool_address)
        self._liquidity_snapshot[pool_address].update(
            {
                "tick_bitmap": tick_bitmap,
            }
        )
        self._liquidity_snapshot[pool_address].update(
            {
                "tick_data": tick_data,
            }
        )
