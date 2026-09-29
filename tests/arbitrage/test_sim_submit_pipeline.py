"""Adapter tests for the sim-submit pipeline seam.

The mechanism itself (bounded concurrency, FIFO submit order, fail-loud on a
sim/submit leaf failure, counters) is Rust-owned and tested in
``degenbot-submission``'s ``sim_pipeline`` suite. Python keeps only the adapter
contracts:

- the knob VALUE parsing (``DEGENBOT_SIM_PIPELINE_CONCURRENCY``, default 8,
  floored at 1) stays driver-side;
- the factory injects the resolved cap into the FFI pipeline;
- the FFI pipeline drives the two Python async leaves and surfaces a leaf
  failure through ``raise_if_failed``.
"""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest

from degenbot.dispatch import SimSubmitPipeline
from degenbot.runner._sim_submit import build_sim_submit_pipeline
from tests.fakes.session import FakeRunnerConfig, FakeRunnerSession
from tests.helpers import verdict_probe as probe


def _concurrency(**env: str) -> int:
    """The resolved cap for a hypothetical environment."""

    return probe.build_config(env=env).sim_pipeline_concurrency


def _session(cap: int = 5) -> FakeRunnerSession:
    """A session double carrying only the factory-read config value."""

    return FakeRunnerSession(cfg=FakeRunnerConfig(sim_pipeline_concurrency=cap))


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


async def test_factory_injects_the_configured_cap() -> None:
    pipeline = build_sim_submit_pipeline(_session(cap=5))
    assert pipeline.concurrency == 5


async def test_factory_honors_an_injected_cap() -> None:
    pipeline = build_sim_submit_pipeline(_session(cap=5), concurrency=3)
    assert pipeline.concurrency == 3


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
