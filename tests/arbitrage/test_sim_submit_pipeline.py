"""SIMPIPE option A acceptance — concurrent sims, ordered submits, loud abort.

The pipeline (``_sim_submit_pipeline.SimSubmitPipeline``) must:
- submit batches STRICTLY in arrival (FIFO) order even when sims complete
  out of order (option A's point: sim concurrency + submit ordering — the
  nonce-serialization contract the serial loop had);
- bound in-flight sims to the configured semaphore width;
- fetch the operator nonce once per SUBMISSION (at submit time, not enqueue);
- surface a failed sim/submit leaf via ``raise_if_failed`` (the loud-abort
  contract — incident 2026-08-20's no-silent-death rule).

The Rust FFI seams (``dispatch_profitable`` / ``dispatch_and_submit``) are
monkeypatched at the module boundary — no live RPC, no Rust engine.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field

import pytest

import degenbot.runner._sim_submit_pipeline as mod
from degenbot.runner._sim_submit_pipeline import SimSubmitPipeline

Raw = tuple[int, int, int, tuple[int, ...], tuple[int, ...], int, tuple[int, ...]]


class _FakeSession:
    """The pipeline's real dependencies, faked at the module seams."""

    def __init__(self) -> None:
        self.dispatcher = type("D", (), {"current_block": 42})()
        self.sim_ctx = object()
        self.cfg = type(
            "C",
            (),
            {
                "operator_address": "0xop",
                "dry_run": True,
                "inject_executor_code": False,
                "min_profit_margin_bps": 0,
            },
        )()
        self.async_w3 = self
        self.engine_registry = type("R", (), {"engine": object()})()
        self.nonce_calls = 0

    async def get_transaction_count(self, address: str) -> int:
        self.nonce_calls += 1
        return self.nonce_calls


@dataclass
class _FakeCandidate:
    path_id: int


@dataclass
class _Outcome:
    gas_profitable: list[int] = field(default_factory=list)


class _Harness:
    """Patched module seams + observation state for one test run."""

    def __init__(self, monkeypatch: pytest.MonkeyPatch, sim_delays: dict[int, float]) -> None:
        self.in_flight = 0
        self.max_in_flight = 0
        self.sim_order: list[int] = []
        self.submitted_ids: list[int] = []
        self.sim_delays = sim_delays

        async def fake_dispatch_profitable(**kwargs):
            pid = kwargs["candidates"][0].path_id
            self.in_flight += 1
            self.max_in_flight = max(self.max_in_flight, self.in_flight)
            try:
                await asyncio.sleep(sim_delays.get(pid, 0.0))
                return _Outcome(gas_profitable=[pid])
            finally:
                self.in_flight -= 1

        def fake_submit(session, outcome, *, operator_nonce, inject_code=True):
            self.submitted_ids.append(outcome.gas_profitable[0])

        monkeypatch.setattr(mod, "dispatch_profitable", fake_dispatch_profitable)
        monkeypatch.setattr(mod, "_submit_batch_records", fake_submit)
        monkeypatch.setattr(mod, "_render_outcome", lambda *a, **k: None)


@pytest.fixture
def harness(monkeypatch: pytest.MonkeyPatch) -> _Harness:
    """Patch module seams + make the candidate builder emit cheap stand-ins."""
    h = _Harness(monkeypatch, sim_delays={})
    # Cheap candidate stand-ins: the real builder needs an engine; the seam
    # under test is the PIPELINE (concurrency + ordering), not the builder
    # (the dispatch tests already cover builder shaping).
    monkeypatch.setattr(
        mod,
        "_build_dispatch_candidates",
        lambda session, results, **kwargs: [_FakeCandidate(path_id=results[0][0])],
    )
    return h


async def test_fifo_submit_order_under_out_of_order_sims(harness: _Harness) -> None:
    """Arrival order 0..4 with sims finishing in REVERSE — submits stay FIFO."""
    session = _FakeSession()
    pipe = SimSubmitPipeline(session, concurrency=8)
    delays = {0: 0.20, 1: 0.16, 2: 0.12, 3: 0.08, 4: 0.04}
    for pid in range(5):
        harness.sim_delays[pid] = delays[pid]
    for pid in range(5):
        await pipe.enqueue([_raw(pid)], block_timestamp=1, base_fee_next=1)
    await pipe.stop()
    assert harness.submitted_ids == [0, 1, 2, 3, 4], "submit order must be FIFO"
    assert session.nonce_calls == 5, "one nonce fetch per submission"


async def test_sims_bounded_by_semaphore(harness: _Harness) -> None:
    """In-flight sims never exceed the configured width (and DO exceed 1)."""
    session = _FakeSession()
    pipe = SimSubmitPipeline(session, concurrency=3)
    for pid in range(6):
        harness.sim_delays[pid] = 0.05
    for pid in range(6):
        await pipe.enqueue([_raw(pid)], block_timestamp=1, base_fee_next=1)
    await asyncio.sleep(0.02)  # let the sims overlap
    peak_during_run = harness.max_in_flight
    await pipe.stop()
    assert harness.submitted_ids == list(range(6))
    assert peak_during_run >= 2, "sims must overlap (the whole point of A)"
    assert harness.max_in_flight <= 3, "semaphore bounds in-flight sims"


class _SimRocketExploded(RuntimeError):
    """Test leaf failure carrier (raw-string-exception lint)."""


async def test_leaf_failure_aborts_loudly(harness: _Harness) -> None:
    """A sim failure is stored and re-raised from the consumer's frame."""
    session = _FakeSession()
    pipe = SimSubmitPipeline(session, concurrency=2)

    orig = mod.dispatch_profitable

    async def fail_first(**kwargs):
        if kwargs["candidates"][0].path_id == 99:
            raise _SimRocketExploded
        return await orig(**kwargs)

    mod.dispatch_profitable = fail_first
    try:
        await pipe.enqueue([_raw(1)], block_timestamp=1, base_fee_next=1)
        await pipe.enqueue([_raw(99)], block_timestamp=1, base_fee_next=1)
        await asyncio.sleep(0.05)  # let the failing sim surface
        with pytest.raises(RuntimeError, match="aborting the consumer loudly"):
            pipe.raise_if_failed()
    finally:
        mod.dispatch_profitable = orig


def test_concurrency_env_parse(monkeypatch: pytest.MonkeyPatch) -> None:
    """Floor at 1, default 8, garbage tolerated."""
    monkeypatch.delenv("DEGENBOT_SIM_PIPELINE_CONCURRENCY", raising=False)
    assert mod.pipeline_concurrency_from_env() == 8
    monkeypatch.setenv("DEGENBOT_SIM_PIPELINE_CONCURRENCY", "1")
    assert mod.pipeline_concurrency_from_env() == 1
    monkeypatch.setenv("DEGENBOT_SIM_PIPELINE_CONCURRENCY", "0")
    assert mod.pipeline_concurrency_from_env() == 1
    monkeypatch.setenv("DEGENBOT_SIM_PIPELINE_CONCURRENCY", "garbage")
    assert mod.pipeline_concurrency_from_env() == 8


def _raw(pid: int) -> Raw:
    return (pid, 1000, 500, (1, 2), (1, 2), 77, ())
