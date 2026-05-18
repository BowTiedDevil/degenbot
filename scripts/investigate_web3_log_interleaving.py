"""Investigate web3.py log subscription interleaving.

Hypothesis: When multiple log subscriptions are active (filtered by topic),
web3.py delivers events in subscription-batches (AAAA-BBBB-CCCC) rather
than in the intra-block transaction order (AABBCAACBBCC) that the RPC
node actually sends them in.

This script:
1. Connects to a WS endpoint via web3.py
2. Subscribes to three separate log subscriptions:
   - A: V3 Swap events (topic 0xc42079f9...)
   - B: V2 Sync events (topic 0x1c411e9a...)
   - C: ERC20 Transfer events (topic 0xddf252ad...)
3. Records the sequence number, subscription label, and logIndex for each event
4. After a timeout, checks whether events from different subscriptions are
   interleaved by logIndex (block-ordered) or batched by subscription.
"""

import asyncio
import os
import time
from dataclasses import dataclass


WS_URI = os.environ.get("ETHEREUM_ARCHIVE_NODE_WS_URI", "ws://node:8546")


@dataclass
class EventRecord:
    seq: int
    timestamp: float
    label: str
    block_number: int | None
    tx_index: int | None       # transactionIndex
    log_index: int | None      # logIndex
    tx_hash: str | None


def extract_fields(result) -> dict:
    """Extract block number, tx index, log index, and tx hash from a log event."""
    d = dict(result) if hasattr(result, 'get') else {}
    block_number = None
    tx_index = None
    log_index = None
    tx_hash = None

    for key, target in [("blockNumber", "block_number"), ("transactionIndex", "tx_index"), ("logIndex", "log_index")]:
        val = d.get(key)
        if val is not None:
            if isinstance(val, str):
                parsed = int(val, 16) if val.startswith("0x") else int(val)
            elif isinstance(val, int):
                parsed = val
            elif hasattr(val, "__int__"):
                parsed = int(val)
            else:
                parsed = None
            if target == "block_number":
                block_number = parsed
            elif target == "tx_index":
                tx_index = parsed
            elif target == "log_index":
                log_index = parsed

    # tx_hash
    th = d.get("transactionHash")
    if th is not None:
        tx_hash = str(th) if not isinstance(th, str) else th

    return {
        "block_number": block_number,
        "tx_index": tx_index,
        "log_index": log_index,
        "tx_hash": tx_hash,
    }


