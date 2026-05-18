"""Compare end-to-end latency: separate vs combined subscriptions.

Measures the time from when the first WS message for a new block arrives
to when all events for the block have been received.

For separate mode, this measures:
  - Time from first WS message → all events received (includes batching delay)
  - Specifically, how long after the first V3/Sync event does the last
    Transfer event arrive? (Transfers always arrive last in separate mode)

For combined mode, this measures:
  - Same metric, but events arrive in logIndex order already
  - The "last event" should arrive no later than separate mode's last event

Key question: does the batching in separate mode introduce detectable
latency for events that arrive late in the batch order?
"""

import asyncio
import json
import os
import time
from dataclasses import dataclass, field


WS_URI = os.environ.get("ETHEREUM_ARCHIVE_NODE_WS_URI", "ws://node:8546")

V3_SWAP_TOPIC = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
V2_SYNC_TOPIC = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"
TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"

SHORT_MAP = {"v3": "V", "v2": "S", "tr": "T"}


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
        return "V"
    elif t == V2_SYNC_TOPIC:
        return "S"
    elif t == TRANSFER_TOPIC:
        return "T"
    return "other"


@dataclass
class Event:
    timestamp: float
    block_number: int | None
    log_index: int | None
    topic: str  # V, S, T


@dataclass
class BlockTiming:
    block_number: int
    first_event_time: float
    last_event_time: float
    event_count: int
    # When each topic type first appeared
    first_v3_time: float | None = None
    first_sync_time: float | None = None
    first_transfer_time: float | None = None
    # When each topic type last appeared
    last_v3_time: float | None = None
    last_sync_time: float | None = None
    last_transfer_time: float | None = None
    # LogIndex range
    min_log_index: int | None = None
    max_log_index: int | None = None
    # Are these in logIndex order?
    inversions: int = 0
    total_events: int = 0


def analyze_block_timings(events: list[Event]) -> dict[int, BlockTiming]:
    by_block: dict[int, list[Event]] = {}
    for e in events:
        if e.block_number is not None:
            by_block.setdefault(e.block_number, []).append(e)

    timings: dict[int, BlockTiming] = {}
    for blk, blk_events in by_block.items():
        if len(blk_events) < 2:
            continue

        t = BlockTiming(
            block_number=blk,
            first_event_time=min(e.timestamp for e in blk_events),
            last_event_time=max(e.timestamp for e in blk_events),
            event_count=len(blk_events),
        )

        # Count inversions
        sorted_by_arrival = sorted(blk_events, key=lambda e: e.timestamp)
        for i in range(len(sorted_by_arrival)):
            for j in range(i + 1, len(sorted_by_arrival)):
                if (sorted_by_arrival[i].log_index or 0) > (sorted_by_arrival[j].log_index or 0):
                    t.inversions += 1

        for e in blk_events:
            if e.topic == "V":
                if t.first_v3_time is None:
                    t.first_v3_time = e.timestamp
                t.last_v3_time = e.timestamp
            elif e.topic == "S":
                if t.first_sync_time is None:
                    t.first_sync_time = e.timestamp
                t.last_sync_time = e.timestamp
            elif e.topic == "T":
                if t.first_transfer_time is None:
                    t.first_transfer_time = e.timestamp
                t.last_transfer_time = e.timestamp

            if e.log_index is not None:
                if t.min_log_index is None:
                    t.min_log_index = e.log_index
                    t.max_log_index = e.log_index
                else:
                    t.min_log_index = min(t.min_log_index, e.log_index)
                    t.max_log_index = max(t.max_log_index, e.log_index)

        t.total_events = len(blk_events)
        timings[blk] = t

    return timings


