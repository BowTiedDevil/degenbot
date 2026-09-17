"""Concurrent-sim + ordered-submit pipeline (SIMPIPE epic, option A).

The solver already streams one candidate per ``ResultBatch`` the moment its
solve completes (``DEGENBOT_STREAMING_DELIVERY``); the legacy consumer awaited
each simulate round-trip SEQUENTIALLY, so per-block candidates paid
``sum(sim_i)`` of wall time (~130ms/block live, 6.6ms avg per candidate per
the T1 lab - ``logs/simpipe_lab.md``, a gitignored logs/ artifact since removed).

This module pipelines the leaf:

- **Sim fan-out**: every batch's simulate work spawns as its own asyncio task,
  bounded by a semaphore (``DEGENBOT_SIM_PIPELINE_CONCURRENCY``, default 8).
  The Rust fan-out future is GIL-free and each call builds its own sim handle
  (build ~0.03ms - lab), so K batches simulate truly in parallel on the tokio
  runtime the pump already drives. Cross-candidate cache reuse is low (lab
  finding 4) - concurrency is a pure latency win.
- **Submit fan-in**: ONE consumer task pops batch descriptors in ARRIVAL
  (FIFO) order and awaits each batch's own sim before submitting. Submission
  semantics are byte-identical to today's serialized loop (same ordering, one
  nonce fetch per submit at the moment of submit) - only the sims overlap.

Loud-abort contract: a sim or submit failure is stored and re-raised from
:func:`~degenbot.runner._consume.consume_result_batches`' main loop (via
:func:`SimSubmitPipeline.raise_if_failed`), preserving the "no silent pump
death" rule (incident 2026-08-20).
"""

from __future__ import annotations

import asyncio
import os
from dataclasses import dataclass
from typing import TYPE_CHECKING

from degenbot.logging import logger as bot_logger
from degenbot.runner._dispatch import (
    _build_dispatch_candidates,
    _merge_payload_outcome,
    _render_outcome,
    _submit_batch_records,
    dispatch_profitable,
)
from degenbot.runner._driver_constants import (
    MIN_PROFIT_NET,
)

if TYPE_CHECKING:
    from collections.abc import Callable
    from typing import Any

    from degenbot.dispatch import DispatchOutcome
    from degenbot.runner._dispatch import _RawResult, _SimOutcome
    from degenbot.runner.bot_runner import _SessionState

    _CandidateBuilder = Callable[..., list]
    _Renderer = Callable[..., None]
    _Simulator = Callable[..., Any]
    _Submitter = Callable[..., Any]


def pipeline_concurrency_from_env() -> int:
    """Parse ``DEGENBOT_SIM_PIPELINE_CONCURRENCY`` (default 8, floor 1).

    ``1`` reproduces the legacy serial behavior exactly (one sim in flight,
    FIFO submit) - the A/B arm for the T5 soak.
    """
    raw = os.environ.get("DEGENBOT_SIM_PIPELINE_CONCURRENCY")
    if raw is None or not raw.strip():
        return 8
    try:
        value = int(raw)
    except ValueError:
        return 8
    return max(1, value)


@dataclass
class _BatchWork:
    """One streamed solver batch traversing the pipeline."""

    results: list[_RawResult]
    block_timestamp: int
    base_fee_next: int
    current_block: int
    # SIMPIPE2 T3: the engine's inline-sim payloads (empty = legacy FFI sim
    # for every entry — per-entry presence decides).
    payloads: dict[int, dict] | None = None
    sim_task: asyncio.Task[_SimOutcome | None] | None = None


async def _run_sim(
    session: _SessionState,
    work: _BatchWork,
    sem: asyncio.Semaphore,
    *,
    candidate_builder: _CandidateBuilder,
    simulator: _Simulator,
) -> _SimOutcome | None:
    """The simulate leaf for one batch (GIL-free across the RPC part)."""
    async with sem:
        # Candidate shaping runs under the GIL on this task (same engine lock
        # order as the legacy serial path - engine-then-core via
        # path_info_for_core). SIMPIPE2 T3: payload entries skip the FFI sim
        # (already simulated inline in the engine).
        candidates = candidate_builder(session, work.results, payloads=work.payloads)
        outcome: DispatchOutcome | None = None
        if candidates:
            sim_ctx = session.sim_ctx
            if sim_ctx is None:
                msg = (
                    "SimulateContext is required to dispatch"
                    " (non-Alloy provider or sim context unbuilt)"
                )
                raise RuntimeError(msg)
            outcome = await simulator(
                candidates=candidates,
                context=sim_ctx,
                dispatcher=session.dispatcher,
                base_fee_next=work.base_fee_next,
                current_block=work.current_block,
                block_timestamp=work.block_timestamp,
                min_profit_net=MIN_PROFIT_NET,
                min_profit_margin_bps=session.cfg.min_profit_margin_bps,
                engine=session.engine_registry.engine,
            )
        merged = _merge_payload_outcome(session, outcome, work.payloads)
        if not merged:
            bot_logger.debug("[sim-none] batch produced no dispatchable candidates")
        return merged


