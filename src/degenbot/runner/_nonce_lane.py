"""Private-lane operator-nonce reservations for relay-postured submission.

Design note — the strategy and its state machine (the why-only record; the
acceptance criteria live as the named tests in ``tests/dispatch/test_nonce_lane.py``):

The bug: under a relay posture (``DEGENBOT_SUBMIT_RELAY_URLS``), broadcasts go
to private builder endpoints but the operator nonce is read from the LOCAL
node at submit time. Relay-pending txs are invisible to that pending read, so
each subsequent relay batch re-claims the same base nonce and replaces or
collides with the still-pending relay tx.

Chosen strategy — (a) an in-flight reservation OVERLAY at the Python submit
seam. :class:`NonceLane` keeps the reservations made for relay batches whose
nonces the local node cannot see, and the base handed to the Rust submit leaf
becomes ``max(local_read, last_reserved_top)`` while any reservation overlaps
the read. No Rust, relay, or RPC surface changes: the blind spot is bridged
entirely Python-side, where the session's posture is known.

Alternatives rejected:

- (b) Relay nonce sourcing (``eth_getTransactionCount`` against a relay):
  private builder endpoints are broadcast-only by contract; that read is not
  part of their offered surface, and probing it would add a network
  round-trip + an untested fallback to the hottest submit path for a capability
  most relays won't serve.
- (c) Serializing the private lane to one in-flight batch: the single ordered
  submitter already serializes submissions; ordering was never the defect —
  visibility was. Serialization only shrinks the collision window and costs
  throughput on every non-colliding batch.

Reservation lifecycle (enum FSM; no ad-hoc bookkeeping)::

    RESERVED ── observed on-chain nonce reaches the top ──> RELEASED (terminal)
    RESERVED ── TTL elapsed with no observed advance ──────> EXPIRED (terminal)

- ``RESERVED``: the range ``[base, base + size)`` is booked against the lane;
  the batch may sign there while the local read stays stale.
- ``RELEASED``: the observed on-chain nonce passed the range's top, so the
  whole range is consumed/revealed and the local read subsumes it. Silent —
  the reservation did its job.
- ``EXPIRED``: the reservation aged out after the bounded TTL (a broadcast
  that was lost, or a pending tx that will never reveal). Loud: one WARN
  naming the range fires and the slot is vacated — never silent, never an
  unbounded ledger. Re-use is possible until the chain catches up; that is the
  documented TTL trade (a bounded collision risk beats an unbounded stall).

Transitions exist ONLY on the :class:`ReservedNonceRange` transition methods;
``NonceLane._settle`` is the single sequencing site and prunes terminal
reservations in the same pass, so the ledger stays bounded.

TTL checks run at reserve time, not on a background timer: the only consumer
of a reservation's liveness is the next reserve, so a timer adds a task and
wakes the hot loop for no behavioral gain.

A disabled lane (empty ``relay_urls``) is a pure passthrough: the no-relay
(public-mempool) posture keeps its exact prior behavior — the caller's nonce,
nothing reserved.

Posture ownership: the session's lane is built once at startup from the relay
env (``relay_urls_from_env``), and the submit seam consults it instead of
re-reading env per batch.
"""

from __future__ import annotations

import os
import time
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from enum import Enum
from typing import Any

from degenbot.logging import logger as bot_logger

#: First match wins; both spellings persist from the relay rollout.
RELAY_URL_ENV_VARS = ("DEGENBOT_SUBMIT_RELAY_URLS", "DEGENBOT_SUBMIT_RELAY_URL")

#: TTL bound for a reservation. A relay-pending broadcast should conclude (or
#: be replaceable) in a few blocks; past this the reservation costs re-use
#: risk and the WARN clears it.
DEFAULT_RESERVATION_TTL_S = 30.0


def relay_urls_from_env() -> list[str]:
    """The configured relay broadcast URLs (the one home for the env shape).

    Comma-separated either env var; blank entries dropped.
    """
    for var in RELAY_URL_ENV_VARS:
        raw = os.environ.get(var)
        if raw and raw.strip():
            return [url.strip() for url in raw.split(",") if url.strip()]
    return []


