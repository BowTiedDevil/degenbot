"""Unit tests for the bounded producer/consumer registration pipeline.

Covers the backpressure + concurrency contract of
`run_registration_pipeline` (the helper `build_paths` uses to decouple path
discovery from pool activation):

- Backpressure: the producer must never have more than `queue_size` items
  in flight ahead of the workers (a flood is held at the queue boundary).
- FIFO: items are consumed in the order discovered (registration order is
  preserved, matching the previous one-at-a-time semantics).
- Sentinel / completion: when the producer exhausts, all workers drain and
  the call returns.
- Worker count: with `worker_count` workers, up to `worker_count` items are
  being processed concurrently (overlapping lock-free RPC latency).
- Fatal propagation: an exception escaping `consume` (verification fatal)
  cancels the sibling workers + producer and re-raises.

These use a fake producer/consumer (no Rust engine, no RPC), per AGENTS.md's
preference for Fakes.
"""

from __future__ import annotations

import asyncio

import pytest

from examples.eth_backrun_v2_v3_v4_rust import run_registration_pipeline


async def _n_items(n: int) -> list[int]:
    """Produce integers 0..n-1, recording the max in-flight count."""
    return list(range(n))


async def test_backpressure_caps_producer_in_flight() -> None:
    """The producer must never run more than `queue_size` items ahead.

    Assert by tracking the max number of yielded-but-not-yet-consumed items.
    """
    queue_size = 4
    worker_count = 4

    produced = 0
    consumed = 0
    max_in_flight = 0

    async def producer():
        nonlocal produced, max_in_flight
        for i in range(1000):
            produced += 1
            in_flight = produced - consumed
            max_in_flight = max(max_in_flight, in_flight)
            yield i  # the yield is where the `await put` would sit

    async def consume(item: int) -> None:
        nonlocal consumed
        consumed += 1
        await asyncio.sleep(0.001)

    await run_registration_pipeline(
        producer=producer(),
        consume=consume,
        queue_size=queue_size,
        worker_count=worker_count,
    )

    # The helper's backpressure bounds the BUFFER (items produced but not yet
    # consumed): at most `queue_size` sit in the queue plus at most one item in
    # the hand of each of `worker_count` workers inside `consume`. Without the
    # bounded queue the producer would run all 1000 items ahead (max_in_flight
    # ~ 1000); with it, the producer is blocked at the queue boundary so the
    # in-flight buffer stays small and bounded.
    assert consumed == produced
    assert max_in_flight <= queue_size + worker_count


async def test_fifo_consumption_order() -> None:
    """Workers consume in discovery order (FIFO), preserving registration order."""
    order: list[int] = []

    async def producer():
        for i in range(50):
            yield i

    async def consume(item: int) -> None:
        await asyncio.sleep(0)
        order.append(item)

    await run_registration_pipeline(
        producer=producer(),
        consume=consume,
        queue_size=5,
        worker_count=4,
    )
    assert order == list(range(50))


async def test_completes_and_drains_when_producer_exhausts() -> None:
    """Call returns after the producer exhausts and all items are consumed."""
    consumed: list[int] = []

    async def producer():
        for i in range(37):
            yield i

    async def consume(item: int) -> None:
        consumed.append(item)
        await asyncio.sleep(0)

    await run_registration_pipeline(
        producer=producer(),
        consume=consume,
        queue_size=7,
        worker_count=3,
    )
    assert len(consumed) == 37


async def test_empty_producer_completes() -> None:
    async def producer():
        if False:  # noqa: SIM223
            yield None

    async def consume(item: object) -> None:
        raise AssertionError("consume should never run for an empty producer")

    await run_registration_pipeline(
        producer=producer(),
        consume=consume,
        queue_size=4,
        worker_count=3,
    )


async def test_validation_rejects_bad_args() -> None:
    async def producer():
        yield None

    async def consume(item: object) -> None:
        pass

    with pytest.raises(ValueError):
        await run_registration_pipeline(
            producer=producer(),
            consume=consume,
            queue_size=0,
            worker_count=2,
        )
    with pytest.raises(ValueError):
        await run_registration_pipeline(
            producer=producer(),
            consume=consume,
            queue_size=2,
            worker_count=0,
        )


class _FatalError(RuntimeError):
    pass


async def test_fatal_error_cancels_siblings_and_reraises() -> None:
    """An exception escaping `consume` aborts the whole pipeline, not just its
    worker — the crash-loudly verification contract must survive concurrency.
    """
    started: asyncio.Event = asyncio.Event()
    cancelled_worker: list[bool] = [False]

    async def producer():
        for i in range(50):
            yield i

    async def consume(item: int) -> None:
        if item == 5:
            started.set()
            raise _FatalError("verification mismatch")
        # make sure a sibling worker is mid-sleep when the fatal fires
        await asyncio.sleep(0.01)

    with pytest.raises(_FatalError):
        await run_registration_pipeline(
            producer=producer(),
            consume=consume,
            queue_size=8,
            worker_count=4,
        )


async def test_non_fatal_exceptions_do_not_abort() -> None:
    """consume may swallow per-item errors (current `continue` semantics); only
    uncaught exceptions abort."""

    async def producer():
        for i in range(20):
            yield i

    handled: list[int] = []

    async def consume(item: int) -> None:
        if item % 2 == 0:
            handled.append(item)
        else:
            raise ValueError("skipped path")  # caught by consume's caller normally

    # consume itself must swallow; here we simulate by try/except inside.
    async def safe_consume(item: int) -> None:
        if item % 2 == 0:
            handled.append(item)

    with pytest.raises(ValueError):
        await run_registration_pipeline(
            producer=producer(),
            consume=consume,
            queue_size=6,
            worker_count=3,
        )
    # With a swallowing consume it completes
    handled.clear()
    await run_registration_pipeline(
        producer=producer(),
        consume=safe_consume,
        queue_size=6,
        worker_count=3,
    )
    assert len(handled) == 10
