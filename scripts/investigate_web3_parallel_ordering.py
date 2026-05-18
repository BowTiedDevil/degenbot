"""Investigate web3.py parallel handler execution order.

With parallelize=True, web3.py dispatches handlers as asyncio.Tasks.
The dispatch order preserves queue order (RPC arrival), but handler
COMPLETION order depends on handler latency.

This test verifies: with parallelize=True and a slow log handler, does
a heads handler for block N complete BEFORE log handlers for block N
start executing? Or do log handlers begin execution before the heads
handler has finished?
"""

import asyncio
import os
import time
from dataclasses import dataclass, field


WS_URI = os.environ.get("ETHEREUM_ARCHIVE_NODE_WS_URI", "ws://node:8546")


@dataclass
class ExecutionRecord:
    """Records when a handler started and finished executing."""
    label: str
    block_number: int | None
    dispatch_seq: int  # order in which web3.py dispatched (dequeued) this event
    start_time: float
    end_time: float


async def main() -> None:
    from web3 import AsyncWeb3
    from web3.providers.persistent import WebSocketProvider
    from web3.utils.subscriptions import NewHeadsSubscription, LogsSubscription
    from web3.datastructures import AttributeDict

    collect_seconds = 30
    provider = WebSocketProvider(WS_URI)
    await provider.connect()
    w3 = AsyncWeb3(provider)
    sm = w3.subscription_manager

    # Enable parallel mode
    sm.parallelize = True

    records: list[ExecutionRecord] = []
    dispatch_counter = 0

    def extract_block_number(result) -> int | None:
        d = dict(result) if isinstance(result, AttributeDict) or hasattr(result, 'get') else {}
        for key in ("number", "blockNumber"):
            val = d.get(key)
            if val is not None:
                if isinstance(val, str):
                    return int(val, 16)
                if isinstance(val, int):
                    return val
                if hasattr(val, "__int__"):
                    return int(val)
        return None

    def make_handler(label: str, *, delay: float = 0.0):
        async def handler(context) -> None:  # type: ignore[misc]  # noqa: ANN001
            nonlocal dispatch_counter
            # The dispatch_seq captures the order this handler was invoked.
            # With parallelize=True, this is approximately completion order
            # of the PREVIOUS event, not dispatch order of THIS event.
            # But more importantly, we track actual start/end times.
            dispatch_counter += 1
            dispatch_seq = dispatch_counter

            result = context.result
            block_number = extract_block_number(result)
            start = time.time()

            if delay > 0:
                await asyncio.sleep(delay)

            end = time.time()
            records.append(
                ExecutionRecord(
                    label=label,
                    block_number=block_number,
                    dispatch_seq=dispatch_seq,
                    start_time=start,
                    end_time=end,
                )
            )

        return handler

    # heads handler: 5ms delay (fast — just updating a pool state)
    # logs handler: 50ms delay (slow — processing transaction details)
    await sm.subscribe(
        NewHeadsSubscription(label="heads", handler=make_handler("heads", delay=0.005))
    )
    await sm.subscribe(
        LogsSubscription(label="logs", handler=make_handler("logs", delay=0.050))
    )

    print(f"parallelize={sm.parallelize}, heads delay=5ms, logs delay=50ms", flush=True)
    print(f"Collecting events for {collect_seconds}s...", flush=True)

    try:
        await asyncio.wait_for(
            sm.handle_subscriptions(run_forever=True),
            timeout=collect_seconds,
        )
    except asyncio.TimeoutError:
        pass

    await sm.unsubscribe_all()
    await provider.disconnect()

    # Analysis: check if any log handler for block N started executing
    # BEFORE the heads handler for block N finished
    print()
    print("=" * 80)
    print("HANDLER EXECUTION OVERLAP ANALYSIS")
    print("(Does a log handler start before the heads handler finishes?)")
    print("=" * 80)
    print()

    # Group by block
    by_block: dict[int, list[ExecutionRecord]] = {}
    for r in records:
        if r.block_number is not None:
            by_block.setdefault(r.block_number, []).append(r)

    overlap_violations = 0
    for blk_num in sorted(by_block):
        block_records = by_block[blk_num]
        head_records = [r for r in block_records if r.label == "heads"]
        log_records = [r for r in block_records if r.label == "logs"]

        if not head_records or not log_records:
            continue

        # Check: does any log handler start before ALL heads handlers finish?
        latest_head_end = max(r.end_time for r in head_records)
        earliest_log_start = min(r.start_time for r in log_records)

        if earliest_log_start < latest_head_end:
            overlap_violations += 1
            gap_ms = (latest_head_end - earliest_log_start) * 1000
            print(
                f"  Block {blk_num}: OVERLAP! "
                f"Log started {gap_ms:.1f}ms BEFORE heads finished "
                f"(heads end={latest_head_end:.6f}, log start={earliest_log_start:.6f})"
            )
        else:
            gap_ms = (earliest_log_start - latest_head_end) * 1000
            print(
                f"  Block {blk_num}: OK — heads finished {gap_ms:.1f}ms before first log started "
                f"({len(head_records)} heads, {len(log_records)} logs)"
            )

    print()
    print(f"Total blocks with heads+logs: "
          f"{sum(1 for b in by_block.values() if any(r.label=='heads' for r in b) and any(r.label=='logs' for r in b))}")
    print(f"Blocks where logs started before heads finished: {overlap_violations}")

    if overlap_violations > 0:
        print()
        print("⚠️  OVERLAP DETECTED: With parallelize=True, log handlers can")
        print("   start executing BEFORE the heads handler for the same block")
        print("   has finished. This means a consumer that relies on heads")
        print("   arriving first (e.g., updating pool state before processing")
        print("   logs) CANNOT rely on web3.py's parallel mode for ordering.")
    else:
        print()
        print("✓  No overlap: heads handler always completed before log handlers started")


if __name__ == "__main__":
    asyncio.run(main())
