"""Measure unfiltered log subscription volume and dispatch cost.

Key questions:
1. How many events per block on mainnet with no filter?
2. How many are "useful" (match V3 Swap, V2 Sync, Transfer)?
3. What is the dispatch cost — topic0 lookup + address match?
4. Does the unfiltered stream still arrive in logIndex order?
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

# Simulated "registered" addresses — 5 major pools
REGISTERED_ADDRESSES = {
    "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640",  # USDC/WETH 0.05%
    "0x8ad599c3a0ff1de082311d1f8de95781fd4ec1c4",  # USDC/WETH 0.3%
    "0x4585fe77279bcc1d8fa80f4e3ce10f4087289da2",  # WBTC/WETH 0.3%
    "0x3416cf6c708da44db2624d63ea0aaef7113527c6",  # USDC/WETH 1%
    "0xa6c788202bc8c4390e8f6a2f0592ec71950df64f",  # DAI/USDC 0.01%
}

TOPIC_SET = {V3_SWAP_TOPIC, V2_SYNC_TOPIC, TRANSFER_TOPIC}


@dataclass
class BlockStats:
    block_number: int
    total_events: int
    matched_topic: int        # topic0 is in our topic set
    matched_topic_addr: int   # topic0 matches AND address is registered
    dispatch_lookup_ns: int   # total time spent on dict lookups (topic + addr)
    inversions: int = 0
    spread_ms: float = 0.0


def parse_int_field(result: dict, key: str) -> int | None:
    val = result.get(key)
    if val is None:
        return None
    if isinstance(val, str):
        return int(val, 16) if val.startswith("0x") else int(val)
    if isinstance(val, int):
        return val
    return None


async def main() -> None:
    import websockets

    collect_seconds = int(os.environ.get("COLLECT_SECONDS", "90"))

    async with websockets.connect(WS_URI) as ws:
        req = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_subscribe",
            "params": ["logs", {}],
        }
        await ws.send(json.dumps(req))
        resp = json.loads(await ws.recv())
        sub_id = resp.get("result", "")
        print(f"Unfiltered subscription: {sub_id}")
        print(f"Collecting for {collect_seconds}s...\n")

        # Per-block tracking
        current_block: int | None = None
        block_events: list[tuple[int, dict]] = []  # (seq, result)
        block_stats: list[BlockStats] = []
        seq = 0
        all_events: list[dict] = []

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
                    all_events.append(result)

                    block_number = parse_int_field(result, "blockNumber")
                    if block_number is None:
                        continue

                    # Block transition — compute stats for previous block
                    if current_block is not None and block_number != current_block:
                        stats = _compute_block_stats(current_block, block_events)
                        if stats is not None:
                            block_stats.append(stats)
                        block_events = []

                    current_block = block_number
                    block_events.append((seq, result))

        except TimeoutError:
            pass

        # Final block
        if current_block is not None and block_events:
            stats = _compute_block_stats(current_block, block_events)
            if stats is not None:
                block_stats.append(stats)

    # ── Results ───────────────────────────────────────────────────────────
    print(f"Total events received: {len(all_events)}")
    print(f"Blocks analyzed: {len(block_stats)}")

    if not block_stats:
        return

    total_events = sum(s.total_events for s in block_stats)
    total_matched_topic = sum(s.matched_topic for s in block_stats)
    total_matched_both = sum(s.matched_topic_addr for s in block_stats)
    total_inversions = sum(s.inversions for s in block_stats)
    total_dispatch_ns = sum(s.dispatch_lookup_ns for s in block_stats)

    print(f"\n{'='*80}")
    print("AGGREGATE STATISTICS")
    print(f"{'='*80}")
    print(f"Total events across all blocks: {total_events}")
    print(f"Avg events/block: {total_events / len(block_stats):.0f}")
    print(f"Matched topic0 in set: {total_matched_topic} ({total_matched_topic/total_events*100:.1f}%)")
    print(f"Matched topic0 AND address registered: {total_matched_both} ({total_matched_both/total_events*100:.1f}%)")
    print(f"Dispatched (useful): {total_matched_both}")
    print(f"Discarded: {total_events - total_matched_both} ({(total_events - total_matched_both)/total_events*100:.1f}%)")
    print(f"Total inversions: {total_inversions}")
    print(f"Total dispatch lookup time: {total_dispatch_ns/1_000_000:.1f}ms across {total_events} events")
    print(f"Avg lookup per event: {total_dispatch_ns/total_events:.0f}ns")

    print(f"\n{'='*80}")
    print("PER-BLOCK DETAIL")
    print(f"{'='*80}")
    print(f"{'Block':>10} {'Total':>7} {'MchTpc':>7} {'MchBoth':>8} {'Disc%':>6} {'Invs':>6} {'Spread':>8} {'Dispμs':>8}")
    for s in block_stats[:20]:
        disc_pct = (1 - s.matched_topic_addr / s.total_events) * 100 if s.total_events else 0
        disp_us = s.dispatch_lookup_ns / s.total_events / 1000 if s.total_events else 0
        print(f"{s.block_number:>10} {s.total_events:>7} {s.matched_topic:>7} {s.matched_topic_addr:>8} {disc_pct:>5.1f}% {s.inversions:>6} {s.spread_ms:>7.1f}ms {disp_us:>7.1f}μs")

    # Spread stats
    spreads = [s.spread_ms for s in block_stats if s.total_events > 1]
    if spreads:
        print(f"\nBlock spread (first→last event):")
        print(f"  Avg: {sum(spreads)/len(spreads):.1f}ms")
        print(f"  Median: {sorted(spreads)[len(spreads)//2]:.1f}ms")
        print(f"  Max: {max(spreads):.1f}ms")
        print(f"  Min: {min(spreads):.1f}ms")


def _compute_block_stats(block_number: int, events: list[tuple[int, dict]]) -> BlockStats | None:
    """Compute stats for a single block."""
    if not events:
        return None

    total = len(events)
    matched_topic = 0
    matched_both = 0
    dispatch_ns = 0
    timestamps = []

    for _seq, result in events:
        topics = result.get("topics", [])
        address = result.get("address", "")
        ts_entry = result.get("__timestamp__", 0)

        # Simulate the dispatch lookup
        t0 = time.perf_counter_ns()
        topic0 = topics[0] if topics else None
        is_topic_match = topic0 in TOPIC_SET
        is_addr_match = isinstance(address, str) and address.lower() in REGISTERED_ADDRESSES
        dispatch_ns += time.perf_counter_ns() - t0

        if is_topic_match:
            matched_topic += 1
            if is_addr_match:
                matched_both += 1

    # Count inversions
    indexed = []
    for seq_val, result in events:
        log_index = parse_int_field(result, "logIndex")
        if log_index is not None:
            indexed.append((seq_val, log_index))

    inversions = 0
    for i in range(len(indexed)):
        for j in range(i + 1, len(indexed)):
            if indexed[i][1] > indexed[j][1]:
                inversions += 1

    return BlockStats(
        block_number=block_number,
        total_events=total,
        matched_topic=matched_topic,
        matched_topic_addr=matched_both,
        dispatch_lookup_ns=dispatch_ns,
        inversions=inversions,
    )


if __name__ == "__main__":
    asyncio.run(main())
