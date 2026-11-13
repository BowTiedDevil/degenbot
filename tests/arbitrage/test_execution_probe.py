from __future__ import annotations

import asyncio
import logging
from typing import TYPE_CHECKING, Any

import pytest

from degenbot.abi import encode_single
from degenbot.logging import logger as bot_logger
from degenbot.runner._sim_submit import (
    ExecutorInputs,
    SubmissionStance,
    build_batch_executor,
)
from degenbot.runner.session import Session, SessionPhase
from tests.deadlock_guard import DEADLOCK_GUARD_S
from tests.helpers.session import OFFLINE_RELAY, offline_cfg, session_for_test

if TYPE_CHECKING:
    from collections.abc import Iterator


class _Pool:
    def __init__(self, ratio: int) -> None:
        self.ratio = ratio

    def calculate_tokens_out(self, zero_for_one: bool, amount_in: int) -> int:
        assert isinstance(zero_for_one, bool)
        return amount_in * self.ratio


class _PyBot:
    def get_pool(self, pool_id: int) -> _Pool:
        return {12: _Pool(10), 13: _Pool(20), 14: _Pool(30)}[pool_id]


class _Bot:
    _py_bot = _PyBot()


class _Engine:
    def pool_id_for_pool(self, family: str, address: str) -> int:
        return {"v2": 12, "v3": 13}[family]

    def pool_id_for_v4_pool(self, manager: str, pool_id: str) -> int:
        return 14

    def payload_path_info(self, path_id: int) -> dict[str, Any]:
        # Path 7 is the full V2-V3-V4 route. The concurrency tests drive their
        # second probe over path 8, a single V3 hop, so its quote takes no thread
        # hop and "the second probe is still waiting" is a pure-asyncio fact.
        if path_id == 8:
            return {
                "path_type": "V3",
                "hops": [
                    {
                        "family": "v3",
                        "pool_address": "0x0000000000000000000000000000000000000003",
                        "token0_address": "0x0000000000000000000000000000000000000010",
                        "token1_address": "0x0000000000000000000000000000000000000011",
                        "fee": 3000,
                        "zfo": False,
                    },
                ],
            }
        assert path_id == 7
        return {
            "path_type": "V2-V3-V4",
            "hops": [
                {
                    "family": "v2",
                    "pool_address": "0x0000000000000000000000000000000000000002",
                    "zfo": True,
                },
                {
                    "family": "v3",
                    "pool_address": "0x0000000000000000000000000000000000000003",
                    "token0_address": "0x0000000000000000000000000000000000000010",
                    "token1_address": "0x0000000000000000000000000000000000000011",
                    "fee": 3000,
                    "zfo": False,
                },
                {
                    "family": "v4",
                    "pool_manager_address": "0x0000000000000000000000000000000000000004",
                    "pool_id_hex": "0x1234",
                    "currency0_address": "0x0000000000000000000000000000000000000011",
                    "currency1_address": "0x0000000000000000000000000000000000000012",
                    "fee": 3000,
                    "tick_spacing": 60,
                    "hook_address": "0x0000000000000000000000000000000000000000",
                    "zfo": True,
                },
            ],
        }


class _Registry:
    engine = _Engine()


class _Provider:
    def __init__(self) -> None:
        self.quotes = iter([20_000, 600_000])

    async def get_block(self, block: int) -> dict[str, int]:
        assert block == 12_345
        return {"base_fee_per_gas": 77, "timestamp": 1_700_000_001}

    async def call(self, target: str, data: bytes, block: int) -> bytes:
        assert block == 12_345
        return encode_single("uint256", next(self.quotes))


class _Pipeline:
    def __init__(self) -> None:
        self.kwargs = None

    async def probe_path(self, **kwargs: Any) -> dict[str, str]:
        self.kwargs = kwargs
        return {"outcome": "failed", "bucket": "InsufficientProfit"}


