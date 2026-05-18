"""Measure unfiltered subscription: event volume, ordering, and block spread.

Records timestamps for each WS message to measure actual block spread
(the time from first to last event for a given block).
"""

import asyncio
import json
import os
import time
from collections import Counter


WS_URI = os.environ.get("ETHEREUM_ARCHIVE_NODE_WS_URI", "ws://node:8546")

V3_SWAP_TOPIC = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
V2_SYNC_TOPIC = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"
TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"

TOPIC_SET = {V3_SWAP_TOPIC, V2_SYNC_TOPIC, TRANSFER_TOPIC}


def parse_int(val) -> int | None:
    if val is None:
        return None
    if isinstance(val, str):
        return int(val, 16) if val.startswith("0x") else int(val)
    if isinstance(val, int):
        return val
    if hasattr(val, "__int__"):
        return int(val)
    return None


async def main() -> None:
    import websockets

    collect_seconds = int(os.environ.get("COLLECT_SECONDS", "60"))

    async with websockets.connect(WS_URI) as ws:
        req = {"jsonrpc": "2.0", "id": 1, "method": "eth_subscribe", "params": ["logs", {}]}
        await ws.send(json.dumps(req))
        resp = json.loads(await ws.recv())
        sub_id = resp.get("result", "")
        print(f"Unfiltered subscription: {sub_id}")
        print(f"Collecting for {collect_seconds}s...\n")

        # Collect (timestamp, result) pairs
        events: list[tuple[float, dict]] = []
        try:
            async with asyncio.timeout(collect_seconds):
                async for raw_msg in ws:
                    msg = json.loads(raw_msg)
                    if msg.get("method") != "eth_subscription":
                        continue
                    result = msg.get("params", {}).get("result", {})
                    if msg.get("params", {}).get("subscription") != sub_id:
                        continue
                    events.append((time.time(), result))
        except TimeoutError:
            pass

    if not events:
        print("No events received.")
        return

    # Group by block
    by_block: dict[int, list[tuple[float, dict]]] = {}
    for ts, result in events:
        bn = parse_int(result.get("blockNumber"))
        if bn is not None:
            by_block.setdefault(bn, []).append((ts, result))

    print(f"Total events: {len(events)}")
    print(f"Blocks: {sorted(by_block.keys())}")

    # Per-block analysis
    print(f"\n{'Block':>10} {'Events':>7} {'Spread':>8} {'V3':>5} {'V2':>5} {'Tr':>5} {'Other':>6} {'Invs':>6}")
    total_events = 0
    total_inversions = 0
    total_topic_v3 = 0
    total_topic_v2 = 0
    total_topic_tr = 0
    total_topic_other = 0
    spreads = []

    for blk_num in sorted(by_block):
        block_data = by_block[blk_num]
        n = len(block_data)
        total_events += n

        # Spread
        timestamps = [ts for ts, _ in block_data]
        spread_ms = (max(timestamps) - min(timestamps)) * 1000
        spreads.append(spread_ms)

        # Topic breakdown
        topic_counts = Counter()
        for _, result in block_data:
            topics = result.get("topics", [])
            t0 = topics[0] if topics else None
            if t0 == V3_SWAP_TOPIC:
                topic_counts["V3"] += 1
            elif t0 == V2_SYNC_TOPIC:
                topic_counts["V2"] += 1
            elif t0 == TRANSFER_TOPIC:
                topic_counts["Tr"] += 1
            else:
                topic_counts["Other"] += 1

        total_topic_v3 += topic_counts["V3"]
        total_topic_v2 += topic_counts["V2"]
        total_topic_tr += topic_counts["Tr"]
        total_topic_other += topic_counts["Other"]

        # Inversions
        indexed = []
        for idx, (ts, result) in enumerate(block_data):
            li = parse_int(result.get("logIndex"))
            if li is not None:
                indexed.append((idx, li))

        inversions = 0
        for i in range(len(indexed)):
            for j in range(i + 1, len(indexed)):
                if indexed[i][1] > indexed[j][1]:
                    inversions += 1
        total_inversions += inversions

        ordered = "✓" if inversions == 0 else f"✗ ({inversions})"
        print(f"{blk_num:>10} {n:>7} {spread_ms:>7.1f}ms {topic_counts['V3']:>5} {topic_counts['V2']:>5} {topic_counts['Tr']:>5} {topic_counts['Other']:>6} {ordered}")

    print(f"\n{'='*80}")
    print("SUMMARY")
    print(f"{'='*80}")
    print(f"Total events: {total_events}")
    print(f"  V3 Swap:   {total_topic_v3} ({total_topic_v3/total_events*100:.1f}%)")
    print(f"  V2 Sync:   {total_topic_v2} ({total_topic_v2/total_events*100:.1f}%)")
    print(f"  Transfer:  {total_topic_tr} ({total_topic_tr/total_events*100:.1f}%)")
    print(f"  Other:     {total_topic_other} ({total_topic_other/total_events*100:.1f}%)")
    print(f"Total inversions: {total_inversions}")
    if spreads:
        print(f"Block spread (first→last event):")
        print(f"  Avg: {sum(spreads)/len(spreads):.1f}ms")
        print(f"  Median: {sorted(spreads)[len(spreads)//2]:.1f}ms")
        print(f"  Max: {max(spreads):.1f}ms")
        print(f"  Min: {min(spreads):.1f}ms")


if __name__ == "__main__":
    asyncio.run(main())