async def main() -> None:
    from web3 import AsyncWeb3
    from web3.providers.persistent import WebSocketProvider
    from web3.utils.subscriptions import LogsSubscription

    collect_seconds = int(os.environ.get("COLLECT_SECONDS", "60"))

    provider = WebSocketProvider(WS_URI)
    await provider.connect()
    w3 = AsyncWeb3(provider)
    sm = w3.subscription_manager

    events: list[EventRecord] = []
    seq_counter = 0

    # Topic hashes
    V3_SWAP_TOPIC = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
    V2_SYNC_TOPIC = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"
    TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"

    def make_handler(label: str):
        async def handler(context) -> None:  # type: ignore[misc]  # noqa: ANN001
            nonlocal seq_counter
            fields = extract_fields(context.result)
            seq_counter += 1
            events.append(
                EventRecord(
                    seq=seq_counter,
                    timestamp=time.time(),
                    label=label,
                    block_number=fields["block_number"],
                    tx_index=fields["tx_index"],
                    log_index=fields["log_index"],
                    tx_hash=fields["tx_hash"],
                )
            )
        return handler

    # Three separate subscriptions, each filtered by one topic
    await sm.subscribe(
        LogsSubscription(
            label="v3-swap",
            topics=[[V3_SWAP_TOPIC]],
            handler=make_handler("v3-swap"),
        )
    )
    await sm.subscribe(
        LogsSubscription(
            label="v2-sync",
            topics=[[V2_SYNC_TOPIC]],
            handler=make_handler("v2-sync"),
        )
    )
    await sm.subscribe(
        LogsSubscription(
            label="transfer",
            topics=[[TRANSFER_TOPIC]],
            handler=make_handler("transfer"),
        )
    )

    print(f"Subscribed. Collecting for {collect_seconds}s...", flush=True)

    try:
        await asyncio.wait_for(
            sm.handle_subscriptions(run_forever=True),
            timeout=collect_seconds,
        )
    except asyncio.TimeoutError:
        pass

    await sm.unsubscribe_all()
    await provider.disconnect()

    # ── Analysis ──────────────────────────────────────────────────────────

    print(f"\nTotal events collected: {len(events)}")

    # Show a sample of events with logIndex
    print(f"\n{'Seq':>5}  {'Label':>9}  {'Block':>9}  {'TxIdx':>6}  {'LogIdx':>7}  {'LabelSeq'}")
    print("-" * 70)

    # Count per subscription
    from collections import Counter
    label_counts = Counter(e.label for e in events)
    print("\nEvents per subscription:")
    for label, count in sorted(label_counts.items()):
        print(f"  {label}: {count}")

    # Group by block for ordering analysis
    by_block: dict[int, list[EventRecord]] = {}
    for e in events:
        if e.block_number is not None and e.log_index is not None:
            by_block.setdefault(e.block_number, []).append(e)

    print(f"\nBlocks with indexed events: {len(by_block)}")

    # Check: are events batched by subscription within each block?
    # i.e. does the sequence go AAAAA-BBBBB-CCCCC (batched) or AABCBAC (interleaved)?
    print()
    print("=" * 80)
    print("PER-BLOCK ORDERING ANALYSIS")
    print("Is the event sequence batched by subscription, or interleaved by logIndex?")
    print("=" * 80)
    print()

    total_batched = 0
    total_interleaved = 0
    total_blocks_analyzed = 0

    for blk_num in sorted(by_block):
        block_events = by_block[blk_num]
        if len(block_events) < 2:
            continue

        # Sort events by their arrival sequence number
        block_events.sort(key=lambda e: e.seq)

        # Compute the "label sequence" — the order of labels by arrival
        arrival_labels = [e.label for e in block_events]

        # Also sort by logIndex to get the "on-chain order"
        by_log_idx = sorted(block_events, key=lambda e: (e.log_index or 0))
        chain_labels = [e.label for e in by_log_idx]

        # Check if all same-label events are contiguous in arrival order
        # (batched) vs interleaved
        is_batched = True
        seen_labels: set[str] = set()
        last_label = None
        for label in arrival_labels:
            if label != last_label:
                if label in seen_labels:
                    # This label appeared, then a different label, now it's back
                    is_batched = False
                    break
                seen_labels.add(label)
                last_label = label

        # Check if arrival order matches logIndex order
        arrival_log_indices = [e.log_index for e in block_events]
        chain_log_indices = [e.log_index for e in by_log_idx]
        matches_chain_order = arrival_log_indices == chain_log_indices

        total_blocks_analyzed += 1
        if is_batched:
            total_batched += 1
        else:
            total_interleaved += 1

        # Show a few representative blocks in detail
        if total_blocks_analyzed <= 5 or (not is_batched and total_interleaved <= 3):
            # Show per-event detail for small blocks, summary for large ones
            if len(block_events) <= 20:
                order_type = "BATCHED" if is_batched else "INTERLEAVED"
                chain_match = "✓ matches chain" if matches_chain_order else "✗ DIFFERS from chain"
                print(f"  Block {blk_num} ({order_type}, {len(block_events)} events) {chain_match}:")
                for e in block_events:
                    print(f"    seq={e.seq:>4}  {e.label:>9}  logIdx={e.log_index:>4}  txIdx={e.tx_index:>4}")
                print()
            else:
                # Compact: show arrival label sequence and chain label sequence
                arrival_seq = "".join(
                    {"v3-swap": "V", "v2-sync": "S", "transfer": "T"}.get(l, "?")
                    for l in arrival_labels[:50]
                )
                chain_seq = "".join(
                    {"v3-swap": "V", "v2-sync": "S", "transfer": "T"}.get(l, "?")
                    for l in chain_labels[:50]
                )

                order_type = "BATCHED" if is_batched else "INTERLEAVED"
                chain_match = "✓ matches chain" if matches_chain_order else "✗ DIFFERS from chain"

                # Compute how many label transitions in arrival vs chain
                arrival_transitions = sum(1 for i in range(1, len(arrival_labels)) if arrival_labels[i] != arrival_labels[i-1])
                chain_transitions = sum(1 for i in range(1, len(chain_labels)) if chain_labels[i] != chain_labels[i-1])

                print(f"  Block {blk_num} ({order_type}, {len(block_events)} events) {chain_match}:")
                print(f"    Arrival ({arrival_transitions} transitions): {arrival_seq}")
                print(f"    Chain   ({chain_transitions} transitions):   {chain_seq}")
                print()

    print(f"Blocks analyzed: {total_blocks_analyzed}")
    print(f"  Batched by subscription (AAAA-BBBB-CCCC): {total_batched}")
    print(f"  Interleaved (AABCBAC): {total_interleaved}")

    # ── Quantitative inversion count ────────────────────────────────────
    print()
    print("=" * 80)
    print("INVERSION ANALYSIS")
    print("How many event pairs are in the wrong order relative to logIndex?")
    print("=" * 80)
    print()

    for blk_num in sorted(by_block):
        block_events = by_block[blk_num]
        if len(block_events) < 2:
            continue

        # Sort by arrival seq to get the received order
        by_arrival = sorted(block_events, key=lambda e: e.seq)

        # Count inversions: pairs (i,j) where arrival_i < arrival_j but logIndex_i > logIndex_j
        n = len(by_arrival)
        inversions = 0
        for i in range(n):
            for j in range(i + 1, n):
                if (by_arrival[i].log_index or 0) > (by_arrival[j].log_index or 0):
                    inversions += 1

        total_pairs = n * (n - 1) // 2
        pct = (inversions / total_pairs * 100) if total_pairs > 0 else 0

        # Count how many inversions are cross-subscription vs same-subscription
        cross_inv = 0
        same_inv = 0
        for i in range(n):
            for j in range(i + 1, n):
                if (by_arrival[i].log_index or 0) > (by_arrival[j].log_index or 0):
                    if by_arrival[i].label == by_arrival[j].label:
                        same_inv += 1
                    else:
                        cross_inv += 1

        print(
            f"  Block {blk_num}: {inversions}/{total_pairs} pairs inverted "
            f"({pct:.1f}%) — "
            f"cross-sub: {cross_inv}, same-sub: {same_inv}"
        )

    # ── Sample block detail ─────────────────────────────────────────────
    print()
    print("=" * 80)
    print("SAMPLE BLOCK DETAIL (first block with mixed subscriptions)")
    print("Showing arrival order with logIndex for visual inspection")
    print("=" * 80)
    print()

    for blk_num in sorted(by_block):
        block_events = by_block[blk_num]
        by_arrival = sorted(block_events, key=lambda e: e.seq)

        # Count unique labels
        labels = set(e.label for e in by_arrival)
        if len(labels) < 2:
            continue

        # Show first 40 events with their logIndex
        short_label = {"v3-swap": "V3", "v2-sync": "V2", "transfer": "Tr"}
        print(f"  Block {blk_num} — first 40 of {len(by_arrival)} events:")
        print(f"    {'Seq':>4}  {'Label':>3}  {'LogIdx':>6}  {'TxIdx':>5}")
        for e in by_arrival[:40]:
            lbl = short_label.get(e.label, "??")
            print(f"    {e.seq:>4}  {lbl:>3}  {e.log_index:>6}  {e.tx_index:>5}")
        print()
        break  # Just show one block in detail


if __name__ == "__main__":
    asyncio.run(main())
