"""Verify that combined subscriptions with both topics AND addresses
preserve logIndex ordering, and measure event volumes.

Tests:
1. subscribe_logs(topics=[[V3, V2, Transfer]], addresses=[addr1, addr2, ...])
   — Does this preserve logIndex order?
2. subscribe_logs(topics=[[V3, V2, Transfer]]) (no address filter)
   — Compare event volumes
3. subscribe_logs(addresses=[addr1, addr2, ...]) (no topic filter)
   — Compare event volumes

The goal: understand whether combining all topics + addresses into one
subscription is practical for the event-driven Bot architecture.
"""

import asyncio
import json
import os
import time
from dataclasses import dataclass


WS_URI = os.environ.get("ETHEREUM_ARCHIVE_NODE_WS_URI", "ws://node:8546")

V3_SWAP_TOPIC = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
V2_SYNC_TOPIC = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"
TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"

# Well-known mainnet addresses for high-volume Uniswap pools
ADDRESSES = [
    "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",  # USDC/WETH 0.05%
    "0x8ad599c3A0ff1De082311d1F8DE95781fD4Ec1c4",  # USDC/WETH 0.3%
    "0x4585FE77279bCc1d8FA80F4E3cE10f4087289DA2",  # WBTC/WETH 0.3%
    "0x3416cF6C708Da44DB2624D63ea0AAef7113527C6",  # USDC/WETH 1%
    "0xA6C788202BC8c4390E8f6A2f0592EC71950Df64f",  # DAI/USDC 0.01%
]


def parse_int_field(result: dict, key: str) -> int | None:
    val = result.get(key)
    if val is None:
        return None
    if isinstance(val, str):
        return int(val, 16) if val.startswith("0x") else int(val)
    if isinstance(val, int):
        return val
    return None


def classify_topic(topics: list[str] | None) -> str:
    if not topics:
        return "other"
    t = topics[0]
    if t == V3_SWAP_TOPIC:
        return "V3"
    elif t == V2_SYNC_TOPIC:
        return "V2"
    elif t == TRANSFER_TOPIC:
        return "T"
    return "other"


@dataclass
class Event:
    seq: int
    timestamp: float
    block_number: int | None
    log_index: int | None
    topic_type: str
    address: str | None


async def run_subscription(ws, label: str, params: dict, collect_seconds: int) -> list[Event]:
    """Run a single subscription and collect events."""
    req = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_subscribe",
        "params": ["logs", params],
    }
    await ws.send(json.dumps(req))
    resp = json.loads(await ws.recv())
    sub_id = resp.get("result", "")
    print(f"  {label}: subscription_id={sub_id}")

    events: list[Event] = []
    seq = 0
    try:
        async with asyncio.timeout(collect_seconds):
            async for raw_msg in ws:
                msg = json.loads(raw_msg)
                if msg.get("method") != "eth_subscription":
                    continue
                result = msg.get("params", {}).get("result", {})
                if msg.get("params", {}).get("subscription") != sub_id:
                    continue
                seq += 1
                events.append(Event(
                    seq=seq,
                    timestamp=time.time(),
                    block_number=parse_int_field(result, "blockNumber"),
                    log_index=parse_int_field(result, "logIndex"),
                    topic_type=classify_topic(result.get("topics", [])),
                    address=result.get("address"),
                ))
    except TimeoutError:
        pass

    return events


def analyze(events: list[Event], label: str) -> None:
    print(f"\n{'=' * 80}")
    print(f"ANALYSIS: {label}")
    print(f"{'=' * 80}")
    print(f"Total events: {len(events)}")

    indexed = [e for e in events if e.block_number is not None and e.log_index is not None]
    by_block: dict[int, list[Event]] = {}
    for e in indexed:
        by_block.setdefault(e.block_number, []).append(e)

    print(f"Events with block+logIndex: {len(indexed)}")
    print(f"Blocks: {sorted(by_block.keys())}")

    # Topic breakdown
    from collections import Counter
    topic_counts = Counter(e.topic_type for e in events)
    print(f"Topic breakdown: {dict(topic_counts)}")

    # Address breakdown
    addr_counts = Counter(e.address for e in events)
    print(f"Unique addresses: {len(addr_counts)}")
    for addr, count in addr_counts.most_common(5):
        short = addr[:10] + "..." if addr and len(addr) > 10 else str(addr)
        print(f"  {short}: {count}")

    # Ordering check
    total_inv = 0
    total_pairs = 0
    for blk_num in sorted(by_block):
        block_events = sorted(by_block[blk_num], key=lambda e: e.seq)
        n = len(block_events)
        if n < 2:
            continue
        pairs = n * (n - 1) // 2
        total_pairs += pairs
        inv = sum(
            1 for i in range(n) for j in range(i + 1, n)
            if (block_events[i].log_index or 0) > (block_events[j].log_index or 0)
        )
        total_inv += inv
        ordered = "✓" if inv == 0 else f"✗ ({inv} inversions)"
        print(f"  Block {blk_num}: {n} events, {ordered}")

    if total_pairs > 0:
        print(f"\nInversion rate: {total_inv / total_pairs * 100:.2f}% ({total_inv}/{total_pairs})")
    else:
        print("\nNo multi-event blocks to check ordering.")

    # Block timing
    if by_block:
        spreads = []
        for blk_num in sorted(by_block):
            block_events = by_block[blk_num]
            spread = (max(e.timestamp for e in block_events) - min(e.timestamp for e in block_events)) * 1000
            spreads.append(spread)
        avg = sum(spreads) / len(spreads)
        print(f"Avg block spread (first→last): {avg:.1f}ms")


async def main() -> None:
    import websockets

    collect_seconds = int(os.environ.get("COLLECT_SECONDS", "30"))
    mode = os.environ.get("MODE", "all")

    # Test 1: Combined topics + addresses
    if mode in ("topics_addr", "all"):
        print("\n" + "=" * 80)
        print("TEST: Combined subscription (topics [[V3, V2, Transfer]] + 5 addresses)")
        print("=" * 80)
        async with websockets.connect(WS_URI) as ws:
            events = await run_subscription(ws, "topics+addresses", {
                "topics": [[V3_SWAP_TOPIC, V2_SYNC_TOPIC, TRANSFER_TOPIC]],
                "address": ADDRESSES,
            }, collect_seconds)
        analyze(events, "Combined topics + addresses")

    # Test 2: Combined topics only (no address filter)
    if mode in ("topics_only", "all"):
        print("\n" + "=" * 80)
        print("TEST: Combined subscription (topics [[V3, V2, Transfer]] only, no address filter)")
        print("=" * 80)
        async with websockets.connect(WS_URI) as ws:
            events = await run_subscription(ws, "topics-only", {
                "topics": [[V3_SWAP_TOPIC, V2_SYNC_TOPIC, TRANSFER_TOPIC]],
            }, collect_seconds)
        analyze(events, "Combined topics only (no address filter)")

    # Test 3: Addresses only (no topic filter)
    if mode in ("addr_only", "all"):
        print("\n" + "=" * 80)
        print("TEST: Address-filtered subscription (5 addresses, no topic filter)")
        print("=" * 80)
        async with websockets.connect(WS_URI) as ws:
            events = await run_subscription(ws, "addresses-only", {
                "address": ADDRESSES,
            }, collect_seconds)
        analyze(events, "Address-filtered only (no topic filter)")


if __name__ == "__main__":
    asyncio.run(main())
