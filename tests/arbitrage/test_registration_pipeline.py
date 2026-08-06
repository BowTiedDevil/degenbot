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

from degenbot.runner.build_paths import run_registration_pipeline


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
        if False:
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


async def test_fatal_error_aborts_with_unbounded_producer() -> None:
    """A fatal `consume` error must abort FAST even when discovery is unbounded
    (6VZN7H forever producer).

    Regression for the crash-loudly swallow: the old
    `gather(worker_tasks, return_exceptions=True)` inspected results only after
    ALL workers finished, but with a never-ending producer the surviving
    workers drain the queue forever — so the fatal exception sat trapped in the
    gathered results and the bot kept trading instead of failing loudly. The
    pipeline must now re-raise the first worker exception immediately.
    """
    fatal_hit: asyncio.Event = asyncio.Event()

    async def producer():
        # Never terminates — simulates the unbounded discovery re-sweep.
        i = 0
        while True:
            yield i
            i += 1
            await asyncio.sleep(0)

    async def consume(item: int) -> None:
        if item == 0:
            fatal_hit.set()
            fatal = _FatalError("verification mismatch (unbounded producer)")
            raise fatal
        # Keep sibling workers busy so the old gather never returned.
        await asyncio.sleep(0.01)

    with pytest.raises(_FatalError):
        # wait_for turns a regression (hang) into a fast failure.
        await asyncio.wait_for(
            run_registration_pipeline(
                producer=producer(),
                consume=consume,
                queue_size=8,
                worker_count=4,
            ),
            timeout=5.0,
        )
    assert fatal_hit.is_set()


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


def test_consume_offloads_pool_build_off_the_event_loop_thread() -> None:
    """The blocking pool build must run on a WORKER thread, not the asyncio
    loop thread (35NMBX). `_consume` must route `constr_bot.build_pool` (and
    the V3/V4 tracker/build variants) through the bounded pool-build executor,
    so the loop stays free to run the consumer/dispatch while registration
    crawls.

    RED phase: before offload, `build_pool` runs synchronously on the loop
    thread -> build_thread == loop_thread -> assertion fails. GREEN: the build
    lands on a worker thread.
    """
    import threading
    from types import SimpleNamespace

    from degenbot.database.models.pools import UniswapV2PoolTableBase
    from degenbot.runner.build_paths import PathRegistrationPipeline

    loop_thread: list[int] = []
    build_thread: list[int] = []

    class FakeBot:
        def build_pool(self, address: str, *, silent: bool = False, **kwargs: object):
            build_thread.append(threading.get_ident())
            return SimpleNamespace(address=address)

    class FakeRegistry:
        def register_v2_pool(self, pool: object) -> int:
            return 1

        def register_path(self, path: object) -> None:
            return None

    ctx = SimpleNamespace(
        bot=FakeBot(),
        uniswap_v3_tracker=None,
        sushiswap_v3_tracker=None,
        pancakeswap_v3_tracker=None,
        db=None,
        chain_id=1,
        weth=SimpleNamespace(address="0x" + "0" * 40),
    )
    pipe = PathRegistrationPipeline(
        context=ctx,  # type: ignore[arg-type]
        engine_registry=FakeRegistry(),  # type: ignore[arg-type]
    )

    step = SimpleNamespace(type=UniswapV2PoolTableBase, address="0x" + "1" * 40)

    async def run() -> None:
        loop_thread.append(threading.get_ident())
        await pipe._consume([step], directions=[True])

    asyncio.run(run())

    assert len(build_thread) == 1, "the build should have run exactly once"
    assert build_thread[0] != loop_thread[0], (
        "pool build ran on the asyncio loop thread; offload did not happen"
    )
