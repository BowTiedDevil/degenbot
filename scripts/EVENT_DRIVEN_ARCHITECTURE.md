# Event-Driven Subscription Architecture

## Decision: Unfiltered Log Subscription + Internal Dispatch

The listener subscribes to **all logs with no filter**, then does
topic/address filtration internally before publishing events to the
message bus.

### Why this works

The Ethereum node delivers events from a single `eth_subscribe` call
in **strict logIndex order** within each block. This eliminates the
entire cross-subscription ordering problem that plagues
per-topic subscriptions.

### Empirical basis

| Metric | Value | Source |
|--------|-------|--------|
| Inversions (unfiltered) | **0** | `investigate_unfiltered_volume.py` |
| Inversions (3 separate subs) | 3-5% of pairs | `investigate_raw_node_ordering.py` |
| Inversions (1 combined, 3 topics) | **0** | `investigate_combined_subscription.py` |
| Events per block (mainnet, all logs) | ~800 (range 388-1252) | `investigate_unfiltered_volume.py` |
| Block spread (first→last event) | 8.6ms avg, 13.2ms max | Same |
| Dispatch lookup cost | 376ns/event (set membership) | `investigate_unfiltered_dispatch_cost.py` |
| Subscribe/unsubscribe RTT | 0.4ms | `investigate_dynamic_subscriptions.py` |

### Why not separate or filtered subscriptions

**Separate per-topic subscriptions** — the node batches events by
subscription. V3 Swap events arrive as a burst, then V2 Sync, then
Transfers. Even though each subscription is internally ordered, the
cross-subscription interleaving is wrong. A LogIndexSorter middleware
can fix this but introduces 12s latency (or ~100-200ms with newHeads
flush).

**Combined subscription with `topics` filter** — impossible to express
"V3 Swap at address X OR V2 Sync at address Y" in a single subscription.
The `eth_subscribe` logs filter applies `address` AND `topics` as
conjunction — an event must match BOTH the address AND the topic filter.
There is no way to express per-address topic sets.

**Combined subscription with `topics` only (no address filter)** — this
arrives in logIndex order, but you get ALL Transfer events on mainnet
(76% of all events). Still workable — the dispatch just discards
unmatched addresses. But the unfiltered subscription is even simpler
and only adds 20% more events (the "Other" category includes Approval,
Mint, Burn, etc. which you might actually want).

### Architecture

```
                         ┌─────────────────────────┐
                         │  Unfiltered Log Listener │
                         │  (eth_subscribe "logs") │
                         └────────┬────────────────┘
                                  │ events in logIndex order
                                  ▼
                         ┌─────────────────────────┐
                         │  Internal Dispatch       │
                         │  topic0 → handler set    │
                         │  address → handler set   │
                         └────────┬────────────────┘
                                  │ filtered events
                                  ▼
                         ┌─────────────────────────┐
                         │  Message Bus              │
                         │  (per-topic channels)     │
                         └────────┬────────────────┘
                                  │
                    ┌─────────────┼─────────────┐
                    ▼             ▼             ▼
              Pool A handler  Pool B handler  Token handler
```

The listener:
1. Opens a single `eth_subscribe("logs", {})` — no address or topic filter
2. Receives events in strict logIndex order (guaranteed by the node)
3. For each event, looks up `topics[0]` + `address` in the dispatch table
4. Publishes matching events to the message bus
5. Discards events with no registered handler (set membership, ~376ns)

The dispatch table is populated by pool/token objects registering their
interest when they are created:

```python
# Pool registers for its own Swap events
listener.register(
    address="0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",
    topic=V3_SWAP_TOPIC,
    handler=pool.on_swap_event,
)

# Token registers for Transfer events involving its address
listener.register(
    address="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
    topic=TRANSFER_TOPIC,
    handler=token.on_transfer_event,
)
```

### Dynamic registration

When a pool is created mid-session:
1. Add its (address, topic0) → handler to the dispatch table
2. No subscription change needed — the unfiltered stream already
   includes all events

