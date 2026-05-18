"""Investigate web3.py subscription event ordering.

Hypothesis: When three subscriptions are active (A=newHeads, B=logs, C=newHeads),
web3.py may deliver events out of RPC arrival order. On a new block, the node
sends newHeads FIRST, then all logs for that block. But web3.py's internal
pipeline (single shared _handler_subscription_queue, sequential processing,
response formatting) may reorder them.

This script:
1. Connects to a WS endpoint via web3.py
2. Subscribes to newHeads (A), all logs (B), newHeads (C)
3. Records timestamps and sequence numbers for every event
4. After a timeout, reports whether newHeads events always arrived before
   the logs for the same block number
"""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass, field
from typing import Any

from web3 import AsyncWeb3
from web3.providers.persistent import WebSocketProvider
from web3.utils.subscriptions import (
    LogsSubscription,
    NewHeadsSubscription,
)


@dataclass
class EventRecord:
    """A single subscription event with timing metadata."""

    seq: int  # global arrival order
    timestamp: float  # time.time() when handler was invoked
    label: str  # subscription label
    block_number: int | None  # extracted from the event data
    raw: dict[str, Any] = field(repr=False)


def extract_block_number(event: dict[str, Any]) -> int | None:
    """Try to extract a block number from a subscription event."""
    # web3.py returns AttributeDict, not plain dict
    from web3.datastructures import AttributeDict

    if isinstance(event, AttributeDict):
        event = dict(event)

    # newHeads events have 'number' as a hex string
    num = event.get("number")
    if num is not None:
        if isinstance(num, str):
            return int(num, 16)
        if isinstance(num, int):
            return num
        if hasattr(num, "__int__"):
            return int(num)
    # log events have 'blockNumber' as a hex string
    bn = event.get("blockNumber")
    if bn is not None:
        if isinstance(bn, str):
            return int(bn, 16)
        if isinstance(bn, int):
            return bn
        if hasattr(bn, "__int__"):
            return int(bn)
    return None


