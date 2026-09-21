"""The at-most-once verify-claim policy behind TWO concurrency adapters.

DMZ3DD consolidation (NRHEAC): the leader/peer/release-on-failure verify
claim existed THREE times — asyncio.Future twins inside
``EngineRegistry.register_v3_pool`` / ``register_v4_pool`` and a
threading.Event twin in the runner's ``_SeatVerifyClaims``. This module owns
the claim RECORD, the POLICY, and the adapters that bind it to genuinely
different concurrency primitives; the consumers shrink to thin callers
(engine_registry: one ``_register_with_claim`` over the asyncio adapter;
build_paths: the ``_SeatVerifyClaims`` threading shell).

The seam is REAL by the two-adapter rule: an ``asyncio.Future`` on the
operator's single event loop and a ``threading.Event`` on the fleet seats
(plain threads with no running loop, PRG-5) are two different primitives —
not one primitive with two skins — so the honest fold is ONE policy behind
TWO thin adapters. The adapters own ONLY the await/wake mechanics of their
primitive (wake construction, settlement, peer park); every policy decision
is made by :class:`VerifyClaims`. A wrong-shape mechanic call (e.g. parking
an asyncio wake) hits the base-class guard and raises ``RuntimeError``
loudly instead of silently no-oping.

THE POLICY (stated once, here — formerly restated in three copies):

* **claim-if-absent (leader)** — the first caller for a claim key registers
  a fresh claim (contiguous check+insert: atomic on the loop for the asyncio
  shape, lock-guarded for the seat-thread shape) and runs the work.
* **await-if-present (peer)** — a caller that sees a live claim waits on the
  SAME claim instead of re-running the work, so a pool's verify lifecycle
  runs at most once per live claim window. A peer re-raises the leader's
  EXACT exception instance on failure; asyncio peers additionally receive
  the leader's settled result value on success.
* **release-on-failure (and on success)** — when the leader finishes,
  success or failure, the claim is released by identity-checked eviction, so
  a failed lifecycle is retriable: a LATER caller re-claims and re-runs it.
  The identity check never evicts a newer claim that a retry may already
  have registered.
* **unretrieved-exception hygiene (asyncio)** — on leader failure the claim
  future's exception is retrieved immediately after ``set_exception``: the
  failed claim is released with no sibling waiter in the common case, and a
  discarded future whose exception was never retrieved would log "Future
  exception was never retrieved" at GC. A sibling that already awaited the
  claim still receives the exception regardless (retrieval does not consume
  it). Cancellation is not an error publication — a cancelled leader cancels
  the claim future instead of ``set_exception``.

ADR-022 boundary untouched: the verify choreography (quarantine →
seed-verify → drain+pin → post-drain-verify → set_live) stays core-owned;
this module is transport dedup only — it never calls the engine.
"""

from __future__ import annotations

import asyncio
import threading
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, cast, override

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Generator
    from typing import Any

__all__ = [
    "AsyncioFutureWake",
    "ClaimRecord",
    "ClaimWake",
    "ThreadEventWake",
    "VerifyClaims",
]


@dataclass
class ClaimRecord[W]:
    """One live verify-claim window (the DMZ3DD claim record).

    ``wake`` is the adapter's concurrency primitive — the object peers wait
    on and the leader settles (``asyncio.Future`` / ``threading.Event``).
    ``error`` is the published leader failure for wakes that cannot carry a
    payload (the Event shape; the Future shape carries the failure in the
    wake itself). Awaiting the record directly is defined for the asyncio
    shape — it delegates to the wake Future (the registry's racing-sibling
    observers ``await`` the in-flight table entry directly).

    ``parked`` is the peer-park observable: a peer (never the leader) sets it
    as it enters the park/peer-wait, so an observer holding the live claim
    record can wait for "the peer is parked" instead of inferring it from
    wall-clock timing.
    """

    wake: W
    error: BaseException | None = None
    parked: threading.Event = field(default_factory=threading.Event)

    def __await__(self) -> Generator[Any, Any, object]:
        # asyncio shape only — the threading Event has no __await__ (its
        # peers park via the wake adapter instead of awaiting).
        return cast("asyncio.Future[object]", self.wake).__await__()


