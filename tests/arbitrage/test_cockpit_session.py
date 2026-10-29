"""Cockpit session state — the one owner of pump-session coordination state.

The block loop and the dispatch path must read the SAME session the cockpit
owns, instead of the session travelling as a 10-parameter signature + 9
kwargs. The loop receives its collaborators as one
:class:`~degenbot.runner._consume.BlockLoop` record assembled from that
session, and the sim/submit pipeline (which the loop hands every batch to) is
built over the same owner.

Seams: the public session lifecycle (an injected consumer fake observes the
owner) and the loop seam (``consume_result_batches`` driven with the record +
injected streams). No anvil, no live RPC.
"""

from __future__ import annotations

import signal

import pytest

from degenbot.runner._consume import consume_result_batches
from tests.fakes.engine import FakeEngineRegistry
from tests.fakes.runner_pipelines import StubPipeline
from tests.helpers.session import FakeDispatcher, session_for_test


@pytest.fixture(autouse=True)
def _restore_sigint() -> None:
    yield
    signal.signal(signal.SIGINT, signal.SIG_DFL)


class AsyncOnce:
    """Yield one item, then StopAsyncIteration."""

    def __init__(self, item: object) -> None:
        self._item = item
        self._done = False

    def __aiter__(self) -> AsyncOnce:
        return self

    async def __anext__(self) -> object:
        if self._done:
            raise StopAsyncIteration
        self._done = True
        return self._item


def _block_tick(number: int) -> dict[str, int]:
    return {
        "number": number,
        "timestamp": 1_700_000_000 + number,
        "base_fee_per_gas": 1_000_000_000,
        "gas_used": 15_000_000,
        "gas_limit": 30_000_000,
    }


def _batch() -> dict[str, object]:
    # One fresh result (path_id, optimal_input, profit, hop_outs,
    # consumed_ins, state_nonces) so the dispatch leaf fires.
    return {
        "fresh": [(1, 1_000, 5_000, [2_000], [1_000], [5])],
        "updated": [],
        "removed": [],
        "solve_block": 12_346,
        "base_fee_per_gas": 1_000_000_000,
        "gas_used": 15_000_000,
        "gas_limit": 30_000_000,
    }


async def _drive_one_shot(
    session: object,
    *,
    tick: int,
    batch: dict[str, object] | None = None,
) -> None:
    """Run the real consumer loop over one block tick + one result batch."""
    await consume_result_batches(
        session.block_loop(),  # type: ignore[attr-defined]
        block_stream=AsyncOnce(_block_tick(tick)),
        result_iter=AsyncOnce(batch if batch is not None else _batch()),
        allow_quiet_end=True,  # injected one-shot streams end by design
    )


class TestSessionOwner:
    async def test_runner_consumer_receives_the_owner(self) -> None:
        """The consumer task and its pipeline are built over the SAME owner."""
        captured: dict[str, object] = {}

        def capturing_consumer(loop: object, **kwargs: object) -> object:
            captured["loop"] = loop
            captured.update(kwargs)

            async def _n() -> None:
                pass

            return _n()

        StubPipeline.instances.clear()
        session = session_for_test(consumer=capturing_consumer)
        async with session:
            await session.run()

        loop = captured["loop"]
        assert loop is not None, "the consumer must be handed the loop's collaborators"
        assert loop.pipeline is session.sim_submit_pipeline  # type: ignore[attr-defined]
        # SIMPIPE: the "same owner" contract rides the pipeline seam — the
        # session built the pipeline over itself, and the loop enqueues on it.
        assert [stub.session for stub in StubPipeline.instances] == [session]

    async def test_the_loop_receives_the_session_pipeline(self) -> None:
        StubPipeline.instances.clear()
        dispatcher = FakeDispatcher(current_block=12_346)
        session = session_for_test(dispatcher=dispatcher, current_block=12_345)
        await session.build_pipeline()

        await _drive_one_shot(session, tick=12_347)

        assert session.current_block == 12_347
        assert [stub.session for stub in StubPipeline.instances] == [session], (
            "the dispatch pipeline did not receive the session owner"
        )

    async def test_owner_advances_with_the_block_clock(self) -> None:
        """The session mirrors the newHeads clock as the loop applies it."""
        session = session_for_test(
            dispatcher=FakeDispatcher(current_block=12_346),
            current_block=12_345,
        )
        await session.build_pipeline()

        await _drive_one_shot(session, tick=12_347)

        assert session.current_block == 12_347

    async def test_engine_registry_is_read_off_the_actors(self) -> None:
        """The loop's result stream comes from the session's own registry."""
        registry = FakeEngineRegistry()
        session = session_for_test(engine_registry=registry)
        await session.build_pipeline()

        loop = session.block_loop()

        assert loop.engine is registry.engine
        assert len(registry.start_calls) == 0, (
            "a for_test session resolves without driving the engine's start ritual"
        )
