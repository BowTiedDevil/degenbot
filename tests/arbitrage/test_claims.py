"""NRHEAC: the ONE claims module behind the two verify-claim adapters.

Drives the DMZ3DD claim-policy matrix (leader success; leader failure →
release → later peer retries; concurrent peers single-winner;
unretrieved-exception hygiene) through BOTH adapters —
:class:`AsyncioFutureWake` (the loop-bound operator surface) and
:class:`ThreadEventWake` (the seat-thread twin, PRG-5) — against
:class:`VerifyClaims`, the module-stated policy.

RED story: pre-fold, the V3/V4 asyncio twins were byte-identical in policy
(no divergence to pin), so the fold's real risk is the duplicated-policy
deletion kind — a future "simplification" of the one surviving copy
drifting. The pin: :func:`_async_policy_properties` states the policy
matrix once as violations; the good adapters satisfy it exactly, and a
simulated drifted copy (the retired dance minus release-on-failure, the
most plausible deletion) fails it on exactly the release property —
:func:`test_policy_pin_catches_a_drifted_copy`. The hygiene property's
detection power is proven by a positive control
(:func:`test_hygiene_pin_positive_control`: a genuinely unretrieved
exception IS observable by the same loop handler).
"""

from __future__ import annotations

import asyncio
import gc
import threading
from typing import TYPE_CHECKING

import pytest

from degenbot.arbitrage._claims import (
    AsyncioFutureWake,
    ClaimRecord,
    ThreadEventWake,
    VerifyClaims,
)

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable


class _DriftedNeverReleases:
    """A simulated drifted copy: the retired asyncio dance minus the
    release-on-failure rule (a plausible "simplification").

    Peer settlement (set_exception + the hygiene retrieval) is kept so
    parked peers still fail fast — the drift is ONLY the missing release,
    the exact deletion the one-copy fold risks.
    """

    def __init__(self) -> None:
        self._claims: dict[str, asyncio.Future[int]] = {}

    async def run(self, key: str, run: Callable[[], Awaitable[int]]) -> int:
        if key in self._claims:
            return await self._claims[key]
        claim = asyncio.get_running_loop().create_future()
        self._claims[key] = claim
        try:
            value = await run()
        except BaseException as exc:
            claim.set_exception(exc)
            claim.exception()  # hygiene kept — the drift is release-only
            raise
        self._claims.pop(key, None)
        claim.set_result(value)
        return value


