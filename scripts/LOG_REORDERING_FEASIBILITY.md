# Log Event Reordering — Feasibility Analysis (Superseded)

> **Superseded by `EVENT_DRIVEN_ARCHITECTURE.md`** which recommends an
> unfiltered log subscription that eliminates the reordering problem
> entirely. This document is retained for historical reference.
>
> The LogIndexSorter is no longer the recommended approach. It remains
> a valid EventMiddleware implementation for users who choose separate
> subscriptions and need cross-subscription ordering.

## Problem

When subscribing to multiple log topics via separate `eth_subscribe` calls, the
Ethereum node delivers events in **subscription batches**, not in on-chain
(logIndex) order. For example, with three subscriptions:
- A: V3 Swap events (topic `0xc42079f9...`)
- B: V2 Sync events (topic `0x1c411e9a...`)
- C: ERC-20 Transfer events (topic `0xddf252ad...`)

The node sends events as: **VVVVV-SSSSS-TTTTT** (batched by subscription), even
though the on-chain order is **TTVTTVTTTSTTV...** (interleaved by logIndex).

### Empirical evidence

See `scripts/investigate_raw_node_ordering.py` — raw WebSocket interception
bypassing web3.py entirely. Results against an Ethereum mainnet archive node:

| Block | Events | WS transitions | Chain transitions | % inverted | Same-sub inversions |
|-------|--------|---------------|-------------------|------------|-------------------|
| 25117804 | 791 | 2 | 70 | 4.3% | 0 |
| 25117805 | 777 | 16 | 59 | 2.8% | 0 |
| 25117806 | 431 | 4 | 54 | 4.4% | 0 |
| 25117807 | 763 | 12 | 52 | 3.2% | 0 |
| 25117808 | 858 | 39 | 84 | 3.0% | 0 |

**Key observations:**
1. All inversions are **cross-subscription** (same-sub: 0 in every block)
2. Within each subscription, events arrive in ascending logIndex order
3. The node batches aggressively: avg WS run length 21-264 events vs chain avg 8-14
4. Transfer events (most frequent) consistently arrive last despite having lowest logIndex
5. This is a **protocol-level behavior** — the node sends each subscription's
   events as a contiguous burst on the WebSocket connection

## Feasibility: LogIndex sorter in SubscriptionManager

### Approach: Block-boundary buffering + sort

1. **Detect block boundaries**: Every log event carries `blockNumber` (hex) and
   `logIndex` (hex). When the `blockNumber` changes from N to N+1, block N is
   complete.

2. **Buffer by block number**: Accumulate events in a
   `dict[int, list[Event]]` keyed by blockNumber.

3. **Flush on block transition**: When the first event for block N+1 arrives
   (from any subscription), sort block N's events by logIndex and emit them.

4. **Head signal as early flush**: A `newHeads` event for block N+1 is a
   stronger signal that block N's logs are complete. This can trigger an early
   flush of block N.

### Constraints

| Constraint | Impact |
|------------|--------|
| **Latency** | Adds one block (12s) of buffering delay. Events for block N aren't emitted until block N+1 starts. The newHeads signal can reduce this to ~100-200ms (time between last log and next head). |
| **Memory** | Must buffer all events for one block. Observed: 300-1200 events per block on mainnet. At ~500 bytes/event, that's ~150KB-600KB. Negligible. |
| **Block gap detection** | If no events arrive for a subscription in block N, that's fine — we just don't have events from that subscription for block N. |
| **Same-sub ordering** | Already correct (proven by 0 same-sub inversions). The sorter only needs to fix cross-sub ordering. |
| **Head arrival** | `newHeads` for block N always arrives before logs for block N (confirmed by previous investigation). So the head can serve as a "block N-1 is complete" signal. |
| **No logIndex on non-log events** | `newHeads` events don't have logIndex. They must be emitted immediately (not buffered) since they're the "start of block" signal. |

### Design sketch

