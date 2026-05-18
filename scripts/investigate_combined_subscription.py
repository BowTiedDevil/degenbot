"""Investigate combined vs separate log subscriptions.

Central question: if we subscribe to ALL topics in a single eth_subscribe
call with a topics filter [[A, B, C]], does the node deliver events in
logIndex order?

If yes, this eliminates the need for a log-index sorter. The tradeoff is
that the user gets a single stream of events they must dispatch themselves
(or the system dispatches by topic inspection).

Compare:
  - Mode A: 3 separate subscriptions (one per topic) — current approach
  - Mode B: 1 combined subscription with topics [[A, B, C]]
  - Mode C: 1 unfiltered subscription (all logs, no topic filter) —
            the node sends ALL logs in logIndex order, but the volume is
            much higher

For each mode, measure:
  1. Whether events arrive in logIndex order (inversions)
  2. End-to-end latency from block production to first handler call
  3. Total event volume
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

ALL_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"


@dataclass
class RawEvent:
    seq: int
    timestamp: float
    mode: str  # "separate" or "combined"
    subscription_label: str
    block_number: int | None
    log_index: int | None
    topic0: str | None = None


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
    """Map first topic to a short label for the three we care about."""
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


async def run_combined_mode(ws, collect_seconds: int) -> list[RawEvent]:
    """Mode B: One combined subscription with topics [[V3, V2, Transfer]]."""
    req = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_subscribe",
        "params": ["logs", {"topics": [[V3_SWAP_TOPIC, V2_SYNC_TOPIC, TRANSFER_TOPIC]]}],
    }
    await ws.send(json.dumps(req))
    resp = json.loads(await ws.recv())
    sub_id = resp.get("result", "")
    print(f"  Combined subscription: {sub_id}")

    events: list[RawEvent] = []
    seq = 0
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

                seq += 1
                topics = result.get("topics", [])
                events.append(RawEvent(
                    seq=seq,
                    timestamp=time.time(),
                    mode="combined",
                    subscription_label="combined",
                    block_number=parse_int_field(result, "blockNumber"),
                    log_index=parse_int_field(result, "logIndex"),
                    topic0=classify_topic(topics),
                ))
    except TimeoutError:
        pass

    return events


async def run_unfiltered_mode(ws, collect_seconds: int) -> list[RawEvent]:
    """Mode C: One unfiltered subscription (all logs)."""
    req = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_subscribe",
        "params": ["logs", {}],
    }
    await ws.send(json.dumps(req))
    resp = json.loads(await ws.recv())
    sub_id = resp.get("result", "")
    print(f"  Unfiltered subscription: {sub_id}")

    events: list[RawEvent] = []
    seq = 0
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

                seq += 1
                topics = result.get("topics", [])
                events.append(RawEvent(
                    seq=seq,
                    timestamp=time.time(),
                    mode="unfiltered",
                    subscription_label="unfiltered",
                    block_number=parse_int_field(result, "blockNumber"),
                    log_index=parse_int_field(result, "logIndex"),
                    topic0=classify_topic(topics),
                ))
    except TimeoutError:
        pass

    return events


async def run_separate_mode(ws, collect_seconds: int) -> list[RawEvent]:
    """Mode A: Three separate subscriptions (one per topic)."""
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
        print(f"  Separate '{label}': {sid}")

    events: list[RawEvent] = []
    seq = 0
    try:
        async with asyncio.timeout(collect_seconds):
            async for raw_msg in ws:
                msg = json.loads(raw_msg)
                if msg.get("method") != "eth_subscription":
                    continue
                params = msg.get("params", {})
                sid = params.get("subscription", "")
                result = params.get("result", {})
                label = sub_id_to_label.get(sid, "unknown")

                seq += 1
                topics = result.get("topics", [])
                events.append(RawEvent(
                    seq=seq,
                    timestamp=time.time(),
                    mode="separate",
                    subscription_label=label,
                    block_number=parse_int_field(result, "blockNumber"),
                    log_index=parse_int_field(result, "logIndex"),
                    topic0=classify_topic(topics),
                ))
    except TimeoutError:
        pass

    return events


def analyze_events(events: list[RawEvent], mode_name: str) -> None:
    """Analyze a list of events for ordering inversions and characteristics."""
    # Filter to events with both block_number and log_index
    indexed = [e for e in events if e.block_number is not None and e.log_index is not None]
    by_block: dict[int, list[RawEvent]] = {}
    for e in indexed:
        by_block.setdefault(e.block_number, []).append(e)

    print(f"\n{'=' * 80}")
    print(f"MODE: {mode_name}")
    print(f"{'=' * 80}")
    print(f"Total events: {len(events)}")
    print(f"Events with block+logIndex: {len(indexed)}")
    print(f"Blocks represented: {len(by_block)}")

    if not indexed:
        return

    # ── Global ordering analysis ──────────────────────────────────────
    # For combined/unfiltered mode, "cross-subscription" isn't meaningful
    # since there's only one subscription. Instead we track:
    #   - same-topic inversions (same topic0, out of logIndex order)
    #   - cross-topic inversions (different topic0, out of logIndex order)
    # For separate mode, subscription_label IS the grouping.
    total_inversions = 0
    total_cross_inv = 0  # cross-topic or cross-subscription
    total_same_inv = 0   # same-topic or same-subscription
    total_pairs = 0
    total_blocks_with_inv = 0
    blocks_analyzed = 0

    # For grouping: use subscription_label in separate mode, topic0 in combined/unfiltered
    def group_key(e: RawEvent) -> str:
        if mode_name.startswith("Separate"):
            return e.subscription_label
        return e.topic0 or "unknown"

    for blk_num in sorted(by_block):
        block_events = sorted(by_block[blk_num], key=lambda e: e.seq)
        if len(block_events) < 2:
            continue

        groups_here = set(group_key(e) for e in block_events)
        if len(groups_here) < 2:
            continue

        blocks_analyzed += 1
        n = len(block_events)
        total_pairs += n * (n - 1) // 2

        inv = 0
        cross_inv = 0
        same_inv = 0
        for i in range(n):
            for j in range(i + 1, n):
                if (block_events[i].log_index or 0) > (block_events[j].log_index or 0):
                    inv += 1
                    if group_key(block_events[i]) == group_key(block_events[j]):
                        same_inv += 1
                    else:
                        cross_inv += 1

        total_inversions += inv
        total_cross_inv += cross_inv
        total_same_inv += same_inv
        if inv > 0:
            total_blocks_with_inv += 1

    print(f"\nBlocks with multiple groups: {blocks_analyzed}")
    print(f"Blocks with inversions: {total_blocks_with_inv}")
    print(f"Total pairs: {total_pairs}")
    print(f"Total inversions: {total_inversions}")
    if mode_name.startswith("Separate"):
        print(f"  Cross-subscription: {total_cross_inv}")
        print(f"  Same-subscription:  {total_same_inv}")
    else:
        print(f"  Cross-topic: {total_cross_inv}")
        print(f"  Same-topic:  {total_same_inv}")
    pct = (total_inversions / total_pairs * 100) if total_pairs > 0 else 0
    print(f"  Inversion rate: {pct:.2f}%")

    # ── Per-block detail for first few blocks ────────────────────────
    print(f"\nPer-block detail (first 5 multi-group blocks):")
    shown = 0
    for blk_num in sorted(by_block):
        block_events = sorted(by_block[blk_num], key=lambda e: e.seq)
        if len(block_events) < 2:
            continue
        groups_here = set(group_key(e) for e in block_events)
        if len(groups_here) < 2:
            continue

        n = len(block_events)
        inv = sum(
            1 for i in range(n) for j in range(i + 1, n)
            if (block_events[i].log_index or 0) > (block_events[j].log_index or 0)
        )

        # Build arrival sequence string
        seq_chars = []
        short_map = {"v3": "V", "v2": "S", "tr": "T", "combined": "?", "unfiltered": "?"}
        for e in block_events[:50]:
            if e.topic0 and e.topic0 in ("V", "S", "T"):
                seq_chars.append(e.topic0)
            else:
                seq_chars.append(short_map.get(e.subscription_label, "?"))

        arrival_seq = "".join(seq_chars)

        # Build chain-order sequence
        chain_events = sorted(block_events, key=lambda e: e.log_index or 0)
        chain_chars = []
        for e in chain_events[:50]:
            if e.topic0 and e.topic0 in ("V", "S", "T"):
                chain_chars.append(e.topic0)
            else:
                chain_chars.append(short_map.get(e.subscription_label, "?"))
        chain_seq = "".join(chain_chars)

        matches = "✓ MATCHES chain" if inv == 0 else f"✗ ({inv} inversions)"
        print(f"  Block {blk_num} ({n} events) {matches}:")
        if inv > 0:
            print(f"    Arrival: {arrival_seq}")
            print(f"    Chain:   {chain_seq}")

        shown += 1
        if shown >= 5:
            break

    # ── Run-length analysis (topic batching in arrival order) ──────
    print(f"\nRun-length analysis (topic batching):")
    total_ws_runs = 0
    total_chain_runs = 0
    total_ws_events = 0
    total_chain_events = 0

    for blk_num in sorted(by_block):
        block_events = sorted(by_block[blk_num], key=lambda e: e.seq)
        if len(block_events) < 2:
            continue
        groups_here = set(group_key(e) for e in block_events)
        if len(groups_here) < 2:
            continue

        # Arrival order runs — by topic0 (the event type, not subscription)
        def topic_label(e: RawEvent) -> str:
            return e.topic0 if e.topic0 and e.topic0 in ("V", "S", "T") else e.subscription_label

        runs = []
        current = topic_label(block_events[0])
        count = 1
        for e in block_events[1:]:
            t = topic_label(e)
            if t == current:
                count += 1
            else:
                runs.append(count)
                current = t
                count = 1
        runs.append(count)
        total_ws_runs += len(runs)
        total_ws_events += len(block_events)

        # Chain order runs
        chain_events = sorted(block_events, key=lambda e: e.log_index or 0)
        chain_runs = []
        current = topic_label(chain_events[0])
        count = 1
        for e in chain_events[1:]:
            t = topic_label(e)
            if t == current:
                count += 1
            else:
                chain_runs.append(count)
                current = t
                count = 1
        chain_runs.append(count)
        total_chain_runs += len(chain_runs)
        total_chain_events += len(chain_events)

    if total_ws_events > 0:
        print(f"  Arrival order: {total_ws_runs} runs across {total_ws_events} events (avg {total_ws_events/total_ws_runs:.1f} events/run)")
    if total_chain_events > 0:
        print(f"  Chain order: {total_chain_runs} runs across {total_chain_events} events (avg {total_chain_events/total_chain_runs:.1f} events/run)")


async def main() -> None:
    import websockets

    collect_seconds = int(os.environ.get("COLLECT_SECONDS", "60"))
    mode = os.environ.get("MODE", "all")  # "separate", "combined", "unfiltered", or "all"

    # ── Mode A: Separate subscriptions ────────────────────────────────
    if mode in ("separate", "all"):
        print("\n" + "=" * 80)
        print("MODE A: THREE SEPARATE SUBSCRIPTIONS (one per topic)")
        print("=" * 80)
        async with websockets.connect(WS_URI) as ws:
            events = await run_separate_mode(ws, collect_seconds)
        analyze_events(events, "Separate (3 subscriptions)")

    # ── Mode B: Combined subscription ────────────────────────────────
    if mode in ("combined", "all"):
        print("\n" + "=" * 80)
        print("MODE B: ONE COMBINED SUBSCRIPTION (topics: [[V3, V2, Transfer]])")
        print("=" * 80)
        async with websockets.connect(WS_URI) as ws:
            events = await run_combined_mode(ws, collect_seconds)
        analyze_events(events, "Combined (1 subscription, 3 topics)")

    # ── Mode C: Unfiltered subscription ───────────────────────────────
    if mode in ("unfiltered", "all"):
        print("\n" + "=" * 80)
        print("MODE C: ONE UNFILTERED SUBSCRIPTION (all logs)")
        print("=" * 80)
        async with websockets.connect(WS_URI) as ws:
            events = await run_unfiltered_mode(ws, collect_seconds)
        analyze_events(events, "Unfiltered (1 subscription, no topic filter)")


if __name__ == "__main__":
    asyncio.run(main())
