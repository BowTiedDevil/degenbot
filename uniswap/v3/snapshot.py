import dataclasses
import json
from typing import Dict, List, Optional, TextIO

from brownie import web3 as brownie_web3  # type: ignore
from eth_typing import ChecksumAddress
from web3 import Web3
from web3._utils.events import get_event_data
from web3._utils.filters import construct_event_filter_params

from degenbot.logging import logger
from degenbot.uniswap.abi import UNISWAP_V3_POOL_ABI
from degenbot.uniswap.v3.v3_liquidity_pool import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
)


@dataclasses.dataclass(slots=True)
class UniswapV3LiquidityEvent:
    block_number: int
    liquidity: int
    tick_lower: int
    tick_upper: int


class UniswapV3LiquiditySnapshot:
    """
    Retrieve and maintain liquidity information for a Uniswap V3 liquidity pool.
    """

    def __init__(self, file: TextIO):
        self.liquidity_snapshot = {}
        json_liquidity_snapshot = json.load(file)
        self.newest_block = json_liquidity_snapshot["snapshot_block"]

        logger.info(
            f"Loaded LP snapshot: {len(json_liquidity_snapshot)} pools @ block {self.newest_block}"
        )

        for pool_address, pool_liquidity_snapshot in [
            (k, v)
            for k, v in json_liquidity_snapshot.items()
            if k not in ["snapshot_block"]
        ]:
            self.liquidity_snapshot[Web3.toChecksumAddress(pool_address)] = {
                "tick_bitmap": {
                    int(k): v
                    for k, v in pool_liquidity_snapshot["tick_bitmap"].items()
                },
                "tick_data": {
                    int(k): v
                    for k, v in pool_liquidity_snapshot["tick_data"].items()
                },
            }

        self.liquidity_events: Dict[
            ChecksumAddress, List[UniswapV3LiquidityEvent]
        ] = dict()

    def get_tick_bitmap(
        self, pool
    ) -> Optional[Dict[int, UniswapV3BitmapAtWord]]:
        # print("getting tick bitmap")
        try:
            return self.liquidity_snapshot[pool]["tick_bitmap"]
        except KeyError:
            return None

    def get_tick_data(
        self, pool
    ) -> Optional[Dict[int, UniswapV3LiquidityAtTick]]:
        # print("getting tick data")
        try:
            return self.liquidity_snapshot[pool]["tick_data"]
        except KeyError:
            return None

    def get_events(self, pool) -> List[UniswapV3LiquidityEvent]:
        try:
            self.liquidity_events[pool]
        except KeyError:
            return []
        else:
            events = self.liquidity_events[pool]
            del self.liquidity_events[pool]
            return events

    def has_events(self, pool) -> bool:
        # print("has events?")
        try:
            self.liquidity_events[pool]
        except KeyError:
            return False
        else:
            return True

    def add_event(
        self,
        pool: ChecksumAddress,
        block_number: int,
        liquidity: int,
        tick_lower: int,
        tick_upper: int,
    ) -> None:
        # logger.info(f"EVENT: {pool=},{liquidity=},{tick_lower=},{tick_upper=}")
        self.liquidity_events[pool].append(
            UniswapV3LiquidityEvent(
                block_number=block_number,
                liquidity=liquidity,
                tick_lower=tick_lower,
                tick_upper=tick_upper,
            )
        )

    def update_to(
        self,
        last_block: int,
        span: int = 1000,
    ) -> None:
        logger.info(f"Updating snapshot to block {last_block}")

        v3pool = Web3().eth.contract(abi=UNISWAP_V3_POOL_ABI)

        for event in [v3pool.events.Mint, v3pool.events.Burn]:
            logger.info(f"processing {event.event_name} events")
            event_abi = event._get_event_abi()
            start_block = self.newest_block + 1

            while True:
                end_block = min(last_block, start_block + span - 1)

                _, event_filter_params = construct_event_filter_params(
                    event_abi=event_abi,
                    abi_codec=brownie_web3.codec,
                    fromBlock=start_block,
                    toBlock=end_block,
                )

                try:
                    event_logs = brownie_web3.eth.get_logs(event_filter_params)
                except:
                    continue

                for log in event_logs:
                    decoded_event = get_event_data(
                        brownie_web3.codec, event_abi, log
                    )

                    pool_address = Web3.toChecksumAddress(
                        decoded_event["address"]
                    )
                    liquidity_block = decoded_event["blockNumber"]
                    liquidity = decoded_event["args"]["amount"] * (
                        -1 if decoded_event["event"] == "Burn" else 1
                    )
                    tick_lower = decoded_event["args"]["tickLower"]
                    tick_upper = decoded_event["args"]["tickUpper"]

                    # skip zero liquidity events
                    if liquidity == 0:
                        continue

                    try:
                        self.liquidity_events[pool_address]
                    except KeyError:
                        self.liquidity_events[pool_address] = []

                    # self.liquidity_events[pool_address].append(
                    #     (
                    #         block,
                    #         tx_index,
                    #         (
                    #             liquidity,
                    #             tick_lower,
                    #             tick_upper,
                    #         ),
                    #     )
                    # )

                    self.liquidity_events[pool_address].append(
                        UniswapV3LiquidityEvent(
                            block_number=liquidity_block,
                            liquidity=liquidity,
                            tick_lower=tick_lower,
                            tick_upper=tick_upper,
                        )
                    )

                logger.info(
                    f"Fetched events: block span [{start_block},{end_block}]"
                )

                if end_block == last_block:
                    break
                else:
                    start_block = end_block + 1

        logger.info(f"Updated snapshot to block {last_block}")
