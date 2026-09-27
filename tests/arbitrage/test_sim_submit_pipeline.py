"""SIMPIPE option A acceptance — concurrent sims, ordered submits, loud abort.

The pipeline (``_sim_submit_pipeline.SimSubmitPipeline``) must:
- submit batches STRICTLY in arrival (FIFO) order even when sims complete
  out of order (option A's point: sim concurrency + submit ordering — the
  nonce-serialization contract the serial loop had);
- bound in-flight sims to the configured semaphore width;
- fetch the operator nonce once per SUBMISSION (at submit time, not enqueue);
- surface a failed sim/submit leaf via ``raise_if_failed`` (the loud-abort
  contract — incident 2026-08-20's no-silent-death rule).

The pipeline's collaborators (candidate builder, FFI sim, renderer, submit
leaf) arrive through its constructor DI seams, and sims are gated with
asyncio events (see ``tests.fakes.runner_pipelines.SimSubmitHarness``) — no
live RPC, no Rust engine, no wall-clock sleeps.
"""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest

import degenbot.runner._sim_submit_pipeline as mod
from tests.fakes.runner_pipelines import SimSubmitHarness
from tests.helpers import verdict_probe as probe

Raw = tuple[int, int, int, tuple[int, ...], tuple[int, ...], int, tuple[int, ...]]


class _SimRocketExploded(RuntimeError):
    """Test leaf failure carrier (raw-string-exception lint)."""


async def test_fifo_submit_order_under_out_of_order_sims() -> None:
    """Arrival order 0..4 with sims finishing in REVERSE — submits stay FIFO."""
    harness = SimSubmitHarness()
    pipe = harness.pipeline(concurrency=8)
    for pid in range(5):
        harness.gate(pid)
    for pid in range(5):
        await pipe.enqueue([_raw(pid)], block_timestamp=1, base_fee_next=1)
    await harness.wait_entered(range(5))
    # Sims 4..1 complete while the queue head (pid 0) is still gated.
    harness.release([4, 3, 2, 1])
    assert harness.submitted_ids == [], "no submit may pass the still-gated head"
    harness.release([0])
    await pipe.stop()
    assert harness.submitted_ids == [0, 1, 2, 3, 4], "submit order must be FIFO"
    assert harness.session.nonce_calls == 5, "one nonce fetch per submission"


async def test_sims_bounded_by_semaphore() -> None:
    """In-flight sims never exceed the configured width (and DO exceed 1)."""
    harness = SimSubmitHarness()
    pipe = harness.pipeline(concurrency=3)
    for pid in range(6):
        harness.gate(pid)
    for pid in range(6):
        await pipe.enqueue([_raw(pid)], block_timestamp=1, base_fee_next=1)
    # Three gated sims in flight = the semaphore is saturated with LIVE overlap.
    await harness.wait_entered(range(3))
    peak_during_run = harness.max_in_flight
    harness.release(range(6))
    await pipe.stop()
    assert harness.submitted_ids == list(range(6))
    assert peak_during_run >= 2, "sims must overlap (the whole point of A)"
    assert harness.max_in_flight <= 3, "semaphore bounds in-flight sims"


async def test_leaf_failure_aborts_loudly() -> None:
    """A sim failure is stored and re-raised from the consumer's frame."""
    harness = SimSubmitHarness()
    base = harness.simulate

    async def fail_first(**kwargs):
        if kwargs["candidates"][0].path_id == 99:
            raise _SimRocketExploded
        return await base(**kwargs)

    pipe = harness.pipeline(concurrency=2, simulator=fail_first)
    await pipe.enqueue([_raw(1)], block_timestamp=1, base_fee_next=1)
    await pipe.enqueue([_raw(99)], block_timestamp=1, base_fee_next=1)
    # The failing leaf kills the submitter; its death is the deterministic
    # signal that the failure is stored.
    assert pipe._submitter is not None
    await asyncio.wait([pipe._submitter])
    with pytest.raises(RuntimeError, match="aborting the consumer loudly"):
        pipe.raise_if_failed()


def _concurrency(**env: str) -> int:
    """The resolved cap for a hypothetical environment."""
    return probe.build_config(env=env).sim_pipeline_concurrency


def test_the_declared_default_is_eight() -> None:
    assert _concurrency() == 8


def test_one_reproduces_the_serial_reference() -> None:
    assert _concurrency(DEGENBOT_SIM_PIPELINE_CONCURRENCY="1") == 1


def test_zero_is_floored_at_one() -> None:
    """A cap of zero sims would wedge the pipeline, not serialize it."""
    assert _concurrency(DEGENBOT_SIM_PIPELINE_CONCURRENCY="0") == 1


def test_the_operator_file_reaches_the_cap(tmp_path: Path) -> None:
    """The A/B arm is a declared key, so the file layer arms it too."""
    with probe.operator_file("[simulation]\npipeline_concurrency = 3\n") as written:
        assert probe.resolved_value("simulation.pipeline_concurrency", operator_file=written)[
            "value"
        ] == 3


def _raw(pid: int) -> Raw:
    return (pid, 1000, 500, (1, 2), (1, 2), 77, ())