async def _submit_ordered(
    session: _SessionState,
    work: _BatchWork,
    outcome: _SimOutcome,
    *,
    renderer: _Renderer,
    submitter: _Submitter,
) -> None:
    """Render + submit one completed batch outcome (the fan-in step)."""
    renderer(session, outcome, work.current_block)
    # The nonce fetch moves to SUBMIT time (it was pre-sim in the serial
    # loop; serialized here it is at least as fresh).
    operator_nonce = int(await session.async_w3.get_transaction_count(session.cfg.operator_address))
    submitted = submitter(
        session,
        outcome,
        operator_nonce=operator_nonce,
    )
    # Test seams (and any future sync fallback) may return a plain value
    # instead of a coroutine - tolerate BOTH, never treat None as awaitable.
    if asyncio.iscoroutine(submitted):
        await submitted


class SimSubmitPipelineLeafFailure(RuntimeError):
    """A sim/submit leaf task failed - the consumer must abort loudly."""


LEAF_FAILURE_MESSAGE = "[sim-submit-pipeline] leaf task failed - aborting the consumer loudly"


class SimSubmitPipeline:
    """K-way concurrent sims, strictly ordered submits (see module doc)."""

    def __init__(
        self,
        session: _SessionState,
        *,
        concurrency: int | None = None,
        candidate_builder: _CandidateBuilder | None = None,
        simulator: _Simulator | None = None,
        renderer: _Renderer | None = None,
        submitter: _Submitter | None = None,
    ) -> None:
        """Wire the pipeline to ``session``.

        ``candidate_builder``/``simulator``/``renderer``/``submitter`` are DI
        seams (the ``_submit_batch_records`` submitter/relay_providers
        pattern): tests inject fakes at construction instead of patching the
        module; omitted kwargs keep the production bindings unchanged.
        """
        self._session = session
        self._concurrency = (
            concurrency if concurrency is not None else pipeline_concurrency_from_env()
        )
        self._sem = asyncio.Semaphore(self._concurrency)
        self._queue: asyncio.Queue[_BatchWork | None] = asyncio.Queue()
        self._submitter: asyncio.Task[None] | None = None
        self._failure: BaseException | None = None
        self._enqueued = 0
        self._submitted = 0
        self._candidate_builder = (
            candidate_builder if candidate_builder is not None else _build_dispatch_candidates
        )
        self._simulator = simulator if simulator is not None else dispatch_profitable
        self._renderer = renderer if renderer is not None else _render_outcome
        self._submit_leaf = submitter if submitter is not None else _submit_batch_records

    @property
    def concurrency(self) -> int:
        """The configured in-flight sim bound (for tests + soak logging)."""
        return self._concurrency

    def start(self) -> None:
        """Spawn the submitter (idempotent). Called once at first enqueue."""
        if self._submitter is None:
            self._submitter = asyncio.ensure_future(self._submit_loop())

    async def stop(self) -> None:
        """Drain in-flight work and stop the submitter (graceful teardown).

        A DEAD submitter (leaf failure already stored) must not hang the join:
        re-raise its exception instead.
        """
        if self._submitter is None:
            return
        if self._submitter.done():
            self._submitter.result()  # re-raise the leaf failure (loud abort)
            self._submitter = None
            return
        await self._queue.join()
        self._queue.put_nowait(None)
        await self._submitter
        self._submitter = None

    def raise_if_failed(self) -> None:
        """Re-raise the first leaf failure in the CALLER's frame (loud abort)."""
        if self._failure is not None:
            failure = self._failure
            self._failure = None
            raise SimSubmitPipelineLeafFailure(LEAF_FAILURE_MESSAGE) from failure

    async def enqueue(
        self,
        results: list[_RawResult],
        *,
        block_timestamp: int,
        base_fee_next: int,
        payloads: dict[int, dict] | None = None,
    ) -> None:
        """Spawn this batch's sim + register it in the FIFO submit queue.

        Returns immediately after backgrounding the sim work (the consumer
        loop keeps advancing); submission happens in :meth:`_submit_loop`.
        """
        self.start()
        work = _BatchWork(
            results=results,
            block_timestamp=block_timestamp,
            base_fee_next=base_fee_next,
            current_block=self._session.dispatcher.current_block,
            payloads=payloads,
        )
        self._enqueued += 1
        self._queue.put_nowait(work)
        work.sim_task = asyncio.ensure_future(
            _run_sim(
                self._session,
                work,
                self._sem,
                candidate_builder=self._candidate_builder,
                simulator=self._simulator,
            )
        )

    async def _submit_loop(self) -> None:
        """The single ordered submitter (fan-in)."""
        try:
            while True:
                work = await self._queue.get()
                try:
                    if work is None:
                        return
                    assert work.sim_task is not None
                    outcome = await work.sim_task
                    if outcome is not None:
                        await _submit_ordered(
                            self._session,
                            work,
                            outcome,
                            renderer=self._renderer,
                            submitter=self._submit_leaf,
                        )
                    self._submitted += 1
                finally:
                    self._queue.task_done()
        except BaseException as exc:
            # Loud-abort contract: store + re-raise; the consumer's main loop
            # surfaces it via raise_if_failed() (kill the run loudly).
            self._failure = exc
            raise