async def main() -> None:
    ws_uri = "ws://node:8546"
    collect_seconds = 60  # how long to collect events

    provider = WebSocketProvider(ws_uri)
    await provider.connect()
    w3 = AsyncWeb3(provider)
    sm = w3.subscription_manager

    # Shared state
    events: list[EventRecord] = []
    seq_counter = 0
    dispatch_counter = 0  # increments when create_task is called

    def make_handler(label: str, *, delay: float = 0.0):
        async def handler(context) -> None:  # type: ignore[misc]  # noqa: ANN001
            nonlocal seq_counter
            result = context.result
            # Convert AttributeDict and HexBytes for block number extraction
            if isinstance(result, dict):
                raw_for_extract = dict(result)
            elif hasattr(result, '__getitem__') and hasattr(result, 'get'):
                # AttributeDict or similar dict-like object
                raw_for_extract = dict(result)  # type: ignore[misc]
            else:
                raw_for_extract = {"value": result}

            # Artificial delay to simulate slow handler work
            if delay > 0:
                await asyncio.sleep(delay)

            seq_counter += 1
            block_number = extract_block_number(raw_for_extract)
            events.append(
                EventRecord(
                    seq=seq_counter,
                    timestamp=time.time(),
                    label=label,
                    block_number=block_number,
                    raw=raw_for_extract,
                )
            )

        return handler

    # Enable parallel handler dispatch (default is False / sequential)
    sm.parallelize = True
    print(f"parallelize={sm.parallelize}", flush=True)

    # Subscribe: A=newHeads, B=logs, C=newHeads
    await sm.subscribe(
        NewHeadsSubscription(label="heads-A", handler=make_handler("heads-A"))
    )
    await sm.subscribe(
        LogsSubscription(label="logs-B", handler=make_handler("logs-B", delay=0.001))
    )
    await sm.subscribe(
        NewHeadsSubscription(label="heads-C", handler=make_handler("heads-C"))
    )

    print(f"Subscribed. Collecting events for {collect_seconds}s...", flush=True)

    # Run handle_subscriptions for a limited time
    try:
        await asyncio.wait_for(
            sm.handle_subscriptions(run_forever=True),
            timeout=collect_seconds,
        )
    except asyncio.TimeoutError:
        pass

    print(f"Collected {len(events)} events. Analyzing...", flush=True)

    await sm.unsubscribe_all()
    await provider.disconnect()

    # --- Analysis ---
    if not events:
        print("No events collected!")
        return

    print()
    print("=" * 80)
    print("EVENT ARRIVAL ORDER")
    print("=" * 80)
    print()
    print(f"{'Seq':>4}  {'Timestamp':>17}  {'Label':>9}  {'Block#':>9}  Delta(ms)")
    print("-" * 80)

    prev_ts = events[0].timestamp
    for e in events:
        delta_ms = (e.timestamp - prev_ts) * 1000
        prev_ts = e.timestamp
        blk = f"{e.block_number}" if e.block_number is not None else "?"
        print(f"{e.seq:>4}  {e.timestamp:>17.6f}  {e.label:>9}  {blk:>9}  {delta_ms:>8.1f}")

    # Group events by block number to check per-block ordering
    print()
    print("=" * 80)
    print("PER-BLOCK ARRIVAL ORDER (is newHeads always before logs?)")
    print("=" * 80)
    print()

    by_block: dict[int, list[EventRecord]] = {}
    for e in events:
        if e.block_number is not None:
            by_block.setdefault(e.block_number, []).append(e)

    violations = 0
    for blk_num in sorted(by_block):
        block_events = by_block[blk_num]
        head_seq = [e.seq for e in block_events if e.label.startswith("heads")]
        log_seq = [e.seq for e in block_events if e.label.startswith("logs")]

        if not head_seq or not log_seq:
            continue

        heads_before = all(h < l for h in head_seq for l in log_seq)
        logs_before = all(l < h for l in log_seq for h in head_seq)

        order_desc = "heads-first \u2713" if heads_before else ("logs-first \u2717" if logs_before else "mixed \u2717")
        if not heads_before:
            violations += 1

        n_heads = len(head_seq)
        n_logs = len(log_seq)
        print(f"  Block {blk_num}: {order_desc}  ({n_heads} heads, {n_logs} logs)")

    # Cross-block interleaving check
    print()
    print("=" * 80)
    print("CROSS-BLOCK INTERLEAVING")
    print("(logs from block N+1 before newHeads for block N+1?)")
    print("=" * 80)
    print()

    cross_violations = 0
    for blk_num in sorted(by_block):
        blk_events = by_block[blk_num]
        head_seqs = [e.seq for e in blk_events if e.label.startswith("heads")]
        log_seqs = [e.seq for e in blk_events if e.label.startswith("logs")]

        if not head_seqs or not log_seqs:
            continue

        earliest_head = min(head_seqs)
        logs_before_heads = [s for s in log_seqs if s < earliest_head]

        if logs_before_heads:
            cross_violations += 1
            print(
                f"  Block {blk_num}: {len(logs_before_heads)} logs arrived BEFORE "
                f"newHeads (seq {min(logs_before_heads)}"
                f"-{max(logs_before_heads)}"
                f" < head seq {earliest_head})"
            )

    if cross_violations == 0:
        print("  No cross-block interleaving violations detected.")

    print()
    total_blocks_with_both = sum(
        1 for blk_events in by_block.values()
        if any(e.label.startswith("heads") for e in blk_events)
        and any(e.label.startswith("logs") for e in blk_events)
    )
    print(f"Blocks with both heads+logs events: {total_blocks_with_both}")
    print(f"Per-block ordering violations (logs before heads): {violations}")
    print(f"Cross-block interleaving violations: {cross_violations}")

    if violations > 0 or cross_violations > 0:
        print()
        print("\u26a0\ufe0f  VIOLATION DETECTED")
    else:
        print()
        print("\u2713  No violations detected: newHeads always arrived before logs")


if __name__ == "__main__":
    asyncio.run(main())
