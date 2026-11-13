"""The session's explicit execution probe: typed request in, own report out.

The probe now runs on the core's DEDICATED, correlated probe executor:

* the CONSTRUCTION is the sim half of the session executor's and nothing else —
  no signer, no nonce read, no submit provider, no relay dial, no run-mode
  stance, and no capital posture on the submitting lane (pinned here);
* the CALL is a typed `ExecutionProbeRequest` handed to the executor, whose
  reply is that call's own `ProbeReport` — so concurrent probes need no lock and
  a cancelled probe leaves nothing for the next caller to take.

The temporary `SubmissionStance.PROBE` + shared-drain `BatchExecutor.probe_path`
path, the session probe lock, and the abandoned-probe reaper are GONE; nothing
here can reinstate them without failing a test.
"""

from __future__ import annotations

import asyncio
from dataclasses import replace
from typing import TYPE_CHECKING, Any

import pytest

from degenbot.dispatch import ExecutionProbeRequest
from degenbot.runner._sim_submit import (
    ExecutorInputs,
    ProbeCapital,
    build_batch_executor,
    build_probe_executor,
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

    def payload_path_info(self, path_id: int) -> dict[str, Any] | None:
        # Path 7 is the full V2-V3-V4 route. The concurrency tests drive their
        # second probe over path 8, a single V3 hop, so its quote takes no thread
        # hop and "the second probe is in flight too" is a pure-asyncio fact.
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
        if path_id != 7:
            return None
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
        from degenbot.abi import encode_single

        return encode_single("uint256", next(self.quotes))


class _EchoProvider:
    """A provider whose quoter answers independently, so N probes can share it."""

    async def get_block(self, block: int) -> dict[str, int]:
        assert block == 12_345
        return {"base_fee_per_gas": 77, "timestamp": 1_700_000_001}

    async def call(self, target: str, data: bytes, block: int) -> bytes:
        assert block == 12_345
        from degenbot.abi import encode_single

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
        from degenbot.abi import encode_single

        return encode_single("uint256", 1_000_000)


class _FakeReport:
    """Stand-in for the FFI `ProbeReport`: the fields a caller reads back."""

    def __init__(self, **fields: Any) -> None:
        self.__dict__.update(fields)

    def __eq__(self, other: object) -> bool:
        return isinstance(other, _FakeReport) and self.__dict__ == other.__dict__

    def __repr__(self) -> str:
        return f"_FakeReport({self.__dict__})"


class _ProbeExecutor:
    """A fake probe executor: it records each call's typed request and answers it.

    A real executor correlates through the call's OWN channel, so there is no
    queue here either: each call parses its request and answers with a report
    describing it. ``hold`` parks a call inside its own ``probe`` await, which is
    how the concurrency and cancellation tests observe the lifecycle.
    """

    def __init__(self, hold: asyncio.Event | None = None, capital: str = "self_fund") -> None:
        self.hold = hold
        self.capital = capital
        self.requests: list[Any] = []
        self.holds = asyncio.Event()
        self.in_flight = 0
        self.max_in_flight = 0
        self.cancelled = 0
        self.entered = asyncio.Event()

    async def probe(self, request: Any) -> Any:
        assert isinstance(request, ExecutionProbeRequest)
        self.requests.append(request)
        self.in_flight += 1
        self.max_in_flight = max(self.max_in_flight, self.in_flight)
        self.entered.set()
        try:
            if self.hold is not None:
                await self.hold.wait()
            else:
                # A few turns stand in for the real stage's awaits, so an
                # unserialized pair genuinely overlaps.
                for _ in range(10):
                    await asyncio.sleep(0)
        except asyncio.CancelledError:
            self.cancelled += 1
            raise
        finally:
            self.in_flight -= 1
        return _FakeReport(
            capital=self.capital,
            path_id=request.path_id,
            optimal_input=request.optimal_input,
            hop_outputs=request.hop_outputs,
            consumed_inputs=request.consumed_inputs,
        )


async def _guarded(awaitable: Any) -> Any:
    """Await an in-process result under the deadlock budget (a guard, not an assertion)."""
    return await asyncio.wait_for(awaitable, DEADLOCK_GUARD_S)


def _session(provider: Any = None) -> Session:
    session = session_for_test(
        bot=_Bot(),
        engine_registry=_Registry(),
        async_w3=provider if provider is not None else _Provider(),
    )
    session.phase = SessionPhase.RUNNING
    return session


def _install(
    session: Session, executor: Any, capital: ProbeCapital = ProbeCapital.SELF_FUND
) -> None:
    """Pre-seed the session's per-posture probe executor cache."""
    session.probe_executors[capital] = executor


@pytest.mark.asyncio
async def test_probe_path_quotes_exact_inputs_into_one_typed_request() -> None:
    """The exact-input quote derivation rides the typed request, per hop."""
    executor = _ProbeExecutor()
    session = _session()
    _install(session, executor)

    report = await session.probe_path(7, optimal_input=100)

    (request,) = executor.requests
    assert request.path_id == 7
    assert request.optimal_input == 100
    assert request.hop_outputs == [1000, 20_000, 600_000]
    assert request.consumed_inputs == [100, 1000, 20_000]
    assert request.current_block == 12_345
    assert request.base_fee_next == 77
    assert request.block_timestamp == 1_700_000_001
    assert report.path_id == 7, "the caller receives ITS call's report"


@pytest.mark.asyncio
async def test_probe_path_refuses_invalid_inputs_before_any_call() -> None:
    """Input validation is the probe surface's, and it fires before the executor."""
    executor = _ProbeExecutor()
    session = _session()
    _install(session, executor)

    with pytest.raises(ValueError, match="optimal_input must be positive"):
        await session.probe_path(7, optimal_input=0)
    with pytest.raises(ValueError, match="not registered"):
        await session.probe_path(999, optimal_input=100)

    assert executor.requests == [], "no probe was dispatched"


@pytest.mark.asyncio
async def test_a_pool_that_is_not_registered_refuses_without_a_call() -> None:
    """A missing pool refuses during quoting, so no request reaches the executor."""

    class _MissEngine(_Engine):
        def pool_id_for_pool(self, family: str, address: str) -> int:
            return 999

    class _MissRegistry:
        engine = _MissEngine()

    class _EmptyBot:
        _py_bot = type("_Py", (), {"get_pool": lambda self, pool_id: None})()

    executor = _ProbeExecutor()
    session = session_for_test(
        bot=_EmptyBot(), engine_registry=_MissRegistry(), async_w3=_Provider()
    )
    session.phase = SessionPhase.RUNNING
    _install(session, executor)

    with pytest.raises(RuntimeError, match="is not registered"):
        await session.probe_path(7, optimal_input=100)

    assert executor.requests == []


# ── Construction: nothing submission-shaped crosses the boundary ──────


class _ConstructionProvider:
    """The session provider the construction boundaries read.

    ``get_transaction_count`` is the nonce seed the SUBMITTING lane reads; a
    probe must never call it, which is what ``nonce_reads`` pins.
    """

    def __init__(self) -> None:
        self.nonce_reads: list[str] = []

    async def get_transaction_count(self, address: str) -> int:
        self.nonce_reads.append(address)
        return 5

    def as_async_alloy(self) -> object:
        return object()


def _inputs(*, live: bool = True, relay_posture: Any = None) -> ExecutorInputs:
    """The construction inputs one boundary is built over.

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


def _recorder() -> tuple[list[dict[str, Any]], Any]:
    """The construction seam: record the FFI kwargs instead of building Rust."""
    calls: list[dict[str, Any]] = []

    def build(**kwargs: Any) -> str:
        calls.append(kwargs)
        return "executor"

    return calls, build


@pytest.mark.asyncio
async def test_the_probe_construction_is_non_submitting_and_reads_no_nonce(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """No signer, nonce read, submit provider, relay dial, or run-mode stance.

    Non-submission here is a MISSING FIELD set, not a flag: the probe boundary
    cannot hand the FFI a key holder, a nonce seed, a submit provider, or a
    broadcast fan-out, so there is no dry-run stance to honor and nothing to
    refuse later.
    """
    dialed: list[Any] = []

    def dial(*args: Any, **kwargs: Any) -> list[str]:
        dialed.append((args, kwargs))
        return ["relay-provider"]

    monkeypatch.setattr("degenbot.runner._sim_submit._resolve_relay_providers", dial)
    calls, build = _recorder()
    inputs = replace(_inputs(relay_posture=OFFLINE_RELAY), capital=ProbeCapital.ZERO_BALANCE)

    executor = build_probe_executor(inputs, probe_builder=build)

    assert executor == "executor"
    assert dialed == [], "a probe construction dials no relay"
    assert inputs.provider.nonce_reads == [], "a probe construction reads no nonce"
    (kwargs,) = calls
    assert set(kwargs) == {
        "context",
        "dispatcher",
        "engine",
        "provider",
        "capital",
        "deployments",
        "target_selection",
    }, f"only sim-side handles cross: {sorted(kwargs)}"
    for absent in (
        "signer",
        "operator_nonce",
        "submit_provider",
        "dry_run",
        "broadcast_providers",
    ):
        assert absent not in kwargs, f"{absent} must not reach a probe construction"
    assert kwargs["capital"] == "zero_balance", "one value, handed over as its spelling"


@pytest.mark.asyncio
async def test_a_live_production_pipeline_keeps_the_run_mode_and_the_relay_fan_out(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The submitting construction is unchanged: the run mode and the posture decide."""
    resolved: list[list[str]] = []

    async def resolve(urls: list[str], providers: Any) -> list[str]:
        resolved.append(list(urls))
        return ["relay-provider"]

    monkeypatch.setattr("degenbot.runner._sim_submit._resolve_relay_providers", resolve)
    calls, build = _recorder()

    await build_batch_executor(
        _inputs(relay_posture=OFFLINE_RELAY),
        engine_builder=build,
    )

    assert resolved == [list(OFFLINE_RELAY.relay_urls)]
    (kwargs,) = calls
    assert kwargs["dry_run"] is False, "a live production executor still submits"
    assert kwargs["broadcast_providers"] == ["relay-provider"]
    assert "capital" not in kwargs, (
        "the submitting lane names no capital posture: its own balances are the posture"
    )


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
    calls, build = _recorder()

    await build_batch_executor(
        _inputs(live=False, relay_posture=OFFLINE_RELAY), engine_builder=build
    )

    assert resolves == [list(OFFLINE_RELAY.relay_urls)]
    (kwargs,) = calls
    assert kwargs["dry_run"] is True
    assert kwargs["broadcast_providers"] == ["relay-provider"]


@pytest.mark.asyncio
async def test_the_session_builds_one_probe_executor_per_capital_posture(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """All three modes are representable, each built once, and a repeat reuses it."""
    session = _session(_EchoProvider())
    builds: list[ProbeCapital] = []

    def build(inputs: Any, **kwargs: Any) -> _ProbeExecutor:
        builds.append(inputs.capital)
        return _ProbeExecutor()

    monkeypatch.setattr("degenbot.runner.session.build_probe_executor", build)
    monkeypatch.setattr(
        "degenbot.runner.session.ExecutorInputs.from_session", lambda _session: _inputs()
    )

    for capital in (
        ProbeCapital.SELF_FUND,
        ProbeCapital.SELF_FUND,
        ProbeCapital.ZERO_BALANCE,
        ProbeCapital.PRODUCTION,
    ):
        await session.probe_path(7, optimal_input=100, capital=capital)
        assert session.probe_executors[capital] is not None

    assert builds == [
        ProbeCapital.SELF_FUND,
        ProbeCapital.ZERO_BALANCE,
        ProbeCapital.PRODUCTION,
    ], "each mode builds once and a repeat reuses the executor it built"
    assert set(session.probe_executors) == {
        ProbeCapital.SELF_FUND,
        ProbeCapital.ZERO_BALANCE,
        ProbeCapital.PRODUCTION,
    }


# ── Correlation: concurrent, cancelled, and mode-swapped calls ────────


@pytest.mark.asyncio
async def test_concurrent_probes_overlap_and_each_owns_its_report() -> None:
    """No lock: two probes are in flight TOGETHER and each gets its own report.

    The retired serialization existed only because the shared drain could hand a
    caller someone else's row. With a per-call channel there is nothing to take,
    so the session lets the calls overlap — and each caller still receives the
    path and amount it asked for.
    """
    hold = asyncio.Event()
    executor = _ProbeExecutor(hold=hold)
    session = _session(_EchoProvider())
    _install(session, executor)

    first = asyncio.create_task(_guarded(session.probe_path(7, optimal_input=100)))
    await _guarded(executor.entered.wait())
    second = asyncio.create_task(_guarded(session.probe_path(8, optimal_input=250)))
    for _ in range(10):
        await asyncio.sleep(0)

    assert executor.in_flight == 2, "both probes are in flight at once (no session lock)"
    assert executor.max_in_flight == 2

    hold.set()
    first_report = await first
    second_report = await second

    assert (first_report.path_id, first_report.optimal_input) == (7, 100)
    assert (second_report.path_id, second_report.optimal_input) == (8, 250)
    assert [request.path_id for request in executor.requests] == [7, 8]


@pytest.mark.asyncio
async def test_a_cancelled_probe_aborts_its_own_call_and_leaves_nothing_behind() -> None:
    """A cancelled caller cancels ITS call; the next probe still gets its own report.

    Nothing is queued and no task is retained: the call's await is the whole
    operation, so cancelling it aborts the stage instead of abandoning a result
    for a successor — the reason the session reaper is gone.
    """
    hold = asyncio.Event()
    executor = _ProbeExecutor(hold=hold)
    session = _session(_EchoProvider())
    _install(session, executor)

    first = asyncio.create_task(_guarded(session.probe_path(7, optimal_input=100)))
    await _guarded(executor.entered.wait())
    first.cancel()
    with pytest.raises(asyncio.CancelledError):
        await first

    assert executor.cancelled == 1, "the cancelled call's own probe await was cancelled"
    assert executor.in_flight == 0, "no call outlives its caller"
    assert session.probe_executors[ProbeCapital.SELF_FUND] is executor, (
        "the executor is still reusable: nothing was stranded"
    )

    hold.set()  # the fake parks every call; release it so the next one can settle
    result = await _guarded(session.probe_path(8, optimal_input=250))
    assert (result.path_id, result.optimal_input) == (8, 250), "not the cancelled call's row"


@pytest.mark.asyncio
async def test_a_capital_swap_during_an_in_flight_probe_strands_nothing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A second posture builds its own executor; the in-flight call keeps its own.

    The retired lock also protected the executor SWAP. It is unnecessary now: the
    in-flight call holds its own executor reference (and its own channel), so a
    new posture's construction cannot disturb it.
    """
    hold = asyncio.Event()
    self_funded = _ProbeExecutor(hold=hold)
    flash_funded = _ProbeExecutor()
    session = _session(_EchoProvider())
    _install(session, self_funded)
    builds: list[ProbeCapital] = []

    def build(inputs: Any, **kwargs: Any) -> _ProbeExecutor:
        builds.append(inputs.capital)
        return flash_funded

    monkeypatch.setattr("degenbot.runner.session.build_probe_executor", build)
    monkeypatch.setattr(
        "degenbot.runner.session.ExecutorInputs.from_session", lambda _session: _inputs()
    )

    funded = asyncio.create_task(_guarded(session.probe_path(7, optimal_input=100)))
    await _guarded(self_funded.entered.wait())

    zero = await _guarded(
        session.probe_path(8, optimal_input=250, capital=ProbeCapital.ZERO_BALANCE)
    )
    assert (zero.path_id, zero.optimal_input) == (8, 250)
    assert builds == [ProbeCapital.ZERO_BALANCE]
    assert session.probe_executors[ProbeCapital.ZERO_BALANCE] is flash_funded

    hold.set()
    self_funded_report = await funded
    assert (self_funded_report.path_id, self_funded_report.optimal_input) == (7, 100)
    assert session.probe_executors[ProbeCapital.SELF_FUND] is self_funded, (
        "the in-flight posture's executor was not replaced under it"
    )


@pytest.mark.asyncio
async def test_a_cancel_while_quoting_never_reaches_the_executor() -> None:
    """A cancel during quoting dispatches no probe, so there is nothing to abort."""
    provider = _GatedProvider()
    executor = _ProbeExecutor()
    session = _session(provider)
    _install(session, executor)

    quoting = asyncio.create_task(_guarded(session.probe_path(7, optimal_input=100)))
    await _guarded(provider.entered.wait())
    quoting.cancel()
    with pytest.raises(asyncio.CancelledError):
        await quoting

    assert executor.requests == [], "no request reached the executor"

    provider.release.set()
    result = await _guarded(session.probe_path(8, optimal_input=250))
    assert (result.path_id, result.optimal_input) == (8, 250)
    assert [request.path_id for request in executor.requests] == [8]