async def run_separate(ws, collect_seconds: int) -> tuple[list[Event], dict[int, BlockTiming]]:
    sub_requests = [
        ("v3", {"jsonrpc": "2.0", "id": 1, "method": "eth_subscribe",
                "params": ["logs", {"topics": [[V3_SWAP_TOPIC]]}]}),
        ("v2", {"jsonrpc": "2.0", "id": 2, "method": "eth_subscribe",
                "params": ["logs", {"topics": [[V2_SYNC_TOPIC]]}]}),
        ("tr", {"jsonrpc": "2.0", "id": 3, "method": "eth_subscribe",
                "params": ["logs", {"topics": [[TRANSFER_TOPIC]]}]}),
    ]

    sub_id_to_label: dict[str, str] = {}
    for label, req in sub_requests:
        await ws.send(json.dumps(req))

    for label, _ in sub_requests:
        resp = json.loads(await ws.recv())
        sid = resp.get("result", "")
        sub_id_to_label[sid] = label

    events: list[Event] = []
    try:
        async with asyncio.timeout(collect_seconds):
            async for raw_msg in ws:
                msg = json.loads(raw_msg)
                if msg.get("method") != "eth_subscription":
                    continue
                params = msg.get("params", {})
                result = params.get("result", {})
                sid = params.get("subscription", "")
                label = sub_id_to_label.get(sid, "unknown")

                events.append(Event(
                    timestamp=time.time(),
                    block_number=parse_int_field(result, "blockNumber"),
                    log_index=parse_int_field(result, "logIndex"),
                    topic=SHORT_MAP.get(label, "?"),
                ))
    except TimeoutError:
        pass

    return events, analyze_block_timings(events)


async def run_combined(ws, collect_seconds: int) -> tuple[list[Event], dict[int, BlockTiming]]:
    req = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_subscribe",
        "params": ["logs", {"topics": [[V3_SWAP_TOPIC, V2_SYNC_TOPIC, TRANSFER_TOPIC]]}],
    }
    await ws.send(json.dumps(req))
    resp = json.loads(await ws.recv())
    sub_id = resp.get("result", "")

    events: list[Event] = []
    try:
        async with asyncio.timeout(collect_seconds):
            async for raw_msg in ws:
                msg = json.loads(raw_msg)
                if msg.get("method") != "eth_subscription":
                    continue
                params = msg.get("params", {})
                result = params.get("result", {})
                if params.get("subscription") != sub_id:
                    continue

                events.append(Event(
                    timestamp=time.time(),
                    block_number=parse_int_field(result, "blockNumber"),
                    log_index=parse_int_field(result, "logIndex"),
                    topic=classify_topic(result.get("topics", [])),
                ))
    except TimeoutError:
        pass

    return events, analyze_block_timings(events)


