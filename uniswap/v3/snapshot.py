import json
from abc import ABC
from typing import Dict, List, TextIO, Tuple

from brownie import web3 as brownie_web3  # type: ignore
from eth_typing import ChecksumAddress
from web3 import Web3
from web3._utils.events import get_event_data
from web3._utils.filters import construct_event_filter_params

from degenbot.logging import logger
from degenbot.uniswap.abi import UNISWAP_V3_POOL_ABI


class V3LiquiditySnapshot(ABC):
    """
    Retrieve and maintain liquidity information for a Uniswap V3 liquidity pool.
    """

    def __init__(
        self, file: TextIO, snapshot_end_block: int, block_span: int = 10_000
    ):
        self.liquidity_snapshot = {}
        json_liquidity_snapshot = json.load(file)
        snapshot_last_block = json_liquidity_snapshot["snapshot_block"]

        logger.info(
            f"Loaded LP snapshot: {len(json_liquidity_snapshot)} pools @ block {snapshot_last_block}"
        )

        for pool_address, snapshot in [
            (k, v)
            for k, v in json_liquidity_snapshot.items()
            if k not in ["snapshot_block"]
        ]:
            self.liquidity_snapshot[pool_address] = {
                "tick_bitmap": {
                    int(k): v for k, v in snapshot["tick_bitmap"].items()
                },
                "tick_data": {
                    int(k): v for k, v in snapshot["tick_data"].items()
                },
            }

        v3pool = Web3().eth.contract(abi=UNISWAP_V3_POOL_ABI)

        self.liquidity_events: Dict[
            str, List[Tuple[int, int, Tuple[int, int, int]]]
        ] = dict()

        for event in [v3pool.events.Mint, v3pool.events.Burn]:
            logger.info(f"processing {event.event_name} events")
            event_abi = event._get_event_abi()
            start_block = snapshot_last_block + 1

            while True:
                end_block = min(
                    snapshot_end_block, start_block + block_span - 1
                )

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

                    pool_address = decoded_event["address"]
                    block = decoded_event["blockNumber"]
                    tx_index = decoded_event["transactionIndex"]
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

                    self.liquidity_events[pool_address].append(
                        (
                            block,
                            tx_index,
                            (
                                liquidity,
                                tick_lower,
                                tick_upper,
                            ),
                        )
                    )

                logger.info(
                    f"Fetched events: block span [{start_block},{end_block}]"
                )

                if end_block == snapshot_end_block:
                    break
                else:
                    start_block = end_block + 1

    def get_tick_bitmap(self, pool):
        # print("getting tick bitmap")
        try:
            return self.liquidity_snapshot[pool]["tick_bitmap"]
        except KeyError:
            return None

    def get_tick_data(self, pool):
        # print("getting tick data")
        try:
            return self.liquidity_snapshot[pool]["tick_data"]
        except KeyError:
            return None

    def get_events(self, pool):
        # print("getting events")
        try:
            self.liquidity_events[pool]
        except KeyError:
            yield None
        else:
            for event in self.liquidity_events[pool]:
                yield event

    def has_events(self, pool):
        # print("has events?")
        try:
            self.liquidity_events[pool]
        except KeyError:
            return False
        else:
            return True
