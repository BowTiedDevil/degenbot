"""Adapter tests for the batch-executor construction seam.

The mechanism itself (bounded concurrency, FIFO submit order, fail-loud on a
leaf failure, counters) is Rust-owned and tested in the core crates
(``degenbot-submission``'s ``sim_pipeline`` + ``degenbot-batch-executor``).
The cap's value resolution (``DEGENBOT_SIM_PIPELINE_CONCURRENCY``, declared
default 8) is the verdict's; the conversion inside the FFI applies the only
floor, so these tests pin the cascade ANSWER and the conversion's projection
(``degenbot._ffi.simulation.executor_policy_py``) — the driver itself
resolves no policy value and carries no twin clamp. The construction seam's
kwargs are pinned by ``tests/rust/test_batch_executor_seam.py``.
"""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest

from degenbot._ffi.simulation import executor_policy_py
from degenbot.dispatch import SimSubmitPipeline
from tests.helpers import verdict_probe as probe


def _policy(**env: str) -> object:
    """The converted executor policy for a hypothetical environment."""

    return executor_policy_py(probe.hypothetical_values(env))


def test_the_declared_default_is_eight() -> None:
    policy = _policy()
    assert policy.sim_concurrency == 8
    assert (
        policy.sim_concurrency == probe.resolved_value("simulation.pipeline_concurrency")["value"]
    )


def test_one_reproduces_the_serial_reference() -> None:
    assert _policy(DEGENBOT_SIM_PIPELINE_CONCURRENCY="1").sim_concurrency == 1


def test_zero_is_floored_at_one() -> None:
    """A cap of zero sims would wedge the pipeline, not serialize it.

    The verdict answers 0 (Python keeps no twin floor); the conversion in
    the FFI is the ONE clamp owner, so the executor policy reads 1.
    """

    assert (
        probe.resolved_value(
            "simulation.pipeline_concurrency", env={"DEGENBOT_SIM_PIPELINE_CONCURRENCY": "0"}
        )["value"]
        == 0
    )
    assert _policy(DEGENBOT_SIM_PIPELINE_CONCURRENCY="0").sim_concurrency == 1


def test_the_operator_file_reaches_the_cap(tmp_path: Path) -> None:
    """The A/B arm is a declared key, so the file layer arms it too."""

    with probe.operator_file("[simulation]\npipeline_concurrency = 3\n") as written:
        assert (
            probe.resolved_value("simulation.pipeline_concurrency", operator_file=written)["value"]
            == 3
        )
        assert (
            executor_policy_py(probe.hypothetical_values(operator_file=written)).sim_concurrency
            == 3
        )


async def test_ffi_pipeline_drives_the_python_leaves_in_order() -> None:
    """The FFI pipeline awaits the sim leaf then the submit leaf per batch."""

    submitted: list[int] = []

    async def sim(work: object) -> object:
        await asyncio.sleep(0)
        return work

    async def submit(_work: object, outcome: object) -> None:
        submitted.append(outcome)  # type: ignore[arg-type]

    pipeline = SimSubmitPipeline(sim=sim, submit=submit, concurrency=2)
    for batch in range(3):
        pipeline.enqueue(batch)
    await pipeline.shutdown()

    assert submitted == [0, 1, 2]
    assert pipeline.enqueued == 3
    assert pipeline.submitted == 3


async def test_a_leaf_failure_surfaces_through_raise_if_failed() -> None:
    class LeafExploded(RuntimeError):
        """Test leaf failure carrier (raw-string-exception lint)."""

    async def sim(_work: object) -> object:
        raise LeafExploded

    async def submit(_work: object, _outcome: object) -> None:
        return None

    pipeline = SimSubmitPipeline(sim=sim, submit=submit, concurrency=1)
    pipeline.enqueue(0)
    with pytest.raises(RuntimeError, match="aborting the consumer loudly"):
        await pipeline.shutdown()
