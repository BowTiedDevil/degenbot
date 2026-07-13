#!/usr/bin/env python3
"""Python repro: web3.py WS provider behavior on oversized ``eth_getLogs``.

Mirrors the alloy+tungstenite test ``ws_getlogs_large_filter_diagnostic``
(in ``rust/crates/degenbot-bot/src/bot_core/block_pump.rs``) to verify
empirically how Python's ``web3.WebSocketProvider`` + ``websockets`` stack
reacts when the same ~90 MB ``eth_getLogs`` response that *hangs* the Rust
pubsub WS provider arrives over a Python WS provider.

Confirmed empirically (2026-07-12):

1. ``websockets``' default ``max_size = 1048576`` (1 MiB) DOES trip when
   the oversized response arrives.
2. The failure is LOUD — surfaces as a
   ``websockets.exceptions.ConnectionClosedError: sent 1009 (message too
   big); no close frame received`` on the awaiting coroutine (within ~21s —
   most of which is server-side compute time for the 99k-log JSON-RPC
   response, not cap-check time).
3. The probe path mirrors alloy's behavior in *one* respect — the WS
   connection is dead after the failure, so subsequent ``eth_blockNumber``
   calls ALSO fail with the same ``ConnectionClosedError``. But this is
   NOT a silent retry loop: each failing call returns the immediately
   visible exception to its caller, not an indefinitely pending future.
4. There is, however, NO auto-reconnect — the connection stays dead and
   every subsequent request fails until the caller makes a fresh
   ``connect()`` call. (Different from alloy: alloy auto-reconnects and
   re-dispatches in-flight requests; web3.py stays dead and surfaces the
   error to each caller.)

USAGE::

    DEGENBOT_RPC_WS_CHAINID_1=wss://mainnet.example.com \\
        uv run python3 examples/py_ws_getlogs_diagnostic.py
"""

from __future__ import annotations

import asyncio
import os
import sys
import time

from web3 import AsyncWeb3, WebSocketProvider

# ── Filter ────────────────────────────────────────────────────────────────
# Same 6-topic OR over ``topic0`` as ``build_backfill_filter`` in
# ``rust/crates/degenbot-bot/src/bot_core/block_pump.rs``. A ~2000-block
# mainnet range covers enough V2/V3/V4 activity to produce a ~90 MB response,
# the same oversized payload that hung the Rust WS pubsub provider.
SWAP_TOPICS_0 = [
    "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1",  # V2  Sync
    "0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde",  # V3  Mint
    "0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c",  # V3  Burn
    "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67",  # V3  Swap
    "0x40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f",  # V4  Swap
    "0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec",  # V4  ModifyLiquidity
]

# Mirrors ``DEFAULT_BACKFILL_CHUNK_SIZE`` (2000 in block_pump.rs). A 2000-block
# mainnet range is enough to produce the ~90 MB response that trips the cap.
DEFAULT_RANGE_BLOCKS = 2000

# Should exceed the time the oversized response takes to either arrive or
# raise. The Rust diagnostic hung FOREVER; here we cap at 60s so a failure
# mode that LOOKS like a silent hang (instead of the expected exception)
# also produces a loud failure for inspection.
BIG_GETLOGS_TIMEOUT_SECS = 60

PROBE_INTERVAL_SECS = 2.0


async def probe_block_number(w3: AsyncWeb3, label: str, stop: asyncio.Event) -> None:
    """Poll ``eth_blockNumber`` to prove transport is alive (or fails loudly)
    while the oversized ``eth_getLogs`` is in flight."""
    counter = 0
    while not stop.is_set():
        try:
            t0 = time.perf_counter()
            bn = await asyncio.wait_for(w3.eth.block_number, timeout=15)
            dt = time.perf_counter() - t0
            counter += 1
            print(
                f"  probe ({label}) #{counter}: eth_blockNumber OK = {bn}"
                f" in {dt * 1000:.0f}ms"
            )
        except Exception as e:  # noqa: BLE001 — print every failure class
            counter += 1
            print(
                f"  probe ({label}) #{counter}: eth_blockNumber FAILED: "
                f"{type(e).__module__}.{type(e).__name__}: {e}"
            )
        try:
            await asyncio.wait_for(stop.wait(), timeout=PROBE_INTERVAL_SECS)
        except asyncio.TimeoutError:
            pass