async def _async_policy_properties(make_claims: Callable[[], object]) -> list[str]:
    """Drive the claim-policy matrix through an asyncio-shape claims object.

    Returns the violated property tags (empty == the object obeys the
    module-stated policy: claim-if-absent / await-if-present /
    release-on-failure + the unretrieved-exception hygiene).
    """
    violations: list[str] = []

    # P1 leader success: run once; a peer shares the settled value; the
    # release-on-success closes the window (a later caller re-runs — the
    # consumers' key caches are the cross-window dedup, a different layer).
    claims = make_claims()
    calls: list[int] = []

    async def ok_run() -> int:
        calls.append(1)
        await asyncio.sleep(0)  # yield: the peer gets its turn to park
        return 7

    leader, peer = await asyncio.gather(
        claims.run("k1", ok_run),
        claims.run("k1", ok_run),
    )
    if calls != [1] or leader != 7 or peer != 7:
        violations.append("P1: leader runs once; peers receive the settled value")
    await claims.run("k1", ok_run)
    if calls != [1, 1]:
        violations.append("P1b: a settled claim is released — a later caller re-runs")

    # P2 leader failure: a peer parked on the live claim receives the
    # leader's EXACT exception instance and never re-runs; the failed claim
    # is released so a LATER caller retries. Deterministic choreography: the
    # leader holds the claim until the probe has CONFIRMED the peer parked
    # (peer-started event + one loop tick guarantees the peer reached its
    # first suspension = parked on the claim), then fails on cue — no
    # timing races under load.
    claims = make_claims()
    calls = []
    boom = RuntimeError("leader fails")
    fail_cue = asyncio.Event()
    peer_started = asyncio.Event()

    async def failing_run() -> int:
        calls.append(1)
        await fail_cue.wait()
        raise boom

    leader_task = asyncio.create_task(claims.run("k2", failing_run))
    live_claims = claims  # stable closure alias (the probe reassigns per section)

    async def peer_call() -> int:
        peer_started.set()
        return await live_claims.run("k2", failing_run)

    peer_task = asyncio.create_task(peer_call())
    await peer_started.wait()  # the peer task has claimed (parked on the live claim)
    await asyncio.sleep(0)  # one tick: the peer is at its first suspension
    if calls != [1]:
        violations.append("P2a: a parked peer must not re-run the lifecycle")
    fail_cue.set()  # the leader now fails; the parked peer inherits the error
    outcomes = await asyncio.gather(leader_task, peer_task, return_exceptions=True)
    if outcomes[0] is not boom or outcomes[1] is not boom:
        violations.append("P2b: the peer receives the leader's EXACT exception instance")
    try:
        retry = await claims.run("k2", ok_run)
        if calls != [1, 1] or retry != 7:
            violations.append("P2c: the failed claim is released — a later caller retries")
    except RuntimeError:
        violations.append("P2c: the failed claim is released — a later caller retries")

    # P3 concurrent peers (n>2): single winner.
    claims = make_claims()
    calls = []

    async def slow_ok() -> int:
        calls.append(1)
        await asyncio.sleep(0)  # yield: the other 7 coroutines park on the claim
        return 3

    results = await asyncio.gather(*(claims.run("k3", slow_ok) for _ in range(8)))
    if calls != [1] or results != [3] * 8:
        violations.append("P3: N concurrent peers — the lifecycle runs once, all get the value")

    # P4 unretrieved-exception hygiene: a failed leader with no sibling
    # waiter must not leak an unretrieved-exception record at GC.
    handler_records: list[dict] = []
    asyncio.get_running_loop().set_exception_handler(
        lambda _loop, context: handler_records.append(context),
    )
    claims = make_claims()
    probe_msg = "hygiene probe"

    async def hygiene_failure() -> int:
        await asyncio.sleep(0)
        raise RuntimeError(probe_msg)

    try:
        await claims.run("k4", hygiene_failure)
        violations.append("P4: leader failure must propagate")
    except RuntimeError:
        pass
    del claims
    gc.collect()
    if any(
        record.get("message") == "Future exception was never retrieved"
        for record in handler_records
    ):
        violations.append("P4: a failed leader with no peer must not leak an unretrieved exception")

    return violations


async def test_policy_matrix_holds_for_the_asyncio_adapter() -> None:
    """The asyncio.Future adapter satisfies the full pinned policy matrix."""
    assert await _async_policy_properties(lambda: VerifyClaims(AsyncioFutureWake())) == []


async def test_policy_pin_catches_a_drifted_copy() -> None:
    """RED story: the pin catches one old copy drifting.

    The drifted copy (retired dance minus release-on-failure) violates
    EXACTLY the release property — a parked peer still fails fast (its
    settlement is kept) but the failed claim is never released, so a later
    caller can never retry.
    """
    violations = await _async_policy_properties(_DriftedNeverReleases)
    assert violations == ["P2c: the failed claim is released — a later caller retries"]


async def test_hygiene_pin_positive_control() -> None:
    """Detection power: a genuinely unretrieved exception IS observable by
    the loop handler the P4 hygiene property uses — if the retrieval band
    ever regressed, the probe above would see it."""
    await asyncio.sleep(0)
    records: list[dict] = []
    loop = asyncio.get_running_loop()
    loop.set_exception_handler(lambda _loop, context: records.append(context))
    claim = loop.create_future()
    claim.set_exception(RuntimeError("never retrieved"))
    del claim
    gc.collect()
    assert any(
        record.get("message") == "Future exception was never retrieved" for record in records
    ), "the hygiene probe must be able to see a real unretrieved exception"