@pytest.mark.asyncio
async def test_probe_path_quotes_exact_inputs_and_uses_the_batch_executor() -> None:
    pipeline = _Pipeline()
    session = session_for_test(bot=_Bot(), engine_registry=_Registry(), async_w3=_Provider())
    session.phase = SessionPhase.RUNNING
    session.probe_pipeline = pipeline

    result = await session.probe_path(7, optimal_input=100)

    assert result == {"outcome": "failed", "bucket": "InsufficientProfit"}
    assert pipeline.kwargs == {
        "path_id": 7,
        "optimal_input": 100,
        "hop_outputs": [1000, 20_000, 600_000],
        "consumed_inputs": [100, 1000, 20_000],
        "current_block": 12_345,
        "base_fee_next": 77,
        "block_timestamp": 1_700_000_001,
    }


@pytest.mark.asyncio
async def test_zero_balance_probe_builds_flash_funded_pipeline_without_capital(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    pipeline = _Pipeline()
    session = session_for_test(bot=_Bot(), engine_registry=_Registry(), async_w3=_Provider())
    session.phase = SessionPhase.RUNNING
    calls = []

    async def build(inputs: Any, **kwargs: Any) -> _Pipeline:
        calls.append(kwargs)
        return pipeline

    monkeypatch.setattr("degenbot.runner.session.build_batch_executor", build)
    monkeypatch.setattr(
        "degenbot.runner.session.ExecutorInputs.from_session", lambda _session: object()
    )
    result = await session.probe_path(7, optimal_input=100, zero_balance=True)

    assert result == {"outcome": "failed", "bucket": "InsufficientProfit"}
    assert calls == [
        {"self_fund": False, "zero_balance": True, "submission": SubmissionStance.PROBE}
    ]
    assert session.probe_zero_balance is True


class _ConstructionProvider:
    """The session provider a construction boundary reads (never dialed).

    ``get_transaction_count`` IS called: the FFI's nonce lane is seeded from the
    chain read at construction. The probe's lane is never leased, which is the
    property the stance pins — the seed read is a read, not a consumption.
    """

    def __init__(self) -> None:
        self.nonce_reads: list[str] = []

    async def get_transaction_count(self, address: str) -> int:
        self.nonce_reads.append(address)
        return 5

    def as_async_alloy(self) -> object:
        return object()


def _inputs(*, live: bool = True, relay_posture: Any = None) -> ExecutorInputs:
    """The construction inputs one pipeline is built over.

    ``live=True`` is the LIVE configuration (``cfg.dry_run is False``) that a
    probe has to be safe under.
    """
    cfg = offline_cfg(live=live)
    assert cfg.dry_run is not live
    return ExecutorInputs(
        sim_ctx=object(),
        dispatcher=object(),
        engine=object(),
        provider=_ConstructionProvider(),
        cfg=cfg,
        relay_posture=relay_posture,
    )


def _record_construction() -> tuple[list[dict[str, Any]], Any]:
    """The construction seam: record the FFI kwargs instead of building Rust."""
    calls: list[dict[str, Any]] = []

    def build(**kwargs: Any) -> str:
        calls.append(kwargs)
        return "executor"

    return calls, build


@pytest.mark.asyncio
async def test_a_live_probe_pipeline_is_built_non_submitting_without_relay_fan_out(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A probe under a LIVE config is non-submitting by construction.

    The construction hands the FFI the dry-run stance (no nonce lease, no
    signature, no broadcast) and no relay fan-out at all, so nothing is dialed.
    """
    dialed: list[Any] = []

    def dial(*args: Any, **kwargs: Any) -> list[str]:
        dialed.append((args, kwargs))
        return ["relay-provider"]

    monkeypatch.setattr("degenbot.runner._sim_submit._resolve_relay_providers", dial)
    calls, build = _record_construction()

    executor = await build_batch_executor(
        _inputs(relay_posture=OFFLINE_RELAY),
        engine_builder=build,
        submission=SubmissionStance.PROBE,
    )

    assert executor == "executor"
    assert dialed == [], "a probe construction dials no relay"
    (kwargs,) = calls
    assert kwargs["dry_run"] is True, "a probe never takes the live stance"
    assert kwargs["broadcast_providers"] is None, "a probe fans out to no relay"


@pytest.mark.asyncio
async def test_a_production_pipeline_keeps_the_run_mode_and_the_relay_fan_out(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The production stance is unchanged: the run mode and the posture decide."""
    resolved: list[list[str]] = []

    async def resolve(urls: list[str], providers: Any) -> list[str]:
        resolved.append(list(urls))
        return ["relay-provider"]

    monkeypatch.setattr("degenbot.runner._sim_submit._resolve_relay_providers", resolve)
    calls, build = _record_construction()

    await build_batch_executor(
        _inputs(relay_posture=OFFLINE_RELAY),
        engine_builder=build,
        submission=SubmissionStance.PRODUCTION,
    )

    assert resolved == [list(OFFLINE_RELAY.relay_urls)]
    (kwargs,) = calls
    assert kwargs["dry_run"] is False, "a live production executor still submits"
    assert kwargs["broadcast_providers"] == ["relay-provider"]


@pytest.mark.asyncio
async def test_a_dry_run_session_keeps_its_production_relay_fan_out(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A dry-run production session is unchanged: the fan-out is still resolved."""
    resolves: list[list[str]] = []

    async def resolve(urls: list[str], providers: Any) -> list[str]:
        resolves.append(list(urls))
        return ["relay-provider"]

    monkeypatch.setattr("degenbot.runner._sim_submit._resolve_relay_providers", resolve)
    calls, build = _record_construction()

    await build_batch_executor(
        _inputs(live=False, relay_posture=OFFLINE_RELAY), engine_builder=build
    )

    assert resolves == [list(OFFLINE_RELAY.relay_urls)]
    (kwargs,) = calls
    assert kwargs["dry_run"] is True
    assert kwargs["broadcast_providers"] == ["relay-provider"]


@pytest.mark.asyncio
async def test_the_session_builds_its_probe_pipeline_with_the_probe_stance(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The one probe construction site asks for the probe stance explicitly."""
    pipeline = _Pipeline()
    session = session_for_test(bot=_Bot(), engine_registry=_Registry(), async_w3=_Provider())
    session.phase = SessionPhase.RUNNING
    assert session.cfg.dry_run is False, "the session under test is LIVE"
    calls: list[dict[str, Any]] = []

    async def build(inputs: Any, **kwargs: Any) -> _Pipeline:
        calls.append(kwargs)
        return pipeline

    monkeypatch.setattr("degenbot.runner.session.build_batch_executor", build)
    monkeypatch.setattr(
        "degenbot.runner.session.ExecutorInputs.from_session", lambda _session: object()
    )

    await session.probe_path(7, optimal_input=100)

    assert calls == [
        {"self_fund": True, "zero_balance": False, "submission": SubmissionStance.PROBE}
    ]


# ── Response identity and the cancellation lifecycle ──────────────────
#
# The probe path is one operator-triggered critical section over ONE executor
# whose outcome stream is a shared, single-consumer channel. Two hazards follow
# from that shape, and these tests pin the behaviors that close them:
#
#   * two concurrent probes must not interleave on that lane (each caller keeps
#     its own response identity), and a capital-mode swap must not drop the
#     executor holding an in-flight row;
#   * a cancelled or timed-out caller must not leave its row queued for the next
#     caller — the core's drain is cancel-safe in the STREAM direction, so a
#     dropped drain keeps the message. That is the leak, not the fix.


async def _scheduler_turns(count: int = 10) -> None:
    """Yield a few loop turns so a task reaches its next await point.

    Not an assertion: every ordering assertion below gates on a fake's own event,
    and every load-bearing wait carries the suite's deadlock guard.
    """
    for _ in range(count):
        await asyncio.sleep(0)


async def _guarded(awaitable: Any) -> Any:
    """Await an in-process result under the deadlock budget (a guard, not an assertion)."""
    return await asyncio.wait_for(awaitable, DEADLOCK_GUARD_S)


class _LanePipeline:
    """A fake probe executor whose outcome stream is shared, like the core's.

    ``probe_path`` queues its row and then drains it: the returned outcome is the
    row at the head of the queue. Cancelling an awaiting drain leaves the row
    queued — the core's documented cancel-safety contract, and precisely the leak
    the cancellation tests pin against.
    """

    def __init__(self, hold: asyncio.Event | None = None) -> None:
        self.hold = hold
        self.enqueued: list[dict[str, Any]] = []
        self.queue: list[dict[str, Any]] = []
        self.drained: list[dict[str, Any]] = []
        self.in_flight = 0
        self.max_in_flight = 0
        self.cancelled_waits = 0
        self.tasks: list[asyncio.Task[Any]] = []
        self.row_queued = asyncio.Event()

    async def probe_path(self, **kwargs: Any) -> dict[str, Any]:
        row = {"path_id": kwargs["path_id"], "optimal_input": kwargs["optimal_input"]}
        self.enqueued.append(row)
        self.queue.append(row)
        task = asyncio.current_task()
        if task is not None:
            self.tasks.append(task)
        self.in_flight += 1
        self.max_in_flight = max(self.max_in_flight, self.in_flight)
        self.row_queued.set()
        try:
            if self.hold is not None:
                # The parked drain: the row sits in the lane until the test says so.
                await self.hold.wait()
            else:
                # A few turns stand in for the drain's own awaits, so an
                # unserialized second probe would overlap here.
                await _scheduler_turns()
        except asyncio.CancelledError:
            self.cancelled_waits += 1
            raise
        finally:
            self.in_flight -= 1
        outcome = self.queue.pop(0)
        self.drained.append(outcome)
        return outcome


class _EchoProvider:
    """A provider whose quoter answers independently, so N probes can share it."""

    async def get_block(self, block: int) -> dict[str, int]:
        assert block == 12_345
        return {"base_fee_per_gas": 77, "timestamp": 1_700_000_001}

    async def call(self, target: str, data: bytes, block: int) -> bytes:
        assert block == 12_345
        return encode_single("uint256", 1_000_000)


class _GatedProvider:
    """A provider whose quoter call parks until the test releases it."""

    def __init__(self) -> None:
        self.entered = asyncio.Event()
        self.release = asyncio.Event()

    async def get_block(self, block: int) -> dict[str, int]:
        assert block == 12_345
        return {"base_fee_per_gas": 77, "timestamp": 1_700_000_001}

    async def call(self, target: str, data: bytes, block: int) -> bytes:
        assert block == 12_345
        self.entered.set()
        await self.release.wait()
        return encode_single("uint256", 1_000_000)


def _probe_session(provider: Any, pipeline: Any) -> Session:
    """A running session whose probe pipeline is already the self-funded one."""
    session = session_for_test(bot=_Bot(), engine_registry=_Registry(), async_w3=provider)
    session.phase = SessionPhase.RUNNING
    session.probe_pipeline = pipeline
    session.probe_zero_balance = False
    return session


@pytest.fixture
def probe_warnings(caplog: pytest.LogCaptureFixture) -> Iterator[pytest.LogCaptureFixture]:
    """Capture the session logger's warnings (it sets ``propagate = False``)."""
    bot_logger.addHandler(caplog.handler)
    caplog.set_level(logging.WARNING, logger=bot_logger.name)
    try:
        yield caplog
    finally:
        bot_logger.removeHandler(caplog.handler)


@pytest.mark.asyncio
async def test_concurrent_probes_on_one_lane_each_get_their_own_outcome() -> None:
    """Two concurrent probes serialize, and each caller drains its own row.

    Without a session-owned lock the second probe enters the same lane mid-drain,
    so the two waits can take each other's queued outcome (and the response would
    describe the wrong path/amount). The pinned behavior: one probe at a time on
    the lane, and each response carrying the path ID and amount its own caller
    asked for.
    """
    hold = asyncio.Event()
    pipeline = _LanePipeline(hold=hold)
    session = _probe_session(_EchoProvider(), pipeline)

    first = asyncio.create_task(_guarded(session.probe_path(7, optimal_input=100)))
    await _guarded(pipeline.row_queued.wait())

    second = asyncio.create_task(_guarded(session.probe_path(8, optimal_input=250)))
    await _scheduler_turns()
    assert pipeline.enqueued == [{"path_id": 7, "optimal_input": 100}], (
        "the second probe waits for the lane instead of queueing beside the first"
    )

    hold.set()
    assert await first == {"path_id": 7, "optimal_input": 100}
    assert await second == {"path_id": 8, "optimal_input": 250}
    assert pipeline.max_in_flight == 1, "the session runs one probe at a time"
    assert pipeline.drained == [
        {"path_id": 7, "optimal_input": 100},
        {"path_id": 8, "optimal_input": 250},
    ], "each row was drained by the call that queued it"
    assert pipeline.queue == [], "no outcome was left in the lane"


@pytest.mark.asyncio
async def test_a_zero_balance_probe_swaps_the_pipeline_only_after_the_in_flight_one(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A capital-mode swap waits for the in-flight probe; neither row is stranded.

    The self-funded probe is parked inside its drain when the zero-balance probe
    arrives. Swapping ``probe_pipeline`` at that moment would drop the executor
    holding the queued row, so the second probe must wait, then build its own
    flash-funded pipeline, and each caller's response must keep its own path ID
    and mode.
    """
    hold = asyncio.Event()
    self_funded = _LanePipeline(hold=hold)
    flash_funded = _LanePipeline()
    session = _probe_session(_EchoProvider(), self_funded)

    builds: list[dict[str, Any]] = []

    async def build(inputs: Any, **kwargs: Any) -> _LanePipeline:
        builds.append({**kwargs, "queued": list(self_funded.queue)})
        return flash_funded

    monkeypatch.setattr("degenbot.runner.session.build_batch_executor", build)
    monkeypatch.setattr(
        "degenbot.runner.session.ExecutorInputs.from_session", lambda _session: object()
    )

    funded = asyncio.create_task(_guarded(session.probe_path(7, optimal_input=100)))
    await _guarded(self_funded.row_queued.wait())

    zero = asyncio.create_task(
        _guarded(session.probe_path(8, optimal_input=250, zero_balance=True))
    )
    await _scheduler_turns()
    assert session.probe_pipeline is self_funded, "the swap waits for the in-flight probe"

    hold.set()
    assert await funded == {"path_id": 7, "optimal_input": 100}
    assert await zero == {"path_id": 8, "optimal_input": 250}

    (swap,) = builds
    assert swap == {
        "self_fund": False,
        "zero_balance": True,
        "submission": SubmissionStance.PROBE,
        "queued": [],
    }, "the flash-funded executor was built only once the self-funded lane drained"
    assert session.probe_pipeline is flash_funded
    assert session.probe_zero_balance is True
    assert self_funded.drained == [{"path_id": 7, "optimal_input": 100}]
    assert flash_funded.drained == [{"path_id": 8, "optimal_input": 250}]


@pytest.mark.asyncio
async def test_a_cancelled_probe_leaves_no_outcome_for_the_next_caller() -> None:
    """Cancelling a probe AFTER its row is queued must not leak that row.

    This is the case a bare ``async with lock:`` gets wrong: the cancelled call's
    drain future is dropped with the row still in the shared lane, and the next
    caller's drain takes it as its own answer. The pinned behavior: the row is
    drained by the call that queued it even though that call was cancelled, and
    the next probe receives its own outcome.
    """
    hold = asyncio.Event()
    pipeline = _LanePipeline(hold=hold)
    session = _probe_session(_EchoProvider(), pipeline)

    first = asyncio.create_task(_guarded(session.probe_path(7, optimal_input=100)))
    await _guarded(pipeline.row_queued.wait())
    assert pipeline.queue == [{"path_id": 7, "optimal_input": 100}], "the row is in the lane"

    first.cancel()
    await _scheduler_turns()
    assert pipeline.queue != [], "the abandoned drain is still pending"
    assert pipeline.cancelled_waits == 0, "the drain was not cancelled with its caller"

    hold.set()
    with pytest.raises(asyncio.CancelledError):
        await first

    assert pipeline.queue == [], "the cancelled probe reaped its own row"
    assert pipeline.drained == [{"path_id": 7, "optimal_input": 100}]

    result = await _guarded(session.probe_path(8, optimal_input=250))
    assert result == {"path_id": 8, "optimal_input": 250}, "not the cancelled caller's row"
    assert pipeline.drained == [
        {"path_id": 7, "optimal_input": 100},
        {"path_id": 8, "optimal_input": 250},
    ]


@pytest.mark.asyncio
async def test_a_timed_out_probe_does_not_strand_its_outcome() -> None:
    """A supervisor timeout cancels the caller, never the queued row's drain.

    ``asyncio.timeout`` is how a driver bounds an operator request. The pinned
    behavior: the deadline surfaces to that caller as ``TimeoutError`` while the
    already-queued row is still drained by its own call, so the next probe gets
    its own outcome.
    """
    hold = asyncio.Event()
    pipeline = _LanePipeline(hold=hold)
    session = _probe_session(_EchoProvider(), pipeline)
    loop = asyncio.get_running_loop()

    deadline = asyncio.timeout(None)

    async def probe() -> Any:
        async with deadline:
            return await session.probe_path(7, optimal_input=100)

    timed = asyncio.create_task(probe())
    await _guarded(pipeline.row_queued.wait())

    deadline.reschedule(loop.time())  # the supervisor's deadline expires now
    await _scheduler_turns()
    hold.set()

    await asyncio.wait({timed}, timeout=DEADLOCK_GUARD_S)
    assert timed.done(), "the timed-out probe returned within the guard"
    with pytest.raises(TimeoutError):
        await timed

    assert pipeline.queue == [], "the timeout reaped the queued row"
    assert pipeline.cancelled_waits == 0, "the drain outlived its caller's cancellation"

    result = await _guarded(session.probe_path(8, optimal_input=250))
    assert result == {"path_id": 8, "optimal_input": 250}


@pytest.mark.asyncio
async def test_cancelling_before_the_enqueue_leaves_the_lane_untouched() -> None:
    """A cancel while quoting enqueues no row, so nothing needs reaping.

    The block-clock read and the V3/V4 quotes sit inside the same serialized
    section, so this cancel lands before anything entered the lane. The pinned
    behavior: the lane is untouched and the next caller sees only its own row.
    """
    provider = _GatedProvider()
    pipeline = _LanePipeline()
    session = _probe_session(provider, pipeline)

    quoting = asyncio.create_task(_guarded(session.probe_path(7, optimal_input=100)))
    await _guarded(provider.entered.wait())
    quoting.cancel()
    with pytest.raises(asyncio.CancelledError):
        await quoting

    assert pipeline.enqueued == [], "no row reached the lane"

    provider.release.set()
    result = await _guarded(session.probe_path(8, optimal_input=250))
    assert result == {"path_id": 8, "optimal_input": 250}
    assert pipeline.drained == [{"path_id": 8, "optimal_input": 250}]


@pytest.mark.asyncio
async def test_an_unreapable_abandoned_probe_is_cancelled_and_the_pipeline_dropped(
    monkeypatch: pytest.MonkeyPatch,
    probe_warnings: pytest.LogCaptureFixture,
) -> None:
    """A wedge past the reap bound ends the probe, not just the session's handle on it.

    This is the one operator-visible consequence of the serialization, and it is
    a log line, not a wire change: the abandoned caller never receives a
    response. The pinned behavior, with the lane still wedged — the hold is NEVER
    released, so nothing here can pass by un-wedging the fake — after the bound
    the session refuses to reuse the executor that still holds the stranded row
    (so no later probe can receive it), logs one warning naming the bound,
    cancels the abandoned probe task and waits for its terminal state (so the
    abandoned drain is not retained behind the dropped pipeline), and the next
    probe builds a fresh pipeline and gets its own outcome.
    """
    monkeypatch.setattr("degenbot.runner.session.PROBE_REAP_TIMEOUT_S", 0.01)
    stuck = _LanePipeline(hold=asyncio.Event())  # never released: the lane stays wedged
    fresh = _LanePipeline()
    session = _probe_session(_EchoProvider(), stuck)

    async def build(inputs: Any, **kwargs: Any) -> _LanePipeline:
        return fresh

    monkeypatch.setattr("degenbot.runner.session.build_batch_executor", build)
    monkeypatch.setattr(
        "degenbot.runner.session.ExecutorInputs.from_session", lambda _session: object()
    )

    first = asyncio.create_task(_guarded(session.probe_path(7, optimal_input=100)))
    await _guarded(stuck.row_queued.wait())
    first.cancel()
    with pytest.raises(asyncio.CancelledError):
        await first

    (abandoned,) = stuck.tasks
    assert session.probe_pipeline is None, "the wedged executor was dropped"
    assert session.probe_zero_balance is None
    assert abandoned.done(), "the give-up path waited the abandoned probe to terminal"
    assert abandoned.cancelled(), "the give-up path CANCELLED the abandoned probe"
    assert abandoned not in asyncio.all_tasks(), "no pending probe task outlives it"
    assert stuck.cancelled_waits == 1, "the abandoned row was cancelled, not left running"
    assert stuck.drained == [], "the stranded row is still in the dropped lane"
    assert stuck.queue == [{"path_id": 7, "optimal_input": 100}]
    assert not any(value is abandoned for value in vars(session).values()), (
        "the session kept no handle on the abandoned task"
    )
    assert any(
        "did not drain within" in record.getMessage() for record in probe_warnings.records
    ), "the refusal is operator-visible"

    result = await _guarded(session.probe_path(8, optimal_input=250))
    assert result == {"path_id": 8, "optimal_input": 250}, "not the stranded row"
    assert fresh.drained == [{"path_id": 8, "optimal_input": 250}]
    assert session.probe_pipeline is fresh
    assert stuck.drained == [], "the stranded row never reached a later caller"
    assert stuck.queue == [{"path_id": 7, "optimal_input": 100}]


@pytest.mark.asyncio
async def test_a_second_cancel_does_not_abandon_the_terminal_cleanup(
    monkeypatch: pytest.MonkeyPatch,
    probe_warnings: pytest.LogCaptureFixture,
) -> None:
    """A second cancellation of the unwinding caller cannot skip the cleanup.

    A supervisor (or ``asyncio.timeout``) can request cancellation more than once.
    The pinned behavior: the extra request is swallowed by the cleanup's own
    waits, so the pipeline is still dropped and the abandoned probe still reaches
    a terminal state before the caller's cancellation surfaces.
    """
    monkeypatch.setattr("degenbot.runner.session.PROBE_REAP_TIMEOUT_S", 0.01)
    stuck = _LanePipeline(hold=asyncio.Event())  # never released
    fresh = _LanePipeline()
    session = _probe_session(_EchoProvider(), stuck)

    async def build(inputs: Any, **kwargs: Any) -> _LanePipeline:
        return fresh

    monkeypatch.setattr("degenbot.runner.session.build_batch_executor", build)
    monkeypatch.setattr(
        "degenbot.runner.session.ExecutorInputs.from_session", lambda _session: object()
    )

    # Deliberately not wrapped in ``_guarded``: ``asyncio.wait_for`` answers a
    # second cancellation by returning at once and leaving the inner call's
    # cleanup in flight. Cancelling the probe task itself is what lets the
    # assertions below read the state AFTER the cleanup the cancellation
    # triggered, while the settle is still gated on the deadlock guard.
    first = asyncio.create_task(session.probe_path(7, optimal_input=100))
    await _guarded(stuck.row_queued.wait())
    first.cancel()
    await _scheduler_turns()  # the give-up path is inside its bounded waits
    first.cancel()  # a SECOND request while the cleanup is already running
    await asyncio.wait({first}, timeout=DEADLOCK_GUARD_S)
    assert first.done(), "the cancelled probe settled within the deadlock guard"
    with pytest.raises(asyncio.CancelledError):
        await first

    (abandoned,) = stuck.tasks
    assert session.probe_pipeline is None, "the second cancel did not skip the drop"
    assert abandoned.done(), "the abandoned probe is terminal"
    assert abandoned.cancelled(), "the second cancel did not skip the cancellation"
    assert any("did not drain within" in record.getMessage() for record in probe_warnings.records)

    result = await _guarded(session.probe_path(8, optimal_input=250))
    assert result == {"path_id": 8, "optimal_input": 250}, "not the stranded row"
    assert fresh.drained == [{"path_id": 8, "optimal_input": 250}]