class ReservationState(Enum):
    """Reservation lifecycle: ``RESERVED`` -> (``RELEASED`` | ``EXPIRED``).

    Both terminal states are exclusionary ends of the same pass: RELEASED
    means the chain subsumed the range (normal path), EXPIRED means the TTL
    vacated it (the loud WARN path).
    """

    RESERVED = "reserved"
    RELEASED = "released"
    EXPIRED = "expired"


@dataclass
class ReservedNonceRange:
    """One relay batch's booked nonce range ``[base, top)`` and its FSM state.

    Transitions live here and nowhere else; the terminal states refuse all
    further transitions.
    """

    base: int
    size: int
    reserved_at: float
    deadline: float
    state: ReservationState = field(default=ReservationState.RESERVED)

    @property
    def top(self) -> int:
        """One past the last reserved nonce (the invariant the overlay reads)."""
        return self.base + self.size

    def release_if_consumed(self, observed_nonce: int) -> bool:
        """``RESERVED -> RELEASED`` once the on-chain nonce passed the top."""
        if self.state is not ReservationState.RESERVED or observed_nonce < self.top:
            return False
        self.state = ReservationState.RELEASED
        return True

    def expire_if_due(self, now: float) -> bool:
        """``RESERVED -> EXPIRED`` once the TTL elapsed without an advance."""
        if self.state is not ReservationState.RESERVED or now < self.deadline:
            return False
        self.state = ReservationState.EXPIRED
        return True


class NonceLane:
    """Reservation overlay over the local pending-nonce read (relay posture).

    Constructed once per session from the configured relay URLs; an empty
    list disables the lane (public-mempool passthrough). The clock and
    logger are injectable seams (tests / deterministic TTL).
    """

    def __init__(
        self,
        relay_urls: Sequence[str],
        *,
        ttl_s: float = DEFAULT_RESERVATION_TTL_S,
        clock: Callable[[], float] = time.monotonic,
        logger: Any = bot_logger,
    ) -> None:
        self._relay_urls = list(relay_urls)
        self._ttl_s = ttl_s
        self._clock = clock
        self._logger = logger
        self._reservations: list[ReservedNonceRange] = []
        self.released_count = 0
        self.expired_count = 0

    @property
    def relay_urls(self) -> list[str]:
        """The posture-defining relay URLs (a defensive copy)."""
        return list(self._relay_urls)

    @property
    def enabled(self) -> bool:
        """True iff relay-postured (only then do reservations exist)."""
        return bool(self._relay_urls)

    @property
    def active_reservations(self) -> tuple[ReservedNonceRange, ...]:
        """The live (still RESERVED) bookings, for observability and tests."""
        return tuple(self._reservations)

    def reserve_base(
        self,
        local_nonce: int,
        *,
        size: int,
    ) -> int:
        """Reconcile, then return the base this batch must sign at.

        The reconciling observed nonce is ``local_nonce`` itself — the fresh
        local read each submit makes. Relay posture: the base is
        ``max(local_nonce, last_reserved_top)``, so a stale read can never
        re-claim a still-pending relay nonce. Disabled posture: the local
        read passes through untouched (public-mempool semantics).
        """
        now = self._clock()
        self._settle(observed_nonce=local_nonce, now=now)
        if not self.enabled or size <= 0:
            return local_nonce
        base = max(local_nonce, self._reserved_top())
        self._reservations.append(
            ReservedNonceRange(
                base=base,
                size=size,
                reserved_at=now,
                deadline=now + self._ttl_s,
            )
        )
        return base

    def _settle(self, *, observed_nonce: int, now: float) -> None:
        """The single transition site: release/expire every reservation, then
        prune the terminal ones (ledger stays bounded)."""
        for reservation in self._reservations:
            if reservation.release_if_consumed(observed_nonce):
                self.released_count += 1
            elif reservation.expire_if_due(now):
                self.expired_count += 1
                self._logger.warning(
                    "[nonce-lane] relay reservation %d..%d expired after %.0fs without "
                    "an observed on-chain advance - its slot is vacated and the next "
                    "batch may re-claim those nonces (the broadcast was likely lost)",
                    reservation.base,
                    reservation.top,
                    self._ttl_s,
                )
        self._reservations = [r for r in self._reservations if r.state is ReservationState.RESERVED]

    def _reserved_top(self) -> int:
        return max((r.top for r in self._reservations), default=0)
