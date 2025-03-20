# TODO: support unwinding updates for re-org

import pathlib
from typing import Any, cast

import pydantic_core
from eth_typing import ABIEvent, ChecksumAddress, HexStr
from eth_utils.abi import event_abi_to_log_topic
from hexbytes import HexBytes
from web3 import Web3
from web3.contract.base_contract import BaseContractEvent
from web3.types import EventData, FilterParams, LogReceipt
from web3.utils import get_abi_element

from degenbot.cache import get_checksum_address
from degenbot.config import connection_manager
from degenbot.logging import logger
from degenbot.uniswap.abi import UNISWAP_V3_POOL_ABI
from degenbot.uniswap.types import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3LiquidityEvent,
    UniswapV3PoolLiquidityMappingUpdate,
)


class UniswapV3LiquiditySnapshot:
    """
    Retrieve and maintain liquidity positions for Uniswap V3 pools.
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
            get_checksum_address(pool_address): {
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
        """
        Create entries for the pool if missing.
        """

        if pool_address not in self._liquidity_events:
            self._liquidity_events[pool_address] = []

        if pool_address not in self._liquidity_snapshot:
            self._liquidity_snapshot[pool_address] = {}

    def fetch_new_liquidity_events(
        self,
        to_block: int,
        span: int = 1000,
    ) -> None:
        """
        Fetch liquidity events from the newest known liquidity events to the target block using
        `eth_getLogs`. Blocks per request will be capped at `blocks_per_request`.
        """

        def process_liquidity_event_log(
            event: BaseContractEvent, log: LogReceipt
        ) -> tuple[ChecksumAddress, UniswapV3LiquidityEvent]:
            """
            Decode an event log and convert to an address and a `UniswapV3LiquidityEvent` for
            processing with `UniswapV3Pool.update_liquidity_map`.
            """

            decoded_event: EventData = event.process_log(log)
            address = get_checksum_address(decoded_event["address"])
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

        logger.info(f"Updating Uniswap V3 snapshot from block {self.newest_block} to {to_block}")
        w3 = connection_manager.get_web3(self.chain_id)

        v3pool = Web3().eth.contract(abi=UNISWAP_V3_POOL_ABI)
        for event in [v3pool.events.Mint, v3pool.events.Burn]:
            event_instance = event()
            logger.info(f"Processing {event.event_name} events")
            event_abi = cast(
                "ABIEvent", get_abi_element(abi=v3pool.abi, abi_element_identifier=event.event_name)
            )
            start_block = self.newest_block + 1

            while True:
                end_block = min(to_block, start_block + span - 1)
                logger.info(f"Fetching events for blocks {start_block}-{end_block}")

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

    def get_new_liquidity_updates(
        self,
        pool_address: HexStr,
    ) -> tuple[UniswapV3PoolLiquidityMappingUpdate]:
        """
        Consume and return all pending liquidity events for this pool.
        """

        pool_address = get_checksum_address(pool_address)

        # Fetch any pending updates
        pending_events = self._liquidity_events.get(pool_address)

        if pending_events is None:
            return ()

        # Mint/Burn events must be applied in chronological order to ensure effective
        # invariant checks
        sorted_events = tuple(
            sorted(
                pending_events,
                key=lambda event: (event.block_number, event.tx_index),
            )
        )

        # Clear pending events for this pool
        self._liquidity_events[pool_address] = []

        return tuple(
            UniswapV3PoolLiquidityMappingUpdate(
                block_number=event.block_number,
                liquidity=event.liquidity,
                tick_lower=event.tick_lower,
                tick_upper=event.tick_upper,
            )
            for event in sorted_events
        )

    def get_tick_bitmap(self, pool: ChecksumAddress | HexStr) -> dict[int, UniswapV3BitmapAtWord]:
        try:
            tick_bitmap: dict[int, UniswapV3BitmapAtWord] = self._liquidity_snapshot[
                get_checksum_address(pool)
            ]["tick_bitmap"]
        except KeyError:
            return {}
        else:
            return tick_bitmap

    def get_tick_data(self, pool: ChecksumAddress | HexStr) -> dict[int, UniswapV3LiquidityAtTick]:
        try:
            tick_data: dict[int, UniswapV3LiquidityAtTick] = self._liquidity_snapshot[
                get_checksum_address(pool)
            ]["tick_data"]
        except KeyError:
            return {}
        else:
            return tick_data

    def update_snapshot(
        self,
        pool: ChecksumAddress | HexStr,
        tick_data: dict[int, UniswapV3LiquidityAtTick],
        tick_bitmap: dict[int, UniswapV3BitmapAtWord],
    ) -> None:
        pool_address = get_checksum_address(pool)

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