class ClaimWake[W, V](ABC):
    """The await/wake mechanics of ONE concurrency primitive (adapter seam).

    Implementations own ONLY their primitive's mechanics — wake
    construction, settlement, peer park. Every policy decision (claim-if-
    absent, await-if-present, release-on-failure, hygiene) is made by
    :class:`VerifyClaims`.

    The mechanics each primitive does NOT support keep the base-class guard
    (a loud ``RuntimeError`` naming the misused adapter) — the Event shape
    carries no result to settle and never awaits; the Future shape's peers
    `await` instead of parking a thread and need no separate done-wake.
    """

    @abstractmethod
    def new_wake(self) -> W:
        """Create the wake primitive for a fresh claim window."""

    @abstractmethod
    def settle_error(self, record: ClaimRecord[W], exc: BaseException) -> None:
        """Publish the leader's failure to the wake (peers re-raise it)."""

    def settle_ok(self, record: ClaimRecord[W], value: V) -> None:
        """Publish the leader's success value (asyncio shapes only).

        Raises:
            RuntimeError: Always — this concrete shape does not carry a
                result value; the guard is the loud wrong-shape refusal.

        """
        msg = (
            f"{type(self).__name__}.settle_ok: this wake shape carries no result "
            f"value — success wakes via its own mechanics (value discarded: "
            f"{value!r}, wake: {type(record.wake).__name__})"
        )
        raise RuntimeError(msg)

    def wake_done(self, record: ClaimRecord[W]) -> None:
        """Wake peers without a payload (Event shapes only).

        Raises:
            RuntimeError: Always — this concrete shape settles/wakes through
                its own mechanics; the guard is the loud wrong-shape refusal.

        """
        msg = (
            f"{type(self).__name__}.wake_done: this wake shape settles/wakes "
            f"through settle_ok/settle_error — no separate done-wake (wake: "
            f"{type(record.wake).__name__})"
        )
        raise RuntimeError(msg)

    def park(self, record: ClaimRecord[W]) -> None:
        """Block the calling thread until settlement (Event shapes).

        Raises:
            RuntimeError: Always — this concrete shape waits via
                `peer_wait`; the guard is the loud wrong-shape refusal.

        """
        msg = (
            f"{type(self).__name__}.park: this wake shape has no sync park — "
            f"peers wait via peer_wait (wake: {type(record.wake).__name__})"
        )
        raise RuntimeError(msg)

    async def peer_wait(self, record: ClaimRecord[W]) -> V:
        """Await the leader's settlement (asyncio shapes only).

        Raises:
            RuntimeError: Always — this concrete shape parks threads via
                `park`; the guard is the loud wrong-shape refusal.

        """
        msg = (
            f"{type(self).__name__}.peer_wait: this wake shape parks threads "
            f"via park — it never awaits (wake: {type(record.wake).__name__})"
        )
        raise RuntimeError(msg)


class AsyncioFutureWake(ClaimWake[asyncio.Future[int], int]):
    """``asyncio.Future`` wake mechanics (the loop-bound operator surface).

    The wake is a loop-bound ``asyncio.Future`` created on the running loop
    at claim time; peers await it (result → the leader's settled value,
    ``set_exception`` → the leader's exact exception, ``cancel`` →
    ``CancelledError``). The settlement mechanics include the unretrieved-
    exception hygiene band — retrieved once, here.
    """

    @override
    def new_wake(self) -> asyncio.Future[int]:
        return asyncio.get_running_loop().create_future()

    @override
    def settle_error(self, record: ClaimRecord[asyncio.Future[int]], exc: BaseException) -> None:
        if isinstance(exc, asyncio.CancelledError):
            # Cancellation is not an error publication: cancel the claim so
            # awaiting peers see CancelledError (the retired twins' branch).
            record.wake.cancel()
        else:
            record.wake.set_exception(exc)
            # Unretrieved-exception hygiene (module docstring): the failed
            # claim is released with no sibling waiter in the common case —
            # mark the exception retrieved so the discarded future doesn't
            # log 'Future exception was never retrieved' at GC. A sibling
            # that already awaited still receives the exception regardless.
            record.wake.exception()

    @override
    def settle_ok(self, record: ClaimRecord[asyncio.Future[int]], value: int) -> None:
        record.wake.set_result(value)

    @override
    async def peer_wait(self, record: ClaimRecord[asyncio.Future[int]]) -> int:
        return await record.wake

    # park / wake_done keep the base-class wrong-shape guards.