```python
@dataclass
class LogIndexSorter:
    """Reorders subscription-batched log events by logIndex within each block."""

    _buffer: dict[int, list[tuple[str, Any]]]  # block_number → [(label, event)]
    _flushed_blocks: set[int]  # blocks already emitted

    def ingest(self, label: str, event: dict) -> list[tuple[str, Any]] | None:
        """Ingest an event. Returns sorted events for a completed block, or None."""

        # newHeads events: emit immediately, trigger flush for previous block
        if "number" in event and "logIndex" not in event:
            block_number = int(event["number"], 16)
            # Flush the previous block (N-1) since we now know block N started
            return self._flush_block(block_number - 1)

        # Log events: buffer by blockNumber
        block_number = int(event["blockNumber"], 16)
        self._buffer.setdefault(block_number, []).append((label, event))

        # If we see a new block number, flush the previous block
        # (alternative to head-triggered flush — works even without newHeads subscription)
        if block_number - 1 in self._buffer and block_number - 1 not in self._flushed_blocks:
            return self._flush_block(block_number - 1)

        return None

    def _flush_block(self, block_number: int) -> list[tuple[str, Any]] | None:
        events = self._buffer.pop(block_number, None)
        if events is None:
            return None
        self._flushed_blocks.add(block_number)
        # Sort by logIndex
        events.sort(key=lambda pair: int(pair[1].get("logIndex", "0x0"), 16))
        return events
```

### Integration point

The sorter fits between the pump tasks and the handler dispatch in
`SubscriptionManager.handle_subscriptions()`. Instead of dispatching each
event immediately after dequeuing, events are routed through the sorter:

```
pump → per-sub queue → _wait_for_any_event → LogIndexSorter → handler dispatch
```

The sorter is only active for log subscriptions (has `logIndex`). Non-log
events (newHeads, pending transactions) pass through unbuffered.

### Wait strategy options

| Strategy | Latency | Complexity | Robustness |
|----------|---------|------------|------------|
| **Block transition** (see event for N+1) | 1 block (12s) | Low | High |
| **Head-triggered** (newHeads for N+1 → flush N) | ~100-200ms | Medium | Medium (requires newHeads subscription) |
| **Timed flush** (flush block N after X seconds of no new events for N) | X seconds | Medium | Low (guess) |
| **Hybrid**: Head-triggered with block transition fallback | ~100-200ms (with heads) or 1 block (without) | Medium | High |

### Deduplication

If the user subscribes to overlapping topics (e.g., one subscription for all
V3 pool events and another for a specific V3 pool's Swap), the same log may
appear in multiple subscriptions. The sorter can deduplicate by
`(transactionHash, logIndex)` — a unique identifier for each log within a block.

### Open questions

1. **Should the sorter be opt-in or default?** Opt-in via a `sort_by_log_index: bool = False`
   parameter on `SubscriptionManager.subscribe()`. The latency tradeoff may not be
   acceptable for all use cases.

2. **Should non-log subscriptions be buffered?** No — `newHeads` and pending
   transaction events don't have logIndex and should be emitted immediately.

3. **What about `subscribe_full_blocks()`?** Full blocks contain all logs in
   order. If the user subscribes to `full_blocks` + filtered logs, the full
   block already provides ordered logs. The sorter is redundant in this case.

4. **Should the sorter live in SubscriptionManager or as a separate wrapper?**
   Probably inside SubscriptionManager — it needs access to the event stream
   before handler dispatch. Could also be a standalone
   `LogReorderingSubscription` wrapper for more flexibility.

## Conclusion

**Feasible — and proven.** A PoC implementation (`scripts/investigate_log_sorter_poc.py`)
was tested against a live mainnet node. Results:

- **0 inversions** after sorting across all blocks (vs 821-11,827 inversions in raw arrival)
- The `LogIndexSorter` buffers events per-block and flushes when the next block's
  first event arrives
- All events are delivered to handlers in on-chain logIndex order
- The latency is one block (12s) without a newHeads subscription, or ~100-200ms with
  newHeads as an early flush signal

The node sends all events for each subscription contiguously, with within-subscription
logIndex ordering preserved. Cross-subscription reordering is fixed by buffering events
per-block and sorting by logIndex before dispatch. The main tradeoff is latency (one
block of buffering without newHeads, ~100-200ms with newHeads as a flush signal).

### Implementation notes

The sorter should NOT be the default behavior — it introduces latency that may not be
acceptable for all use cases. Recommended approach:
- `sort_by_log_index: bool = False` parameter on `SubscriptionManager.handle_subscriptions()`
- Or a standalone `LogReorderingHandler` that wraps user handlers
- The sorter only applies to log events (which have `logIndex`); non-log events
  (newHeads, pending transactions) pass through unbuffered
- Deduplication by `(transactionHash, logIndex)` is possible when overlapping
  subscriptions produce duplicate events
