# TODO: support unwinding updates for re-org

import pathlib
from typing import Any, cast

import pydantic_core
from eth_typing import ABIEvent, ChecksumAddress
from eth_utils.abi import event_abi_to_log_topic
from hexbytes import HexBytes
from web3 import Web3
from web3.contract.base_contract import BaseContractEvent
from web3.types import EventData, FilterParams, LogReceipt
from web3.utils import get_abi_element

from degenbot.cache import get_checksum_address
from degenbot.config import connection_manager
from degenbot.logging import logger
from degenbot.uniswap.abi import UNISWAP_V4_POOL_MANAGER_ABI
from degenbot.uniswap.types import (
    UniswapV4BitmapAtWord,
    UniswapV4LiquidityAtTick,
    UniswapV4LiquidityEvent,
    UniswapV4PoolLiquidityMappingUpdate,
)


class UniswapV4LiquiditySnapshot:
    """
    Retrieve and maintain liquidity positions for Uniswap V4 pools.
    """

    def __init__(
        self,
        file: pathlib.Path | str,
        chain_id: int | None = None,
    ):
        if isinstance(file, str):
            file = pathlib.Path(file)
        json_liquidity_snapshot: dict[str, Any] = pydantic_core.from_json(file.read_bytes())

        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id
        self.newest_block = json_liquidity_snapshot.pop("snapshot_block")

        self._liquidity_snapshot: dict[ChecksumAddress, dict[str, Any]] = {
            pool_id: {
                "tick_bitmap": {
                    int(k): UniswapV4BitmapAtWord(**v)
                    for k, v in pool_liquidity_snapshot["tick_bitmap"].items()
                },
                "tick_data": {
                    int(k): UniswapV4LiquidityAtTick(**v)
                    for k, v in pool_liquidity_snapshot["tick_data"].items()
                },
            }
            for pool_id, pool_liquidity_snapshot in json_liquidity_snapshot.items()
        }

        logger.info(
            f"Loaded LP snapshot: {len(json_liquidity_snapshot)} pools @ block {self.newest_block}"
        )

        self._liquidity_events: dict[ChecksumAddress, list[UniswapV4LiquidityEvent]] = {}

    @property
    def chain_id(self) -> int:
        return self._chain_id

    def _add_pool_if_missing(
        self, pool_manager_address: ChecksumAddress, pool_id: HexBytes
    ) -> None:
        """
        Create empty containers for the pool manager and pool ID, if missing.
        """

        try:
            self._liquidity_events[pool_manager_address]
        except KeyError:
            self._liquidity_events[pool_manager_address] = {}

        try:
            self._liquidity_events[pool_manager_address][pool_id]
        except KeyError:
            self._liquidity_events[pool_manager_address][pool_id] = []

        try:
            self._liquidity_snapshot[pool_manager_address]
        except KeyError:
            self._liquidity_snapshot[pool_manager_address] = {}

        try:
            self._liquidity_snapshot[pool_manager_address][pool_id]
        except KeyError:
            self._liquidity_snapshot[pool_manager_address][pool_id] = {}

    def fetch_new_liquidity_events(
        self,
        to_block: int,
        span: int = 100,
    ) -> None:
        def process_liquidity_event_log(
            event: BaseContractEvent, log: LogReceipt
        ) -> tuple[ChecksumAddress, str, UniswapV4LiquidityEvent]:
            decoded_event: EventData = event.process_log(log)
            pool_manager_address = get_checksum_address(decoded_event["address"])
            pool_id = HexBytes(decoded_event["args"]["id"]).to_0x_hex()
            tx_index = decoded_event["transactionIndex"]
            liquidity_block = decoded_event["blockNumber"]
            liquidity = decoded_event["args"]["liquidityDelta"]
            tick_lower = decoded_event["args"]["tickLower"]
            tick_upper = decoded_event["args"]["tickUpper"]

            return (
                pool_manager_address,
                pool_id,
                UniswapV4LiquidityEvent(
                    block_number=liquidity_block,
                    liquidity=liquidity,
                    tick_lower=tick_lower,
                    tick_upper=tick_upper,
                    tx_index=tx_index,
                ),
            )

        logger.info(f"Updating snapshot from block {self.newest_block} to {to_block}")
        w3 = connection_manager.get_web3(self.chain_id)

        # event ModifyLiquidity(id, msg.sender, params.tickLower, params.tickUpper, params.liquidityDelta, params.salt);

        v4_pool_manager = Web3().eth.contract(abi=UNISWAP_V4_POOL_MANAGER_ABI)
        for event in [v4_pool_manager.events.ModifyLiquidity]:
            event_instance = event()
            logger.info(f"Processing {event.event_name} events")
            event_abi = cast(
                ABIEvent,
                get_abi_element(abi=v4_pool_manager.abi, abi_element_identifier=event.event_name),
            )
            start_block = self.newest_block + 1

            while True:
                end_block = min(to_block, start_block + span - 1)

                event_filter_params = FilterParams(
                    fromBlock=start_block,
                    toBlock=end_block,
                    topics=[HexBytes(event_abi_to_log_topic(event_abi))],
                )

                for event_log in w3.eth.get_logs(event_filter_params):
                    pool_manager_address, pool_id, liquidity_event = process_liquidity_event_log(
                        event_instance, event_log
                    )

                    if liquidity_event.liquidity == 0:  # pragma: no cover
                        continue

                    self._add_pool_if_missing(pool_manager_address, pool_id)
                    self._liquidity_events[pool_manager_address][pool_id].append(liquidity_event)

                if end_block == to_block:
                    break
                start_block = end_block + 1

        logger.info(f"Updated snapshot to block {to_block}")
        self.newest_block = to_block

    def get_new_liquidity_updates(
        pool_manager_address = get_checksum_address(pool_manager_address)
            UniswapV4PoolLiquidityMappingUpdate(
                block_number=event.block_number,
                liquidity=event.liquidity,
                tick_lower=event.tick_lower,
                tick_upper=event.tick_upper,
            )
            for event in sorted_events
        ]

    def get_tick_bitmap(
        self,
        pool_manager_address: ChecksumAddress | str,
        pool_id: str,
    ) -> dict[int, UniswapV4BitmapAtWord]:
        pool_manager_address = to_checksum_address(pool_manager_address)

        try:
            tick_bitmap: dict[int, UniswapV4BitmapAtWord] = self._liquidity_snapshot[
                pool_manager_address
            ][pool_id]["tick_bitmap"]
        except KeyError:
            return {}
        else:
            return tick_bitmap

    def get_tick_data(
        self,
        pool_manager_address: ChecksumAddress | str,
        pool_id: str,
    ) -> dict[int, UniswapV4LiquidityAtTick]:
        pool_manager_address = to_checksum_address(pool_manager_address)

        try:
            tick_data: dict[int, UniswapV4LiquidityAtTick] = self._liquidity_snapshot[
                pool_manager_address
            ][pool_id]["tick_data"]
        except KeyError:
            return {}
        else:
            return tick_data

    def update_snapshot(
        self,
        pool: ChecksumAddress | str,
        tick_data: dict[int, UniswapV4LiquidityAtTick],
        tick_bitmap: dict[int, UniswapV4BitmapAtWord],
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