When a pool is destroyed:
1. Remove its entry from the dispatch table
2. No subscription change needed

**Zero disruption.** The "0.4ms unsubscribe+re-subscribe" problem is
eliminated entirely. Registration/deregistration is a dict mutation.

### The "Other" 20%

The unfiltered subscription delivers events that don't match any
registered address/topic. These are discarded in ~376ns each. At
~800 events/block and 20% discards, that's ~160 discards per block
at 376ns each = 60μs per block — completely negligible.

Alternatively, the "Other" events can be forwarded to a secondary
bus channel for users who want all logs (e.g., for mempool monitoring,
arbitrage path discovery, or indexers).

### Edge case: newHeads subscription

The unfiltered log subscription covers all log events. For block
headers (newHeads), a separate `eth_subscribe("newHeads")` is needed.
This is a single additional subscription. Since the newHeads stream
has no logIndex, there is no cross-subscription ordering concern —
the newHeads event for block N arrives before the logs for block N
(empirically confirmed).

The newHeads subscription serves as a "block boundary" signal:
when block N+1's head arrives, all of block N's logs have been
delivered. This is useful for:
- Flushing any buffered state
- Triggering on-block-boundary callbacks
- Liveness checks (heartbeat)

### Comparison with alternatives

| Approach | Ordering | Dynamic add | Disruption | Latency | Complexity |
|----------|----------|-------------|------------|----------|------------|
| Separate per-topic | ✗ (3-5% inversions) | ✓ (new sub) | None | +0ms | Low |
| Separate + Sorter | ✓ | ✓ (new sub) | None | +12s (or +100-200ms with heads) | Medium |
| Combined topics only | ✓ | ✗ (re-sub all) | Miss events in gap | +0ms | Low |
| **Unfiltered + dispatch** | **✓** | **✓ (dict mutation)** | **None** | **+0ms** | **Low** |

The unfiltered approach dominates every other option on every axis.

### When separate subscriptions are still useful

For specialized use cases where the user wants a targeted stream:
- "I only care about V3 Swap events at address X" — a filtered
  subscription is more bandwidth-efficient if running over a
  metered connection
- "I want pending transactions" — these aren't logs at all,
  they're a different subscription type
- "I'm on a bandwidth-constrained node" — the unfiltered stream
  is ~800 events × ~500 bytes = ~400KB per block

The unfiltered approach is the right default for the event-driven
Bot. Filtered subscriptions remain available as an escape hatch
for special cases.

## Implementation outline

```python
class LogListener:
    """Subscribes to all logs, dispatches to registered handlers."""

    def __init__(self, adapter: AsyncProviderAdapter) -> None:
        self._adapter = adapter
        self._dispatch: dict[tuple[str, str], list[Handler]] = {}
        # (address_lower, topic0) → [handler]

    def register(
        self,
        address: str,
        topic: str,
        handler: Callable[[dict], Awaitable[None]],
    ) -> None:
        key = (address.lower(), topic)
        self._dispatch.setdefault(key, []).append(handler)

    def unregister(self, address: str, topic: str) -> None:
        key = (address.lower(), topic)
        self._dispatch.pop(key, None)

    async def run(self) -> None:
        subscription = await self._adapter.subscribe_logs()
        async for event in subscription:
            address = event.get("address", "").lower()
            topics = event.get("topics", [])
            topic0 = topics[0] if topics else None
            if topic0 is None:
                continue

            # Dispatch by (address, topic0)
            key = (address, topic0)
            handlers = self._dispatch.get(key)
            if handlers is not None:
                for handler in handlers:
                    # Fire-and-forget; errors logged
                    asyncio.create_task(self._safe_call(handler, event))

            # Also dispatch by topic0 only (for handlers that match all addresses)
            key_topic = ("", topic0)
            topic_handlers = self._dispatch.get(key_topic)
            if topic_handlers is not None:
                for handler in topic_handlers:
                    asyncio.create_task(self._safe_call(handler, event))
```