async def run_big_getlogs(w3: AsyncWeb3, from_block: int, to_block: int) -> tuple[str, dict]:
    """Issue the oversized 6-topic ``eth_getLogs``; return result label + traits."""
    # web3.py's raw ``provider.make_request`` does NOT auto-hex the block
    # numbers the way ``w3.eth.get_logs`` does — pass hex strings manually.
    log_filter = {
        "fromBlock": hex(from_block),
        "toBlock": hex(to_block),
        "topics": [SWAP_TOPICS_0],  # topic0 OR over the 6 swap topics
    }
    t0 = time.perf_counter()
    try:
        # Use ``provider.make_request`` to get the raw response — we can
        # measure the actual response size in bytes (not just the decoded
        # log count).
        raw = await asyncio.wait_for(
            w3.provider.make_request("eth_getLogs", [log_filter]),
            timeout=BIG_GETLOGS_TIMEOUT_SECS,
        )
        dt = time.perf_counter() - t0
        result_list = raw.get("result") if isinstance(raw, dict) else None
        n_logs = len(result_list) if isinstance(result_list, list) else "?"
        return "OK", {"dt_ms": dt * 1000, "n_logs": n_logs, "raw": raw}
    except asyncio.TimeoutError:
        dt = time.perf_counter() - t0
        return "HUNG", {"dt_ms": dt * 1000}
    except Exception as e:
        dt = time.perf_counter() - t0
        return type(e).__name__, {
            "dt_ms": dt * 1000,
            "exc_module": type(e).__module__,
            "exc_name": type(e).__name__,
            "exc_str": str(e),
        }


