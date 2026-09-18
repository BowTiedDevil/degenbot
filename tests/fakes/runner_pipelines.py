"""Fakes for the runner's sim-submit pipeline.

Two independent consumers:

- ``StubPipeline`` voids the pipeline at the consumer's module seam (block-
  loop tests drive streams, not sim leaves). It records the session owner it
  received (the "one owner" contract) and the block clock observed at each
  enqueue; :attr:`StubPipeline.instances` collects the instances one drive
  created (clear it before driving to isolate a test's captures).
- ``SimSubmitHarness`` drives the REAL pipeline through its constructor DI
  seams. Sims are gated per path id with asyncio events, so FIFO ordering,
  semaphore overlap, and completion interleaving are controlled explicitly:
  :meth:`SimSubmitHarness.gate` arms a path, :meth:`SimSubmitHarness.wait_entered`
  blocks until the gated sims are actually in flight,
  :meth:`SimSubmitHarness.release` lets them finish. No sleeps anywhere.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, ClassVar

from degenbot.runner._sim_submit_pipeline import PipelineSeams, SimSubmitPipeline

if TYPE_CHECKING:
    from collections.abc import Iterable


@dataclass(frozen=True)
class SimCandidate:
    path_id: int


@dataclass
class SimOutcome:
    gas_profitable: list[int] = field(default_factory=list)


class FakeSimSubmitSession:
    """Stands in for ``_SessionState`` at the pipeline's constructor seam."""

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
class _SimGate:
    entered: asyncio.Event = field(default_factory=asyncio.Event)
    release: asyncio.Event = field(default_factory=asyncio.Event)


class SimSubmitHarness:
    """Constructor-seam fakes + observation state for one pipeline run."""

    def __init__(self) -> None:
        self.session = FakeSimSubmitSession()
        self.in_flight = 0
        self.max_in_flight = 0
        self.submitted_ids: list[int] = []
        self._gates: dict[int, _SimGate] = {}

    def gate(self, path_id: int) -> _SimGate:
        if path_id not in self._gates:
            self._gates[path_id] = _SimGate()
        return self._gates[path_id]

    async def wait_entered(self, path_ids: Iterable[int]) -> None:
        await asyncio.gather(*(self.gate(pid).entered.wait() for pid in path_ids))

    def release(self, path_ids: Iterable[int]) -> None:
        for pid in path_ids:
            self.gate(pid).release.set()

    def pipeline(
        self,
        concurrency: int,
        **seam_overrides: object,
    ) -> SimSubmitPipeline:
        """Build the pipeline wired to this harness, with per-test overrides."""
        seams: dict[str, object] = {
            "candidate_builder": self.build_candidates,
            "simulator": self.simulate,
            "renderer": self.render_outcome,
            "submitter": self.submit,
        }
        seams.update(seam_overrides)
        return SimSubmitPipeline(  # type: ignore[arg-type]
            self.session,
            concurrency=concurrency,
            seams=PipelineSeams(**seams),  # type: ignore[arg-type]
        )

    def build_candidates(
        self,
        session: object,
        results: list[tuple],
        **kwargs: object,
    ) -> list[SimCandidate]:
        # The real builder needs an engine; tests here exercise the pipeline
        # (concurrency + ordering), not candidate shaping.
        return [SimCandidate(path_id=results[0][0])]

    async def simulate(self, **kwargs: object) -> SimOutcome:
        pid = kwargs["candidates"][0].path_id  # type: ignore[index,union-attr]
        self.in_flight += 1
        self.max_in_flight = max(self.max_in_flight, self.in_flight)
        gate = self._gates.get(pid)
        try:
            if gate is not None:
                gate.entered.set()
                await gate.release.wait()
            return SimOutcome(gas_profitable=[pid])
        finally:
            self.in_flight -= 1

    def render_outcome(self, session: object, outcome: object, current_block: int) -> None:
        return None

    def submit(
        self,
        session: object,
        outcome: object,
        *,
        operator_nonce: int,
        **kwargs: object,
    ) -> None:
        self.submitted_ids.append(outcome.gas_profitable[0])  # type: ignore[attr-defined]


class StubPipeline:
    """Void stand-in for ``SimSubmitPipeline`` at the consumer's module seam."""

    instances: ClassVar[list[StubPipeline]] = []

    def __init__(self, session: object, **kwargs: object) -> None:
        self.session = session
        self.enqueue_clock: list[int] = []
        StubPipeline.instances.append(self)

    async def enqueue(
        self,
        results: object,
        *,
        block_timestamp: int,
        base_fee_next: int,
        payloads: dict[int, dict] | None = None,
    ) -> None:
        # Mirrors the real pipeline, which stamps the session's dispatcher
        # clock at enqueue time.
        self.enqueue_clock.append(self.session.dispatcher.current_block)  # type: ignore[union-attr]

    def raise_if_failed(self) -> None:
        return None

    async def stop(self) -> None:
        return None
