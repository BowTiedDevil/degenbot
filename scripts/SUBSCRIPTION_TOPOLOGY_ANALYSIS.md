# Event-Driven Subscription Architecture: Tradeoff Analysis (Superseded)

> **Superseded by `EVENT_DRIVEN_ARCHITECTURE.md`** which resolves the
> tradeoff by using an unfiltered log subscription. This document is
> retained for the empirical data and comparison tables.

## The Core Tradeoff

There are two competing models for subscription topology, each with different
latency and flexibility characteristics:

### Model A: Separate subscriptions per topic (current approach)
```
Bot → adapter.subscribe_logs(topics=[[V3_SWAP]])
Bot → adapter.subscribe_logs(topics=[[V2_SYNC]])
Bot → adapter.subscribe_logs(topics=[[TRANSFER]])
```

**Pros:**
- Each subscription maps to one handler — natural dispatch
- Adding/removing a topic = add/remove a subscription (0.4ms roundtrip)
- No deduplication needed — each event appears in exactly one subscription

**Cons:**
- Events arrive in subscription-batches (not logIndex order)
- 3-5% of event pairs are inverted relative to on-chain order
- Requires a LogIndexSorter middleware to fix ordering (+latency)
- Each subscription is a separate WS frame → more network overhead

### Model B: Combined subscription with topic dispatch
```
Bot → adapter.subscribe_logs(topics=[[V3_SWAP, V2_SYNC, TRANSFER]])
```

**Pros:**
- Events arrive in logIndex order (0 inversions, proven empirically)
- No sorter needed — the node guarantees ordering within a single subscription
- Lower network overhead (1 WS frame vs 3)
- Simpler pipeline: pump → process → dispatch

**Cons:**
- Must dispatch by inspecting the event's topic0 (topic → handler mapping)
- Changing the topic filter requires unsubscribe + re-subscribe (0.4ms, but
  events during the 0.4ms gap are lost)
- Adding a topic = unsubscribe old + subscribe new with expanded filter
- Removing a topic = same unsubscribe + re-subscribe with narrower filter
- Overlapping combined subscriptions produce duplicates

### Model C: Unfiltered subscription (all logs)
```
Bot → adapter.subscribe_logs()  # no filter
```

**Pros:**
- All logs in perfect logIndex order
- No filter changes ever needed — just route by topic0
- Maximum flexibility: any handler can register for any topic

**Cons:**
- ~1400 events per block on mainnet (vs 200-1000 with targeted filters)
- Higher processing overhead for discarding unwanted events
- Wastes bandwidth if most events are irrelevant
- Privacy/UX: some users may not want all logs

## The Dynamic Subscriptions Problem

The event-driven Bot wants: "a pool registers for Swap events on its address,
and the system sets up the right subscription." But what happens when:

1. Pool A registers for V3 Swap at address 0xAAA
2. Pool B registers for V3 Swap at address 0xBBB
3. Pool C registers for V2 Sync at address 0xCCC

With separate subscriptions:
- 3 subscriptions (one per registration)
- Each new registration = new subscription (0.4ms)
- Each removal = unsubscribe (0.1ms)
- Simple but out-of-order events across subscriptions