async def test_asyncio_adapter_cancelled_leader_cancels_claim_and_releases() -> None:
    """A cancelled leader cancels the claim (not an error publication); the
    parked peer sees CancelledError and the claim is released for retry."""
    claims = VerifyClaims(AsyncioFutureWake())
    started = asyncio.Event()
    release_leader = asyncio.Event()
    peer_started = asyncio.Event()

    async def hang() -> int:
        started.set()
        await release_leader.wait()  # never set: the leader hangs until cancelled
        return 1

    async def peer_call() -> int:
        peer_started.set()
        return await claims.run("k", hang)

    leader = asyncio.create_task(claims.run("k", hang))
    await started.wait()
    peer = asyncio.create_task(peer_call())
    await peer_started.wait()  # the peer has claimed (parked on the live claim)
    await asyncio.sleep(0)  # one tick: the peer is at its first suspension
    leader.cancel()
    with pytest.raises(asyncio.CancelledError):
        await leader
    with pytest.raises(asyncio.CancelledError):
        await peer
    # release-on-failure: the cancelled claim is gone; a later caller re-runs.
    calls: list[int] = []

    async def ok() -> int:
        await asyncio.sleep(0)
        calls.append(1)
        return 2

    assert await claims.run("k", ok) == 2
    assert calls == [1]


async def test_claim_record_await_is_the_peer_wait() -> None:
    """Awaiting an in-flight table entry directly (the registry's
    racing-sibling observer surface) delegates to the wake Future."""
    claims = VerifyClaims(AsyncioFutureWake())
    started = asyncio.Event()

    async def ok() -> int:
        started.set()
        await asyncio.sleep(0)  # yield: the leader is at its first suspension
        return 11

    leader = asyncio.create_task(claims.run("k", ok))
    await started.wait()
    (key, record) = next(iter(claims._claims.items()))
    assert key == "k"
    assert await record == 11  # the direct await-inflight-entry surface
    assert await leader == 11


async def test_release_never_clobbers_a_newer_claim() -> None:
    """The identity-checked release only evicts the claim that owns the
    entry — a stale leader's release never clobbers a newer claim a retry
    may already have registered."""
    await asyncio.sleep(0)
    claims = VerifyClaims(AsyncioFutureWake())
    record, leader = claims._acquire("k")
    assert leader is True
    claims._release("k", record)  # the owner's release lands
    record2, leader2 = claims._acquire("k")
    assert leader2 is True
    assert record2 is not record
    claims._release("k", record)  # a STALE release must not evict the newer claim
    assert claims._claims.get("k") is record2
    claims._release("k", record2)  # the new owner's release lands
    assert "k" not in claims._claims


async def test_wrong_shape_mechanics_fail_loudly() -> None:
    """Adapters own only their primitive's mechanics — a wrong-shape call
    raises the base-class guard loudly (never a silent no-op)."""
    asyncio_wake = AsyncioFutureWake()
    thread_wake = ThreadEventWake()

    claims = VerifyClaims(asyncio_wake)
    started = asyncio.Event()

    async def hang() -> int:
        started.set()
        await asyncio.sleep(0)  # yield: the claim stays live while the guard fires
        return 1

    leader = asyncio.create_task(claims.run("k", hang))
    await started.wait()
    with pytest.raises(RuntimeError, match="no sync park"):
        claims.run_sync("k", lambda: None)  # a peer would have to park a thread
    await leader

    with pytest.raises(RuntimeError, match="carries no result"):
        thread_wake.settle_ok(ClaimRecord(wake=threading.Event()), None)
    with pytest.raises(RuntimeError, match="never awaits"):
        await thread_wake.peer_wait(ClaimRecord(wake=threading.Event()))
    with pytest.raises(RuntimeError, match="no separate done-wake"):
        asyncio_wake.wake_done(ClaimRecord(wake=asyncio.get_running_loop().create_future()))


