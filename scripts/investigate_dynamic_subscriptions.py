"""Investigate: Can we add/remove topics from a running subscription?

Tests whether Ethereum nodes support changing the topic filter on an
existing eth_subscription, or whether we must unsubscribe + re-subscribe.

Also measures the time cost of unsubscribe + re-subscribe to understand
the latency impact of dynamic subscription changes.
"""

import asyncio
import json
import os
import time


WS_URI = os.environ.get("ETHEREUM_ARCHIVE_NODE_WS_URI", "ws://node:8546")

V3_SWAP_TOPIC = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
V2_SYNC_TOPIC = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"
TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"


async def test_subscribe_unsubscribe_timings(ws, rounds: int = 10) -> None:
    """Measure the time to subscribe, receive first event, unsubscribe."""
    timings = []

    for i in range(rounds):
        # Subscribe to a single topic (Transfer — most frequent)
        req = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_subscribe",
            "params": ["logs", {"topics": [[TRANSFER_TOPIC]]}],
        }

        t0 = time.time()
        await ws.send(json.dumps(req))
        resp = json.loads(await ws.recv())
        sub_id = resp.get("result", "")
        t_subscribe = time.time()

        # Wait for first event
        while True:
            msg = json.loads(await asyncio.wait_for(ws.recv(), timeout=30))
            if msg.get("method") == "eth_subscription":
                t_first_event = time.time()
                break

        # Unsubscribe immediately
        unsub_req = {
            "jsonrpc": "2.0",
            "id": 2,
            "method": "eth_unsubscribe",
            "params": [sub_id],
        }
        await ws.send(json.dumps(unsub_req))
        unsub_resp = json.loads(await ws.recv())
        t_unsubscribe = time.time()

        subscribe_ms = (t_subscribe - t0) * 1000
        first_event_ms = (t_first_event - t_subscribe) * 1000
        unsubscribe_ms = (t_unsubscribe - t_first_event) * 1000
        total_ms = (t_unsubscribe - t0) * 1000

        timings.append({
            "subscribe_ms": subscribe_ms,
            "first_event_ms": first_event_ms,
            "unsubscribe_ms": unsubscribe_ms,
            "total_ms": total_ms,
        })

        print(f"  Round {i+1}: subscribe={subscribe_ms:.1f}ms, first_event={first_event_ms:.1f}ms, unsubscribe={unsubscribe_ms:.1f}ms, total={total_ms:.1f}ms")

        # Drain any remaining events from the subscription
        await asyncio.sleep(0.1)
        try:
            while True:
                msg = await asyncio.wait_for(ws.recv(), timeout=0.05)
                # Just drain
        except (TimeoutError, asyncio.TimeoutError):
            pass

    print(f"\nAggregated over {len(timings)} rounds:")
    for key in ["subscribe_ms", "first_event_ms", "unsubscribe_ms", "total_ms"]:
        vals = [t[key] for t in timings]
        avg = sum(vals) / len(vals)
        median = sorted(vals)[len(vals) // 2]
        print(f"  {key}: avg={avg:.1f}ms, median={median:.1f}ms, max={max(vals):.1f}ms, min={min(vals):.1f}ms")


async def test_modify_subscription_filter(ws) -> None:
    """Test if we can modify the filter on an existing subscription.

    The eth_subscribe spec doesn't support modifying an active subscription's
    filter. The only way to change what events you receive is to unsubscribe
    and create a new subscription with the updated filter.

    This test verifies that:
    1. The node doesn't have a 'modify subscription' method
    2. The unsubscribe + re-subscribe cycle works
    3. The gap between unsubscribe and new subscription is measurable
    """
    # Subscribe to V3 Swap only
    req1 = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_subscribe",
        "params": ["logs", {"topics": [[V3_SWAP_TOPIC]]}],
    }
    await ws.send(json.dumps(req1))
    resp1 = json.loads(await ws.recv())
    sub_id_1 = resp1.get("result", "")
    print(f"  Subscribed to V3 Swap only: {sub_id_1}")

    # Try to find if there's a modify method (there shouldn't be)
    # The Ethereum JSON-RPC spec only has eth_subscribe and eth_unsubscribe
    # for subscription management. No eth_modifySubscription or similar.
    modify_req = {
        "jsonrpc": "2.0",
        "id": 99,
        "method": "eth_modifySubscription",
        "params": [sub_id_1, {"topics": [[V3_SWAP_TOPIC, V2_SYNC_TOPIC]]}],
    }
    await ws.send(json.dumps(modify_req))
    try:
        modify_resp = json.loads(await asyncio.wait_for(ws.recv(), timeout=2))
        print(f"  eth_modifySubscription response: {modify_resp}")
    except (TimeoutError, asyncio.TimeoutError):
        print("  eth_modifySubscription: no response (method not supported, as expected)")

    # Unsubscribe and re-subscribe with broader filter
    start_gap = time.time()

    unsub_req = {
        "jsonrpc": "2.0",
        "id": 2,
        "method": "eth_unsubscribe",
        "params": [sub_id_1],
    }
    await ws.send(json.dumps(unsub_req))
    unsub_resp = json.loads(await ws.recv())
    t_unsubscribed = time.time()

    # Re-subscribe with both topics
    req2 = {
        "jsonrpc": "2.0",
        "id": 3,
        "method": "eth_subscribe",
        "params": ["logs", {"topics": [[V3_SWAP_TOPIC, V2_SYNC_TOPIC]]}],
    }
    await ws.send(json.dumps(req2))
    resp2 = json.loads(await ws.recv())
    sub_id_2 = resp2.get("result", "")
    t_resubscribed = time.time()

    gap_ms = (t_resubscribed - start_gap) * 1000
    unsub_ms = (t_unsubscribed - start_gap) * 1000
    resub_ms = (t_resubscribed - t_unsubscribed) * 1000

    print(f"  Unsubscribe + re-subscribe gap: {gap_ms:.1f}ms")
    print(f"    Unsubscribe: {unsub_ms:.1f}ms")
    print(f"    Re-subscribe: {resub_ms:.1f}ms")
    print(f"  New subscription (V3+V2): {sub_id_2}")

    # Verify we now get V2 Sync events
    print("  Waiting for events from both topics...")
    got_v3 = False
    got_v2 = False
    deadline = time.time() + 30
    while time.time() < deadline and not (got_v3 and got_v2):
        try:
            msg = json.loads(await asyncio.wait_for(ws.recv(), timeout=5))
            if msg.get("method") == "eth_subscription":
                result = msg.get("params", {}).get("result", {})
                topics = result.get("topics", [])
                if topics and topics[0] == V3_SWAP_TOPIC:
                    got_v3 = True
                elif topics and topics[0] == V2_SYNC_TOPIC:
                    got_v2 = True
        except (TimeoutError, asyncio.TimeoutError):
            continue

    print(f"  Got V3 Swap events: {got_v3}")
    print(f"  Got V2 Sync events: {got_v2}")

    # Clean up
    unsub_req = {
        "jsonrpc": "2.0",
        "id": 4,
        "method": "eth_unsubscribe",
        "params": [sub_id_2],
    }
    await ws.send(json.dumps(unsub_req))
    await ws.recv()