With combined subscription:
- Need `subscribe_logs(topics=[[V3_SWAP, V2_SYNC]], addresses=["0xAAA", "0xBBB", "0xCCC"])`
- Adding address 0xDDD = unsubscribe + re-subscribe with updated addresses
- During the 0.4ms gap, events for ANY address are lost (not just 0xDDD's events)
- Changing topics OR addresses both require unsubscribe + re-subscribe

**This is the real problem**: with combined subscriptions, adding a new address
or topic requires a brief unsubscribe that disrupts ALL currently-tracked addresses.

## Empirical Latency Data

| Metric | Separate | Combined |
|--------|----------|----------|
| Inversions | 3-5% of pairs | 0% |
| Block spread (first→last event) | 1.6-10.7ms | 0.3-13.6ms |
| Transfer arrival (from block start) | 0.2-1.1ms | 0.0ms (first event is often Transfer) |
| Subscription change cost | 0.4ms (one subscription) | 0.4ms + gap for ALL addresses |
| V3-first → Transfer-first | +0.4ms median | -0.3ms (Transfers arrive first) |

## Hybrid Approach: Primary + Supplemental

The insight: **most of the time, the subscription configuration is stable.**
Pools register at startup, and the set changes infrequently. The re-subscription
cost only matters during the brief window when the configuration changes.

```
Primary subscription:  topics=[[V3_SWAP, V2_SYNC, TRANSFER]], addresses=[all known]
  → Delivers events in logIndex order for all tracked addresses

Supplemental subscription: topics=[[V3_SWAP]], addresses=[new_address]
  → Temporary subscription for newly-added addresses during the gap
  → Removed once the primary subscription is updated
```

But this reintroduces the cross-subscription ordering problem during the gap!

## The Real Architecture: Composition

The resolution doesn't require choosing one model. Instead:

### Layer 1: Single combined subscription (for ordering)
The SubscriptionManager creates ONE `subscribe_logs` call with all topics
and addresses that any handler cares about. Events arrive in logIndex order.

### Layer 2: Topic-based dispatch (for routing)
Each event's `topics[0]` determines which handler(s) to invoke. The dispatch
table is updated dynamically when pools register/unregister.

### Layer 3: Subscription update on address changes (for filtering)
When a new address is added:
1. Create a *supplemental* subscription for just the new address
2. Mark the primary subscription as "stale"
3. At the next block boundary (detected via newHeads), unsubscribe primary
   and re-subscribe with the expanded address list
4. During the 0.4ms gap, the supplemental subscription covers the new address
5. After re-subscribe, unsubscribe the supplemental subscription

This gives:
- **In-order events** from the primary subscription (no sorter needed)
- **No gap** for the new address (supplemental covers it)
- **Cross-subscription ordering** during the gap doesn't matter because
  the supplemental only covers the new address — there are no overlapping
  events between primary and supplemental for the same transaction

Wait — that last point needs verification. If the primary subscription covers
address 0xAAA and the supplemental covers 0xBBB, do we still have cross-sub
ordering issues? Yes, but only for events involving 0xBBB during the gap.
And since 0xBBB was just added (no prior handler state), there's nothing to
be "out of order" relative to.

### The edge case: topic changes

If a new handler wants a NEW topic (not just a new address for an existing topic),
the primary subscription must be expanded. Same approach:
1. Add a supplemental for just the new topic
2. Rebuild primary at block boundary
3. The supplemental's events for the current block are in-order within themselves
4. After the primary is rebuilt, unsubscribe the supplemental

### When not to combine: cost-benefit

The combined approach only makes sense when you care about cross-subscription
ordering. If you have:
- A V3 Swap handler that only cares about V3 Swap events
- A Transfer handler that only cares about Transfer events
- No handler needs to see events in a specific cross-topic order

Then separate subscriptions are simpler and equally correct (within-sub
ordering is always guaranteed).

**The LogIndexSorter is only needed when:**
1. You have multiple subscriptions AND
2. You care about cross-subscription event ordering AND
3. You can't or won't use a combined subscription

## Recommendation

### For the event-driven Bot architecture:

1. **Default: combined subscription with topic-dispatch** — a single
   `subscribe_logs(topics=[all_topics], addresses=[all_addresses])` gives
   perfect logIndex ordering. Route by `topics[0]` → handler.

2. **Dynamic updates via block-boundary re-subscribe** — when addresses/topics
   change, defer the re-subscription to the next newHeads event. The brief
   gap (<0.5ms) is insignificant relative to block time (12s).

3. **Supplemental subscription during gap** — optional; only needed if
   losing 1-2 events during re-subscribe is unacceptable. For most
   MEV/bot use cases, a single missed event doesn't break state (the
   next block's events will reflect the current state).

4. **EventMiddleware protocol as extensibility hook** — for users who
   want separate subscriptions + sorting, or deduplication, or other
   custom dispatch logic. The pipeline is:
   ```
   primary subscription → middleware.ingest() → handler dispatch
   ```
   The LogIndexSorter is one implementation of this protocol.

### Why this resolves the tradeoff:

- **No latency from sorting** — combined subscription gives in-order events
  from the node. No buffering needed.
- **Dynamic address changes** — block-boundary re-subscribe with 0.4ms gap.
  For 12s block times, this is a 0.003% disruption window.
- **Flexibility** — the EventMiddleware protocol allows users who prefer
  separate subscriptions (e.g., for simpler per-topic handler code) to
  add sorting if they need it.
- **The Bot doesn't take responsibility for sorting** — it just picks the
  combined subscription topology. Sorting is a middleware for the separate
  subscription topology.

## Quantified tradeoff summary

| Topology | In-order? | Dynamic add? | Add disruption | Complexity |
|----------|-----------|-------------|----------------|------------|
| Separate (per-topic) | ✗ (3-5% inversions) | ✓ (0.4ms, 1 sub) | None | Low |
| Separate + Sorter | ✓ (+12s latency or +100-200ms with heads) | ✓ (0.4ms) | None | Medium |
| Combined (all topics+addr) | ✓ | ✗ (re-sub 0.4ms) | Miss events in gap | Low |
| Combined + Supplemental | ✓ | ✓ (gap covered) | None | Medium |
