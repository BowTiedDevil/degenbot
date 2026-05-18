"""Proof-of-concept: LogIndex sorter with live WS data.

Subscribes to V3 Swap, V2 Sync, and Transfer events via separate
subscriptions, then reorders events by logIndex within each block
before dispatching to handlers.

Validates that the sorted output matches the on-chain logIndex order.
"""

import asyncio
import os
import time
from dataclasses import dataclass, field


WS_URI = os.environ.get("ETHEREUM_ARCHIVE_NODE_WS_URI", "ws://node:8546")

V3_SWAP_TOPIC = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
V2_SYNC_TOPIC = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"
TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"


@dataclass
class SortedEvent:
    label: str
    block_number: int
    log_index: int
    dispatch_order: int  # order in which this event was dispatched to the handler


class LogIndexSorter:
    """Reorders subscription-batched log events by logIndex within each block."""

    def __init__(self) -> None:
        self._buffer: dict[int, list[tuple[str, dict]]] = {}
        self._dispatch_counter = 0

    def ingest(self, label: str, event: dict) -> list[SortedEvent]:
        """Buffer a log event. Returns sorted events for flushed blocks."""
        block_number = self._parse_int(event, "blockNumber")
        if block_number is None:
            return []

        self._buffer.setdefault(block_number, []).append((label, event))

        # Flush previous blocks (any block < current that hasn't been flushed)
        results: list[SortedEvent] = []
        blocks_to_flush = sorted(b for b in self._buffer if b < block_number)
        for blk in blocks_to_flush:
            results.extend(self._flush_block(blk))

        return results

    def flush_all(self) -> list[SortedEvent]:
        """Flush all buffered events (e.g., on shutdown)."""
        results: list[SortedEvent] = []
        for blk in sorted(self._buffer):
            results.extend(self._flush_block(blk))
        return results

    def _flush_block(self, block_number: int) -> list[SortedEvent]:
        events = self._buffer.pop(block_number, None)
        if events is None:
            return []

        # Sort by logIndex
        def sort_key(pair: tuple[str, dict]) -> int:
            return self._parse_int(pair[1], "logIndex") or 0

        events.sort(key=sort_key)

        results: list[SortedEvent] = []
        for label, event in events:
            self._dispatch_counter += 1
            results.append(SortedEvent(
                label=label,
                block_number=block_number,
                log_index=self._parse_int(event, "logIndex") or 0,
                dispatch_order=self._dispatch_counter,
            ))
        return results

    @staticmethod
    def _parse_int(event: dict, key: str) -> int | None:
        val = event.get(key)
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
    from web3 import AsyncWeb3
    from web3.providers.persistent import WebSocketProvider
    from web3.utils.subscriptions import LogsSubscription

    collect_seconds = int(os.environ.get("COLLECT_SECONDS", "60"))

    provider = WebSocketProvider(WS_URI)
    await provider.connect()
    w3 = AsyncWeb3(provider)
    sm = w3.subscription_manager

    sorter = LogIndexSorter()
    # Raw arrival events (before sorting) for comparison
    raw_arrival: list[SortedEvent] = []
    # Sorted dispatch events
    sorted_dispatch: list[SortedEvent] = []

    raw_counter = 0
    dispatch_counter = 0

    def make_handler(label: str):
        async def handler(context) -> None:  # type: ignore[misc]  # noqa: ANN001
            nonlocal raw_counter, dispatch_counter
            result = context.result
            d = dict(result) if hasattr(result, 'get') else {}
            block_number = LogIndexSorter._parse_int(d, "blockNumber")
            log_index = LogIndexSorter._parse_int(d, "logIndex")

            if block_number is None or log_index is None:
                return

            raw_counter += 1
            raw_arrival.append(SortedEvent(
                label=label,
                block_number=block_number,
                log_index=log_index,
                dispatch_order=raw_counter,
            ))

            # Run through the sorter
            sorted_events = sorter.ingest(label, d)
            for ev in sorted_events:
                dispatch_counter += 1
                sorted_dispatch.append(SortedEvent(
                    label=ev.label,
                    block_number=ev.block_number,
                    log_index=ev.log_index,
                    dispatch_order=dispatch_counter,
                ))

        return handler

    await sm.subscribe(
        LogsSubscription(label="v3-swap", topics=[[V3_SWAP_TOPIC]], handler=make_handler("v3-swap"))
    )
    await sm.subscribe(
        LogsSubscription(label="v2-sync", topics=[[V2_SYNC_TOPIC]], handler=make_handler("v2-sync"))
    )
    await sm.subscribe(
        LogsSubscription(label="transfer", topics=[[TRANSFER_TOPIC]], handler=make_handler("transfer"))
    )

    print(f"Collecting for {collect_seconds}s...", flush=True)

    try:
        await asyncio.wait_for(
            sm.handle_subscriptions(run_forever=True),
            timeout=collect_seconds,
        )
    except asyncio.TimeoutError:
        pass

    # Flush remaining buffered events
    remaining = sorter.flush_all()
    for ev in remaining:
        dispatch_counter += 1
        sorted_dispatch.append(SortedEvent(
            label=ev.label,
            block_number=ev.block_number,
            log_index=ev.log_index,
            dispatch_order=dispatch_counter,
        ))

    await sm.unsubscribe_all()
    await provider.disconnect()

    # ── Analysis ──────────────────────────────────────────────────────────
    print(f"\nRaw arrival events: {len(raw_arrival)}")
    print(f"Sorted dispatch events: {len(sorted_dispatch)}")

    # Compare raw arrival order vs sorted order by block
    by_block_raw: dict[int, list[SortedEvent]] = {}
    for e in raw_arrival:
        by_block_raw.setdefault(e.block_number, []).append(e)

    by_block_sorted: dict[int, list[SortedEvent]] = {}
    for e in sorted_dispatch:
        by_block_sorted.setdefault(e.block_number, []).append(e)

    print(f"Blocks with raw events: {len(by_block_raw)}")
    print(f"Blocks with sorted events: {len(by_block_sorted)}")

    short = {"v3-swap": "V", "v2-sync": "S", "transfer": "T"}

    print()
    print("=" * 80)
    print("SORTING VERIFICATION")
    print("Compare raw arrival (batched) vs sorted dispatch (by logIndex)")
    print("=" * 80)
    print()

    for blk_num in sorted(by_block_raw):
        raw = sorted(by_block_raw.get(blk_num, []), key=lambda e: e.dispatch_order)
        srt = sorted(by_block_sorted.get(blk_num, []), key=lambda e: e.dispatch_order)

        if len(raw) < 2:
            continue

        # Check if sorted dispatch is in logIndex order
        sorted_log_indices = [e.log_index for e in srt]
        is_sorted = all(sorted_log_indices[i] <= sorted_log_indices[i+1] for i in range(len(sorted_log_indices)-1))

        # Check if raw arrival is in logIndex order
        raw_log_indices = [e.log_index for e in raw]
        is_raw_sorted = all(raw_log_indices[i] <= raw_log_indices[i+1] for i in range(len(raw_log_indices)-1))

        raw_label_seq = "".join(short.get(e.label, "?") for e in raw[:40])
        sorted_label_seq = "".join(short.get(e.label, "?") for e in srt[:40])

        raw_ok = "✓" if is_raw_sorted else "✗"
        sorted_ok = "✓" if is_sorted else "✗"

        # Count inversions in raw arrival
        n = len(raw)
        raw_inv = sum(1 for i in range(n) for j in range(i+1, n) if raw[i].log_index > raw[j].log_index)
        sorted_inv = sum(1 for i in range(len(srt)) for j in range(i+1, len(srt)) if srt[i].log_index > srt[j].log_index) if srt else 0

        print(f"  Block {blk_num} ({len(raw)} raw, {len(srt)} sorted):")
        print(f"    Raw arrival {raw_ok}: {raw_label_seq}  ({raw_inv} inversions)")
        print(f"    Sorted      {sorted_ok}: {sorted_label_seq}  ({sorted_inv} inversions)")
        print()

    if not by_block_sorted:
        print("  No sorted events — blocks may not have transitioned during collection.")
        print("  (The sorter only flushes when it sees a new block number.)")
        print()
        print("  Raw arrival order for the last block:")
        last_blk = max(by_block_raw.keys())
        raw = sorted(by_block_raw[last_blk], key=lambda e: e.dispatch_order)
        raw_label_seq = "".join(short.get(e.label, "?") for e in raw[:50])
        print(f"    Block {last_blk}: {raw_label_seq}")
        # Show that the raw order IS subscription-batched
        raw_log_indices = [e.log_index for e in raw]
        is_sorted = all(raw_log_indices[i] <= raw_log_indices[i+1] for i in range(len(raw_log_indices)-1))
        print(f"    logIndex-sorted: {'✓' if is_sorted else '✗'}")


if __name__ == "__main__":
    asyncio.run(main())