def _run_capture(claims, key: str, run) -> BaseException | None:
    """Run `run_sync` on a seat thread, capturing the propagated error."""
    try:
        claims.run_sync(key, run)
    except RuntimeError as exc:
        return exc
    return None


class _SeatDance:
    """One deterministic seat-thread claim race (the thread-adapter dance).

    ``leader_peer`` starts the leader and peer seat threads and returns only once
    the peer is parked on the SAME live claim — observed through the claim
    record's ``parked`` signal, which ``VerifyClaims`` sets as a peer parks.
    The leader's body holds the claim open on :attr:`release`; every
    ``wait(5)``/``join(5)`` below is a bounded failure timeout, never a
    sequencing sleep.
    """

    def __init__(self, claims: VerifyClaims, key: str) -> None:
        self._claims = claims
        self._key = key
        self.started = threading.Event()
        self.release = threading.Event()
        self.outcomes: dict[str, BaseException | None] = {}
        self._threads: list[threading.Thread] = []

    def _spawn(self, target: Callable[[], None]) -> None:
        thread = threading.Thread(target=target)
        thread.start()
        self._threads.append(thread)

    def leader_peer(self, work: Callable[[], None]) -> None:
        """Start one leader + peer; return once the peer is parked."""

        def leader_run() -> None:
            self.outcomes["leader"] = _run_capture(self._claims, self._key, work)

        def peer_run() -> None:
            assert self.started.wait(5), "harness timeout: leader never claimed"
            self.outcomes["peer"] = _run_capture(self._claims, self._key, work)

        self._spawn(leader_run)
        self._spawn(peer_run)
        assert self.started.wait(5), "harness timeout: leader never claimed"
        record = self._claims._claims[self._key]
        assert record.parked.wait(5), "harness timeout: peer never parked"

    def race(self, units: list[Callable[[], None]]) -> None:
        """Start every unit thread and join them all (bounded)."""
        for unit in units:
            self._spawn(unit)
        self.join()

    def join(self) -> None:
        for thread in self._threads:
            thread.join(5)


@pytest.fixture
def seat_dance() -> Callable[[VerifyClaims, str], _SeatDance]:
    """Factory for :class:`_SeatDance` — the ONE threaded-dance definition."""

    def make(claims: VerifyClaims, key: str) -> _SeatDance:
        return _SeatDance(claims, key)

    return make


def test_peer_park_is_observable_on_the_live_claim_record() -> None:
    """Pin the peer-park seam: a parked peer sets the live claim record's
    ``parked`` event and the leader never does, so the threaded dance can
    sequence on observed claim state instead of a wall-clock proxy."""
    claims = VerifyClaims(ThreadEventWake())
    claimed = threading.Event()
    release = threading.Event()

    def hold() -> None:
        claimed.set()
        assert release.wait(5), "harness timeout"

    leader = threading.Thread(target=lambda: _run_capture(claims, "k", hold))
    leader.start()
    assert claimed.wait(5), "harness timeout: leader never claimed"
    record = claims._claims["k"]
    assert not record.parked.is_set(), "only a peer parks — the leader must not set it"

    peer = threading.Thread(target=lambda: _run_capture(claims, "k", hold))
    peer.start()
    assert record.parked.wait(5), "a parked peer must be observable on the live claim record"
    release.set()
    leader.join(5)
    peer.join(5)