async def main() -> int:
    ws_url = os.environ.get("DEGENBOT_RPC_WS_CHAINID_1")
    if not ws_url:
        print(
            "ERROR: set DEGENBOT_RPC_WS_CHAINID_1=ws://... to run this diagnostic",
            file=sys.stderr,
        )
        return 1

    print("== websockets oversized getLogs diagnostic (Python repro) ==")
    print(f"ws_url     : {ws_url}")
    print("websockets : max_size default = 1 MiB (1048576 bytes)")
    print("filter     : 6-topic OR over topic0 (V2 Sync, V3 Mint/Burn/Swap,")
    print("             V4 Swap/ModifyLiquidity)")
    print(f"timeout    : {BIG_GETLOGS_TIMEOUT_SECS}s")

    # Use the async ``WebSocketProvider`` (PersistentConnectionProvider variant)
    # — the modern counterpart to the legacy ``LegacyWebSocketProvider`` the
    # degenbot factory uses. Both share the same underlying ``websockets``
    # library and the same 1 MiB default cap, so the failure mode is identical.
    # ``WebSocketProvider`` does NOT implement the ``async with`` protocol —
    # use explicit ``connect`` / ``disconnect``.
    provider = WebSocketProvider(ws_url)
    await provider.connect()
    try:
        w3 = AsyncWeb3(provider)

        # ── [1] Confirm the WS endpoint is reachable ─────────────────────
        try:
            bn = await asyncio.wait_for(w3.eth.block_number, timeout=15)
            print(f"\n[1] eth_blockNumber OK = {bn}  (WS handshake succeeded)")
        except Exception as e:
            print(
                f"\n[1] eth_blockNumber FAILED: "
                f"{type(e).__module__}.{type(e).__name__}: {e}"
            )
            return 1

        from_block = max(bn - DEFAULT_RANGE_BLOCKS + 1, 0)
        to_block = bn
        n_blocks = to_block - from_block + 1
        print(
            f"\n[2] eth_getLogs(from {from_block}..{to_block}, "
            f"{n_blocks} blocks, 6-topic OR)"
        )
        print(
            "    expected: ~90 MB response trips the 1 MiB cap → exception"
            " visible to caller"
        )
        print(
            "    surprise-if-true: silent hang (60s timeout) like the Rust"
            " pubsub path"
        )

        # ── [2] Oversized getLogs, with concurrent blockNumber probe ─────
        stop_probe = asyncio.Event()
        probe_task = asyncio.create_task(
            probe_block_number(w3, "during", stop_probe)
        )
        # Give the probe a head start so we see at least one OK before the big
        # call returns/fails.
        await asyncio.sleep(0.1)

        outcome, traits = await run_big_getlogs(w3, from_block, to_block)

        stop_probe.set()
        try:
            await asyncio.wait_for(probe_task, timeout=5.0)
        except asyncio.TimeoutError:
            probe_task.cancel()

        print(f"\n[2] RESULT: {outcome}")
        dt_ms = traits.get("dt_ms", 0)
        if outcome == "OK":
            n_logs = traits["n_logs"]
            raw = traits["raw"]
            # crude byte-size estimate of the decoded response
            approx_bytes = len(str(raw))
            print(
                f"    → ⚠️  get_logs SUCCEEDED in {dt_ms:.0f}ms ({n_logs} logs,"
                f" ~{approx_bytes:,} bytes response)"
            )
            print(
                "    → websockets did NOT trip the cap — investigate whether"
                " the server fragmented, or size grew smaller than expected"
            )
        elif outcome == "HUNG":
            print(
                f"    → get_logs HUNG past {BIG_GETLOGS_TIMEOUT_SECS}s"
                f" (was at {dt_ms:.0f}ms when timed out)"
            )
            print(
                "    → ⚠️  Python reproduced the silent-hang behavior — the"
                " web3.py stack DOES have a silent retry somewhere"
            )
        else:
            print(f"    → get_logs FAILED loudly in {dt_ms:.0f}ms")
            print(
                f"    error class: {traits['exc_module']}.{traits['exc_name']}"
            )
            print(f"    message   : {traits['exc_str']}")
            print(
                "    → this is the expected behavior — exception bubbles up"
                " to the caller, no silent pubsub retry loop"
            )

        # ── [3] Post-failure connectivity probe ──────────────────────────
        # Did the WS connection die with the big call, or survive it?
        try:
            bn2 = await asyncio.wait_for(w3.eth.block_number, timeout=15)
            print(
                f"\n[3] post-failure eth_blockNumber OK = {bn2} "
                "→ WS connection survived"
            )
        except Exception as e:
            print(
                "\n[3] post-failure eth_blockNumber FAILED → WS connection died"
                " with the big call"
            )
            print(
                f"    error class: {type(e).__module__}.{type(e).__name__}: {e}"
            )

        # ── [4] Post-failure tiny getLogs ────────────────────────────────
        # Does a small follow-up request succeed on the (possibly) recovered
        # connection?
        try:
            small_filter = {
                "fromBlock": hex(bn),
                "toBlock": hex(bn),
                "topics": [SWAP_TOPICS_0],
            }
            t0 = time.perf_counter()
            small_logs = await asyncio.wait_for(
                w3.provider.make_request("eth_getLogs", [small_filter]),
                timeout=30,
            )
            dt = time.perf_counter() - t0
            result = small_logs.get("result") if isinstance(small_logs, dict) else None
            n_small = len(result) if isinstance(result, list) else "?"
            print(
                f"\n[4] post-failure eth_getLogs(1 block) OK = {n_small} logs"
                f" in {dt:.2f}s → small calls recover on the same connection"
            )
        except Exception as e:
            print(
                "\n[4] post-failure eth_getLogs(1 block) FAILED → small calls"
                " also broken"
            )
            print(
                f"    error class: {type(e).__module__}.{type(e).__name__}: {e}"
            )
    finally:
        try:
            await provider.disconnect()
        except Exception as e:  # noqa: BLE001 — never fail just on teardown
            print(f"  (disconnect: {type(e).__name__}: {e})")

    print("\n== done ==")
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))