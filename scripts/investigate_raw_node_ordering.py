"""Investigate raw WS message ordering from the RPC node.

Bypasses web3.py entirely. Opens a raw WebSocket connection, sends
three eth_subscribe requests for different log topics, and records
the exact order in which the node sends subscription events.

This definitively determines whether the batching is caused by the
RPC node or by web3.py's internal pipeline.
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

LABELS = {
    "sub_v3": "V3-Swap",
    "sub_v2": "V2-Sync",
    "sub_tr": "Transfer",
}

SHORT = {
    "sub_v3": "V",
    "sub_v2": "S",
    "sub_tr": "T",
}


@dataclass
class RawEvent:
    seq: int
    timestamp: float
    subscription_id: str
    label: str
    block_number: int | None
    log_index: int | None
    tx_index: int | None


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

    collect_seconds = int(os.environ.get("COLLECT_SECONDS", "60"))

    async with websockets.connect(WS_URI) as ws:
        # Subscribe to three different log topics
        sub_requests = [
            ("sub_v3", {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "eth_subscribe",
                "params": ["logs", {"topics": [V3_SWAP_TOPIC]}],
            }),
            ("sub_v2", {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "eth_subscribe",
                "params": ["logs", {"topics": [V2_SYNC_TOPIC]}],
            }),
            ("sub_tr", {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "eth_subscribe",
                "params": ["logs", {"topics": [TRANSFER_TOPIC]}],
            }),
        ]

        sub_id_to_label: dict[str, str] = {}

        # Send all subscribe requests
        for label, req in sub_requests:
            await ws.send(json.dumps(req))

        # Read subscription confirmations
        for label, req in sub_requests:
            resp = json.loads(await ws.recv())
            sid = resp.get("result")
            sub_id_to_label[sid] = label
            print(f"  {label}: subscription_id={sid}")

        print(f"\nCollecting raw WS events for {collect_seconds}s...", flush=True)

        events: list[RawEvent] = []
        seq = 0

        try:
            async with asyncio.timeout(collect_seconds):
                async for raw_msg in ws:
                    msg = json.loads(raw_msg)

                    # Only process subscription events
                    if msg.get("method") != "eth_subscription":
                        continue

                    params = msg.get("params", {})
                    sid = params.get("subscription")
                    result = params.get("result", {})

                    if sid is None:
                        continue

                    label = sub_id_to_label.get(sid, "unknown")
                    block_number = parse_int_field(result, "blockNumber")
                    log_index = parse_int_field(result, "logIndex")
                    tx_index = parse_int_field(result, "transactionIndex")

                    seq += 1
                    events.append(RawEvent(
                        seq=seq,
                        timestamp=time.time(),
                        subscription_id=sid,
                        label=label,
                        block_number=block_number,
                        log_index=log_index,
                        tx_index=log_index,
                    ))

        except TimeoutError:
            pass

    # ── Analysis ──────────────────────────────────────────────────────────
    print(f"\nTotal raw WS events: {len(events)}")

    from collections import Counter
    label_counts = Counter(e.label for e in events)
    print("Events per subscription:")
    for label, count in sorted(label_counts.items()):
        print(f"  {label}: {count}")

    # Group by block
    by_block: dict[int, list[RawEvent]] = {}
    for e in events:
        if e.block_number is not None and e.log_index is not None:
            by_block.setdefault(e.block_number, []).append(e)

    print(f"Blocks with indexed events: {len(by_block)}")

    print()
    print("=" * 80)
    print("RAW WS MESSAGE ORDER")
    print("Does the NODE send subscription events in subscription-batches")
    print("or interleaved by logIndex?")
    print("=" * 80)
    print()

    for blk_num in sorted(by_block):
        block_events = by_block[blk_num]
        if len(block_events) < 2:
            continue

        labels_here = set(e.label for e in block_events)
        if len(labels_here) < 2:
            continue

        by_ws = sorted(block_events, key=lambda e: e.seq)
        by_chain = sorted(block_events, key=lambda e: e.log_index or 0)

        # Count inversions
        n = len(by_ws)
        inversions = 0
        cross_inv = 0
        same_inv = 0
        for i in range(n):
            for j in range(i + 1, n):
                if (by_ws[i].log_index or 0) > (by_ws[j].log_index or 0):
                    inversions += 1
                    if by_ws[i].label == by_ws[j].label:
                        same_inv += 1
                    else:
                        cross_inv += 1

        total_pairs = n * (n - 1) // 2
        pct = (inversions / total_pairs * 100) if total_pairs > 0 else 0

        ws_seq_str = "".join(SHORT.get(e.label, "?") for e in by_ws[:50])
        chain_seq_str = "".join(SHORT.get(e.label, "?") for e in by_chain[:50])

        ws_transitions = sum(1 for i in range(1, len(by_ws)) if by_ws[i].label != by_ws[i-1].label)
        chain_transitions = sum(1 for i in range(1, len(by_chain)) if by_chain[i].label != by_chain[i-1].label)

        matches = "✓ MATCHES chain" if ws_seq_str == chain_seq_str else "✗ DIFFERS from chain"

        print(f"  Block {blk_num} ({n} events, {pct:.1f}% inverted) {matches}:")
        print(f"    WS order ({ws_transitions} transitions): {ws_seq_str}")
        print(f"    Chain     ({chain_transitions} transitions): {chain_seq_str}")
        print(f"    Inversions: {inversions}/{total_pairs} — cross-sub: {cross_inv}, same-sub: {same_inv}")

        # Show first 25 raw events
        print(f"    First 25 events by WS arrival:")
        for e in by_ws[:25]:
            print(f"      ws={e.seq:>5}  {SHORT.get(e.label,'?')}  logIdx={e.log_index:>4}")
        print()

    # ── "Run length" analysis: are same-subscription events clumped? ─────
    print()
    print("=" * 80)
    print("RUN-LENGTH ANALYSIS")
    print("How long are same-label runs in WS arrival vs chain order?")
    print("Long runs = strong batching. Short runs = interleaved.")
    print("=" * 80)
    print()

    for blk_num in sorted(by_block):
        block_events = by_block[blk_num]
        if len(block_events) < 2:
            continue
        labels_here = set(e.label for e in block_events)
        if len(labels_here) < 2:
            continue

        by_ws = sorted(block_events, key=lambda e: e.seq)
        by_chain = sorted(block_events, key=lambda e: e.log_index or 0)

        def run_lengths(events_list):
            runs = []
            current = events_list[0].label
            count = 1
            for e in events_list[1:]:
                if e.label == current:
                    count += 1
                else:
                    runs.append((current, count))
                    current = e.label
                    count = 1
            runs.append((current, count))
            return runs

        ws_runs = run_lengths(by_ws)
        chain_runs = run_lengths(by_chain)

        ws_avg = sum(r[1] for r in ws_runs) / len(ws_runs) if ws_runs else 0
        chain_avg = sum(r[1] for r in chain_runs) / len(chain_runs) if chain_runs else 0

        print(f"  Block {blk_num}:")
        print(f"    WS runs: {len(ws_runs)}, avg length: {ws_avg:.1f}, max: {max(r[1] for r in ws_runs)}")
        print(f"    Chain runs: {len(chain_runs)}, avg length: {chain_avg:.1f}, max: {max(r[1] for r in chain_runs)}")
        print()


if __name__ == "__main__":
    asyncio.run(main())