def test_thread_adapter_leader_success_peer_parks_and_window_closes(
    seat_dance: Callable[[VerifyClaims, str], _SeatDance],
) -> None:
    """Leader success: the parked peer waits it out (no re-run) and shares
    completion — not a value; the window closes for a later unit."""
    claims = VerifyClaims(ThreadEventWake())
    dance = seat_dance(claims, "k")
    calls: list[str] = []

    def work() -> None:
        calls.append("run")
        dance.started.set()
        assert dance.release.wait(5), "harness timeout"

    dance.leader_peer(work)
    assert calls == ["run"], "a parked peer must not re-run the lifecycle"
    dance.release.set()
    dance.join()
    assert calls == ["run"]
    assert dance.outcomes == {"leader": None, "peer": None}  # completion, not a value
    # release-on-success closes the window: a later unit re-runs (the
    # consumers' key caches are the cross-window dedup, a different layer).
    rechecked: list[str] = []
    assert _run_capture(claims, "k", lambda: rechecked.append("run2")) is None
    assert rechecked == ["run2"]


def test_thread_adapter_leader_failure_peer_exact_exception_then_retry(
    seat_dance: Callable[[VerifyClaims, str], _SeatDance],
) -> None:
    """Leader failure: the parked peer re-raises the leader's EXACT exception
    instance; the failed claim is released so a LATER unit retries."""
    claims = VerifyClaims(ThreadEventWake())
    dance = seat_dance(claims, "k")
    calls: list[str] = []
    boom = RuntimeError("seat leader fails")

    def failing() -> None:
        calls.append("run")
        dance.started.set()
        assert dance.release.wait(5), "harness timeout"
        raise boom

    dance.leader_peer(failing)
    assert calls == ["run"]
    dance.release.set()
    dance.join()
    assert calls == ["run"], "the parked peer re-raised instead of re-running"
    assert dance.outcomes["leader"] is boom
    assert dance.outcomes["peer"] is boom, (
        "the peer re-raises the leader's EXACT exception instance"
    )
    # release-on-failure: the failed claim is gone; a LATER unit retries.
    retried: list[str] = []

    def retry() -> None:
        retried.append("retry")

    assert _run_capture(claims, "k", retry) is None
    assert retried == ["retry"]


def test_thread_adapter_concurrent_peers_single_winner(
    seat_dance: Callable[[VerifyClaims, str], _SeatDance],
) -> None:
    """N>2 concurrent seat threads racing one claim: single winner, every
    peer shares the outcome.

    Deterministic white-box harness: an instance-local _acquire wrap counts
    arrivals, and the leader's slow() does not return until all 8 threads have
    resolved _acquire. Without that, a thread dispatched after the leader's
    release resolves _acquire too late and legitimately re-claims — the
    test-side race this pins out.
    """
    claims = VerifyClaims(ThreadEventWake())
    dance = seat_dance(claims, "k")
    calls: list[int] = []
    barrier = threading.Barrier(8, timeout=5)

    arrivals = 0
    arrivals_cv = threading.Condition()
    original_acquire = claims._acquire

    def counted_acquire(claim_key: str):
        # Count AFTER the original returns: an arrival is only resolved once
        # the caller holds its record (leader or peer), so 8 arrivals means no
        # thread can still be racing to acquire past the leader's release.
        nonlocal arrivals
        record = original_acquire(claim_key)
        with arrivals_cv:
            arrivals += 1
            arrivals_cv.notify_all()
        return record

    claims._acquire = counted_acquire  # instance-local wrap; no global patch

    def slow() -> None:
        calls.append(1)
        with arrivals_cv:
            assert arrivals_cv.wait_for(lambda: arrivals == 8, timeout=5), (
                "harness timeout: not all threads resolved _acquire"
            )

    outcomes: list[BaseException | None] = []
    lock = threading.Lock()

    def unit() -> None:
        barrier.wait()
        outcome = _run_capture(claims, "k", slow)
        with lock:
            outcomes.append(outcome)

    dance.race([unit for _ in range(8)])
    assert calls == [1], "N concurrent seat threads must run the lifecycle once"
    assert outcomes == [None] * 8