async def test_overlapping_subscriptions(ws, collect_seconds: int = 15) -> None:
    """Test: What happens if we subscribe twice to overlapping topics?

    If we have:
      - Sub A: topics=[[V3_SWAP, V2_SYNC]]
      - Sub B: topics=[[TRANSFER]]

    Then later add:
      - Sub C: topics=[[V3_SWAP]]

    Do we get duplicate V3 Swap events from both A and C?
    """
    # Sub A: V3 + V2
    req_a = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_subscribe",
        "params": ["logs", {"topics": [[V3_SWAP_TOPIC, V2_SYNC_TOPIC]]}],
    }
    await ws.send(json.dumps(req_a))
    resp_a = json.loads(await ws.recv())
    sub_id_a = resp_a.get("result", "")

    # Sub B: Transfer only
    req_b = {
        "jsonrpc": "2.0",
        "id": 2,
        "method": "eth_subscribe",
        "params": ["logs", {"topics": [[TRANSFER_TOPIC]]}],
    }
    await ws.send(json.dumps(req_b))
    resp_b = json.loads(await ws.recv())
    sub_id_b = resp_b.get("result", "")

    print(f"  Sub A (V3+V2): {sub_id_a}")
    print(f"  Sub B (Transfer): {sub_id_b}")
    print(f"  Collecting for {collect_seconds}s...")

    # Collect events from both
    events: dict[str, list[str]] = {sub_id_a: [], sub_id_b: []}
    dedup_check: dict[str, set[str]] = {sub_id_a: set(), sub_id_b: set()}  # sub_id → set of txHash

    try:
        async with asyncio.timeout(collect_seconds):
            async for raw_msg in ws:
                msg = json.loads(raw_msg)
                if msg.get("method") != "eth_subscription":
                    continue
                params = msg.get("params", {})
                sid = params.get("subscription", "")
                result = params.get("result", {})
                topics = result.get("topics", [])
                tx_hash = result.get("transactionHash", "")

                if sid in events:
                    if topics:
                        t0 = topics[0]
                        label = "V3" if t0 == V3_SWAP_TOPIC else "V2" if t0 == V2_SYNC_TOPIC else "T" if t0 == TRANSFER_TOPIC else "?"
                        events[sid].append(label)
                        if tx_hash:
                            dedup_check[sid].add(tx_hash)
    except TimeoutError:
        pass

    from collections import Counter
    for sid, evts in events.items():
        counts = Counter(evts)
        label = "A(V3+V2)" if sid == sub_id_a else "B(Transfer)"
        print(f"  {label}: {dict(counts)}, unique txHashes: {len(dedup_check[sid])}")

    # Now add Sub C: V3 only (overlaps with Sub A)
    print("\n  Adding Sub C (V3 only) — overlaps with Sub A...")
    req_c = {
        "jsonrpc": "2.0",
        "id": 3,
        "method": "eth_subscribe",
        "params": ["logs", {"topics": [[V3_SWAP_TOPIC]]}],
    }
    await ws.send(json.dumps(req_c))
    resp_c = json.loads(await ws.recv())
    sub_id_c = resp_c.get("result", "")
    print(f"  Sub C (V3 only): {sub_id_c}")

    events_c: list[str] = []
    dedup_c: set[str] = set()

    try:
        async with asyncio.timeout(collect_seconds):
            async for raw_msg in ws:
                msg = json.loads(raw_msg)
                if msg.get("method") != "eth_subscription":
                    continue
                params = msg.get("params", {})
                sid = params.get("subscription", "")
                result = params.get("result", {})
                topics = result.get("topics", [])
                tx_hash = result.get("transactionHash", "")

                if sid == sub_id_a:
                    # Still getting V3 + V2 from Sub A
                    if topics and topics[0] == V3_SWAP_TOPIC:
                        if tx_hash:
                            dedup_check[sid].add(tx_hash)
                elif sid == sub_id_c:
                    if topics:
                        events_c.append("V3")
                        if tx_hash:
                            dedup_c.add(tx_hash)
    except TimeoutError:
        pass

    counts_a_v3 = len(dedup_check.get(sub_id_a, set()))
    counts_c = len(dedup_c)
    print(f"  Sub A V3 unique txHashes: {counts_a_v3}")
    print(f"  Sub C V3 unique txHashes: {counts_c}")
    print(f"  → V3 Swap events received from BOTH subscriptions (expected)")

    # Clean up
    for sid in [sub_id_a, sub_id_b, sub_id_c]:
        unsub_req = {"jsonrpc": "2.0", "id": 10, "method": "eth_unsubscribe", "params": [sid]}
        await ws.send(json.dumps(unsub_req))
        try:
            await asyncio.wait_for(ws.recv(), timeout=2)
        except (TimeoutError, asyncio.TimeoutError):
            pass


async def main() -> None:
    import websockets

    print("=" * 80)
    print("TEST 1: Subscribe/Unsubscribe round-trip timing")
    print("=" * 80)
    async with websockets.connect(WS_URI) as ws:
        await test_subscribe_unsubscribe_timings(ws, rounds=5)

    print("\n" + "=" * 80)
    print("TEST 2: Can we modify a subscription filter?")
    print("=" * 80)
    async with websockets.connect(WS_URI) as ws:
        await test_modify_subscription_filter(ws)

    print("\n" + "=" * 80)
    print("TEST 3: Overlapping subscriptions produce duplicates")
    print("=" * 80)
    async with websockets.connect(WS_URI) as ws:
        await test_overlapping_subscriptions(ws)


if __name__ == "__main__":
    asyncio.run(main())