def print_timing_analysis(timings: dict[int, BlockTiming], mode_name: str) -> None:
    print(f"\n{'=' * 80}")
    print(f"MODE: {mode_name}")
    print(f"{'=' * 80}")

    if not timings:
        print("No blocks with multiple events.")
        return

    # Aggregate stats
    spreads_ms: list[float] = []  # time from first to last event in block
    v3_first_to_transfer_first_ms: list[float] = []
    v3_first_to_transfer_last_ms: list[float] = []
    transfer_delay_from_block_start_ms: list[float] = []

    for blk, t in sorted(timings.items()):
        spread = (t.last_event_time - t.first_event_time) * 1000
        spreads_ms.append(spread)

        if t.first_v3_time is not None and t.first_transfer_time is not None:
            delay = (t.first_transfer_time - t.first_v3_time) * 1000
            v3_first_to_transfer_first_ms.append(delay)

        if t.first_v3_time is not None and t.last_transfer_time is not None:
            delay = (t.last_transfer_time - t.first_v3_time) * 1000
            v3_first_to_transfer_last_ms.append(delay)

        if t.first_v3_time is not None and t.first_transfer_time is not None:
            delay = (t.first_transfer_time - t.first_event_time) * 1000
            transfer_delay_from_block_start_ms.append(delay)

    # Print per-block detail (first 8)
    print(f"\nPer-block timing (first 8 blocks):")
    for blk, t in sorted(timings.items())[:8]:
        spread = (t.last_event_time - t.first_event_time) * 1000
        inv_str = f", {t.inversions} inversions" if t.inversions > 0 else ", ✓ ordered"

        v3_info = ""
        if t.first_v3_time is not None:
            v3_info = f"  V3: {((t.first_v3_time - t.first_event_time)*1000):.1f}-{((t.last_v3_time or t.first_v3_time - t.first_event_time + t.first_event_time - t.first_event_time)*1000):.1f}ms"

        parts = []
        if t.first_v3_time is not None:
            rel_start = (t.first_v3_time - t.first_event_time) * 1000
            rel_end = ((t.last_v3_time or t.first_v3_time) - t.first_event_time) * 1000
            parts.append(f"V3:[{rel_start:.1f}-{rel_end:.1f}]ms")
        if t.first_sync_time is not None:
            rel_start = (t.first_sync_time - t.first_event_time) * 1000
            rel_end = ((t.last_sync_time or t.first_sync_time) - t.first_event_time) * 1000
            parts.append(f"Sync:[{rel_start:.1f}-{rel_end:.1f}]ms")
        if t.first_transfer_time is not None:
            rel_start = (t.first_transfer_time - t.first_event_time) * 1000
            rel_end = ((t.last_transfer_time or t.first_transfer_time) - t.first_event_time) * 1000
            parts.append(f"Transfer:[{rel_start:.1f}-{rel_end:.1f}]ms")

        print(f"  Block {blk} ({t.event_count} events, spread={spread:.1f}ms{inv_str}):")
        print(f"    {'  '.join(parts)}")

    # Aggregate
    print(f"\nAggregate across {len(timings)} blocks:")
    if spreads_ms:
        avg = sum(spreads_ms) / len(spreads_ms)
        median = sorted(spreads_ms)[len(spreads_ms) // 2]
        print(f"  Block spread (first→last event): avg={avg:.1f}ms, median={median:.1f}ms, max={max(spreads_ms):.1f}ms, min={min(spreads_ms):.1f}ms")
    if v3_first_to_transfer_first_ms:
        avg = sum(v3_first_to_transfer_first_ms) / len(v3_first_to_transfer_first_ms)
        median = sorted(v3_first_to_transfer_first_ms)[len(v3_first_to_transfer_first_ms) // 2]
        print(f"  V3-first → Transfer-first: avg={avg:.1f}ms, median={median:.1f}ms, max={max(v3_first_to_transfer_first_ms):.1f}ms")
    if v3_first_to_transfer_last_ms:
        avg = sum(v3_first_to_transfer_last_ms) / len(v3_first_to_transfer_last_ms)
        median = sorted(v3_first_to_transfer_last_ms)[len(v3_first_to_transfer_last_ms) // 2]
        print(f"  V3-first → Transfer-last: avg={avg:.1f}ms, median={median:.1f}ms, max={max(v3_first_to_transfer_last_ms):.1f}ms")
    if transfer_delay_from_block_start_ms:
        avg = sum(transfer_delay_from_block_start_ms) / len(transfer_delay_from_block_start_ms)
        median = sorted(transfer_delay_from_block_start_ms)[len(transfer_delay_from_block_start_ms) // 2]
        print(f"  Block-start → Transfer-first: avg={avg:.1f}ms, median={median:.1f}ms, max={max(transfer_delay_from_block_start_ms):.1f}ms")

    # Inversions
    total_inv = sum(t.inversions for t in timings.values())
    blocks_inv = sum(1 for t in timings.values() if t.inversions > 0)
    print(f"  Inversions: {total_inv} across {blocks_inv}/{len(timings)} blocks")


async def main() -> None:
    import websockets

    collect_seconds = int(os.environ.get("COLLECT_SECONDS", "60"))
    mode = os.environ.get("MODE", "all")

    if mode in ("separate", "all"):
        print("Running SEPARATE mode...")
        async with websockets.connect(WS_URI) as ws:
            _, timings = await run_separate(ws, collect_seconds)
        print_timing_analysis(timings, "Separate (3 subscriptions)")

    if mode in ("combined", "all"):
        print("\nRunning COMBINED mode...")
        async with websockets.connect(WS_URI) as ws:
            _, timings = await run_combined(ws, collect_seconds)
        print_timing_analysis(timings, "Combined (1 subscription, 3 topics)")


if __name__ == "__main__":
    asyncio.run(main())