class ThreadEventWake(ClaimWake[threading.Event, None]):
    """``threading.Event`` wake mechanics (the seat-thread twin, PRG-5).

    The wake is a ``threading.Event``; it carries no payload, so the
    leader's failure is published on the record's ``error`` slot and peers
    re-raise it verbatim after `wait()` returns. Success wakes peers
    without a value — the seat-thread shape shares completion, not results
    (the pipeline call sites discard `run()`'s return).

    `settle_ok` / `peer_wait` keep the base-class wrong-shape guards.
    """

    @override
    def new_wake(self) -> threading.Event:
        return threading.Event()

    @override
    def settle_error(self, record: ClaimRecord[threading.Event], exc: BaseException) -> None:
        record.error = exc

    @override
    def wake_done(self, record: ClaimRecord[threading.Event]) -> None:
        record.wake.set()

    @override
    def park(self, record: ClaimRecord[threading.Event]) -> None:
        record.wake.wait()
        if record.error is not None:
            # The peer re-raises the leader's EXACT exception instance.
            raise record.error


class VerifyClaims[W, V]:
    """The at-most-once verify-claim policy (DMZ3DD) over a wake adapter.

    See the module docstring — the policy (claim-if-absent / await-if-
    present / release-on-failure + the asyncio unretrieved-exception
    hygiene) is stated ONCE there and implemented here, shared by both
    concurrency shapes. The claim table takes one lock for both shapes:
    required by the seat-thread shape, uncontended (unobservable) on the
    single loop — it is never held across an await or a park.

    `claims` may be supplied by the consumer when the table must live at
    a stable attribute (the registry's ``_vN_inflight`` tables are read
    directly by the racing-sibling observers).
    """

    def __init__(
        self,
        wake: ClaimWake[W, V],
        claims: dict[str, ClaimRecord[W]] | None = None,
    ) -> None:
        self._wake = wake
        self._lock = threading.Lock()
        self._claims: dict[str, ClaimRecord[W]] = {} if claims is None else claims

    def _acquire(self, claim_key: str) -> tuple[ClaimRecord[W], bool]:
        """Claim-if-absent / await-if-present — the policy's first decision.

        The check+insert is contiguous under the table lock (atomic on the
        loop; race-free on the seats).

        Returns:
            The live claim record and whether the caller is the leader.

        """
        with self._lock:
            record = self._claims.get(claim_key)
            if record is None:
                record = ClaimRecord(wake=self._wake.new_wake())
                self._claims[claim_key] = record
                return record, True
            return record, False

    def _release(self, claim_key: str, record: ClaimRecord[W]) -> None:
        """Release-on-failure (and on success) — identity-checked eviction.

        Only the leader that owns the live entry evicts it, so a failed
        claim stays retriable by a LATER caller without ever clobbering a
        newer claim that a retry may already have registered.
        """
        with self._lock:
            if self._claims.get(claim_key) is record:
                del self._claims[claim_key]

    async def run(self, claim_key: str, run: Callable[[], Awaitable[V]]) -> V:
        """Run `run()` at most once per live claim window (asyncio shape).

        Peers await the leader's claim and receive its settled value — or
        the leader's exact exception. On leader failure (including
        cancellation) the claim is released so a later caller retries.

        Returns:
            The leader's `run()` result (peers: the settled value).

        """
        record, leader = self._acquire(claim_key)
        if not leader:
            record.parked.set()
            return await self._wake.peer_wait(record)
        try:
            value = await run()
        except BaseException as exc:
            self._wake.settle_error(record, exc)
            raise
        finally:
            self._release(claim_key, record)
        self._wake.settle_ok(record, value)
        return value

    def run_sync(self, claim_key: str, run: Callable[[], object]) -> None:
        """Run `run()` at most once per live claim window (threading shape).

        Peers park on the claim's event; a parked peer re-raises the
        leader's exact exception (success shares completion, not a value —
        the seat-thread call sites discard `run()`'s return). On leader
        failure the claim is released so a later caller retries.
        """
        record, leader = self._acquire(claim_key)
        if not leader:
            record.parked.set()
            self._wake.park(record)
            return
        try:
            run()
        except BaseException as exc:
            self._wake.settle_error(record, exc)
            raise
        finally:
            self._wake.wake_done(record)
            self._release(claim_key, record)
