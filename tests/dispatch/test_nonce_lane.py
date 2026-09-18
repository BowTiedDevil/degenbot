"""Private-lane nonce reservations: the lane FSM + the submit-seam integration.

What this suite guards against: under a relay posture
(``DEGENBOT_SUBMIT_RELAY_URLS`` set), broadcasts go to private builder
endpoints while the operator nonce is read from the LOCAL node at submit time
(``_sim_submit_pipeline._submit_ordered`` -> ``get_transaction_count`` ->
``_submit_batch_records``). Relay-pending txs are invisible to that pending
read, so a relay batch signing at the raw local read re-claims the same base
nonce and replaces or collides with the still-pending relay tx: driving the
submit seam twice with an identical local read must never sign the same base
twice.

A reservation overlay (:class:`~degenbot.runner._nonce_lane.NonceLane`)
guards against exactly that at the submit seam (see that module's design note
for the strategy and the reservation FSM). The tests below drive the seam with
CONSTRUCTOR-INJECTED fakes — a recording submitter and injected relay
providers (the ``submitter``/``relay_providers`` seams on
``_submit_batch_records``), no live RPC, no monkeypatching, no env mutation:
relay posture enters through the injected NonceLane itself.
"""

from __future__ import annotations

import types
from typing import Any

from degenbot.runner._dispatch import _submit_batch_records
from degenbot.runner._nonce_lane import (
    NonceLane,
    ReservationState,
    ReservedNonceRange,
)


class _RecordingSubmitter:
    """Fake ``dispatch_and_submit`` companion: records the submit kwargs."""

    def __init__(self) -> None:
        self.calls: list[dict[str, Any]] = []

    async def __call__(self, **kwargs: Any) -> list[Any]:
        self.calls.append(kwargs)
        return []


class _FakeClock:
    """Hand-advanced monotonic clock (no sleeps in TTL tests)."""

    def __init__(self, start: float = 0.0) -> None:
        self.now = start

    def __call__(self) -> float:
        return self.now

    def advance(self, seconds: float) -> None:
        self.now += seconds


class _CaptureLogger:
    """Records only what the lane must emit (the loud TTL WARN)."""

    def __init__(self) -> None:
        self.warnings: list[str] = []

    def warning(self, msg: str, *args: Any) -> None:
        # logging's lazy contract: the args fill the %-placeholders.
        self.warnings.append(msg % args if args else msg)

    def info(self, msg: str) -> None:
        pass


def _opaque_rust_provider() -> Any:
    # The injected fakes are opaque at the submit seam: the only access is
    # as_async_alloy() producing the Rust-pyclass provider.
    return object()


def _session(nonce_lane: NonceLane | None) -> Any:
    return types.SimpleNamespace(
        async_w3=types.SimpleNamespace(as_async_alloy=_opaque_rust_provider),
        cfg=types.SimpleNamespace(
            operator_private_key="0x" + "a" * 64,
            chain_id=1,
            dry_run=False,
            inject_executor_code=False,
        ),
        dispatcher=types.SimpleNamespace(current_block=100),
        nonce_lane=nonce_lane,
    )


def _relays() -> list[Any]:
    return [types.SimpleNamespace(as_async_alloy=_opaque_rust_provider)]


def _candidate(path_id: int = 7) -> Any:
    return types.SimpleNamespace(
        path_id=path_id, solve_block=1, net_profit=1, gas_used=1, execute_calldata=None
    )


def _outcome(candidates: list[Any]) -> Any:
    # MergedOutcome-protocol stand-in: attribute parity with the shape the
    # submit seam reads.
    return types.SimpleNamespace(gas_profitable=candidates)


async def _submit_relay(
    session: Any,
    candidates: list[Any],
    *,
    operator_nonce: int,
    submitter: _RecordingSubmitter,
) -> None:
    await _submit_batch_records(
        session,
        _outcome(candidates),
        operator_nonce=operator_nonce,
        submitter=submitter,
        relay_providers=_relays(),
    )


class TestRelayBatchNonceReservations:
    """Submit-seam behavior under a relay posture (injected lanes/fakes)."""

    async def test_consecutive_relay_batches_do_not_repeat_base_nonce(self) -> None:
        """Two relay batches reading the SAME local nonce broadcast 42 then 43.

        A second batch signing the caller's base 42 again — the base the
        first batch already broadcast, invisible to the local read — is the
        collision this suite forbids.
        """
        lane = NonceLane(relay_urls=["http://relay-a"])
        submitter = _RecordingSubmitter()
        session = _session(lane)

        await _submit_relay(session, [_candidate()], operator_nonce=42, submitter=submitter)
        await _submit_relay(session, [_candidate()], operator_nonce=42, submitter=submitter)

        assert submitter.calls[0]["context"].broadcast_providers is not None
        assert [call["context"].operator_nonce for call in submitter.calls] == [42, 43]

    async def test_second_batch_accounts_for_multi_tx_pending_first_batch(self) -> None:
        """A 2-candidate relay batch reserves both nonces; the next batch
        reading the stale local value signs at base + 2, not over nonce 43."""
        lane = NonceLane(relay_urls=["http://relay-a"])
        submitter = _RecordingSubmitter()
        session = _session(lane)

        await _submit_relay(
            session, [_candidate(1), _candidate(2)], operator_nonce=42, submitter=submitter
        )
        await _submit_relay(session, [_candidate(3)], operator_nonce=42, submitter=submitter)

        assert [call["context"].operator_nonce for call in submitter.calls] == [42, 44]
        assert [r.base for r in lane.active_reservations] == [42, 44]

    async def test_reservation_released_when_observed_nonce_advances_past_it(self) -> None:
        """Once the observed on-chain nonce passes a reservation's top, the
        local read subsumes it: RELEASED + pruned, and the next base follows
        the chain (no phantom hold)."""
        lane = NonceLane(relay_urls=["http://relay-a"])
        submitter = _RecordingSubmitter()
        session = _session(lane)

        await _submit_relay(
            session, [_candidate(1), _candidate(2)], operator_nonce=42, submitter=submitter
        )
        assert len(lane.active_reservations) == 1

        # The relay txs landed (or revealed): the local read now reports 44.
        await _submit_relay(session, [_candidate(3)], operator_nonce=44, submitter=submitter)

        assert lane.released_count == 1
        assert lane.expired_count == 0
        # The only live booking is the batch just made at 44 — the released
        # range was pruned from the ledger.
        assert [r.base for r in lane.active_reservations] == [44]
        assert all(r.state is ReservationState.RESERVED for r in lane.active_reservations)
        assert [call["context"].operator_nonce for call in submitter.calls] == [42, 44]

    async def test_ttl_expiry_frees_the_base_and_warns(self) -> None:
        """A stale reservation expires after its TTL with a loud WARN naming
        the range — never silent, never held unboundedly; the base is vacated
        (re-use possible until the chain catches up) and the ledger stays
        bounded (the terminal reservation is pruned)."""
        clock = _FakeClock()
        logger = _CaptureLogger()
        lane = NonceLane(relay_urls=["http://relay-a"], ttl_s=5.0, clock=clock, logger=logger)
        submitter = _RecordingSubmitter()
        session = _session(lane)

        await _submit_relay(session, [_candidate()], operator_nonce=42, submitter=submitter)
        assert [call["context"].operator_nonce for call in submitter.calls] == [42]

        clock.advance(6.0)
        await _submit_relay(session, [_candidate()], operator_nonce=42, submitter=submitter)

        # The expired slot is vacated: the re-read 42 is broadcast again.
        assert [call["context"].operator_nonce for call in submitter.calls] == [42, 42]
        assert lane.expired_count == 1
        assert lane.released_count == 0
        assert len(logger.warnings) == 1, logger.warnings
        assert "42" in logger.warnings[0]
        # The ledger stays bounded: the expired range is pruned and only the
        # fresh booking from this batch remains.
        assert [r.base for r in lane.active_reservations] == [42]


class TestNoRelayPosture:
    """No-relay posture (empty relay_urls) keeps the current behavior."""

    async def test_no_relay_lane_is_a_passthrough(self) -> None:
        """Without relay URLs the caller's nonce passes through unchanged and
        nothing is reserved — the exact public-mempool semantics."""
        lane = NonceLane(relay_urls=[])
        submitter = _RecordingSubmitter()
        session = _session(lane)

        await _submit_relay(session, [_candidate()], operator_nonce=42, submitter=submitter)
        await _submit_relay(session, [_candidate()], operator_nonce=42, submitter=submitter)

        # Identical bases (the caller owns the nonce) + no relay broadcast.
        assert [call["context"].operator_nonce for call in submitter.calls] == [42, 42]
        assert all(call["context"].broadcast_providers is None for call in submitter.calls)
        assert lane.active_reservations == ()
        assert lane.released_count == 0
        assert lane.expired_count == 0


class TestReservationFsm:
    """The reservation state machine: RESERVED -> (RELEASED | EXPIRED)."""

    def test_fresh_reservation_is_reserved(self) -> None:
        reservation = ReservedNonceRange(base=42, size=3, reserved_at=0.0, deadline=5.0)
        assert reservation.state is ReservationState.RESERVED
        assert reservation.top == 45

    def test_release_transition_requires_observed_nonce_past_top(self) -> None:
        reservation = ReservedNonceRange(base=42, size=3, reserved_at=0.0, deadline=5.0)
        # Observed 43 < top 45: the range is not fully consumed yet.
        assert not reservation.release_if_consumed(43)
        assert reservation.state is ReservationState.RESERVED
        assert reservation.release_if_consumed(45)
        assert reservation.state is ReservationState.RELEASED

    def test_expire_transition_requires_ttl_elapsed(self) -> None:
        reservation = ReservedNonceRange(base=42, size=1, reserved_at=0.0, deadline=5.0)
        assert not reservation.expire_if_due(4.999)
        assert reservation.state is ReservationState.RESERVED
        assert reservation.expire_if_due(5.0)
        assert reservation.state is ReservationState.EXPIRED

    def test_terminal_states_never_transition(self) -> None:
        released = ReservedNonceRange(
            base=1, size=1, reserved_at=0.0, deadline=1.0, state=ReservationState.RELEASED
        )
        expired = ReservedNonceRange(
            base=1, size=1, reserved_at=0.0, deadline=1.0, state=ReservationState.EXPIRED
        )
        assert not released.release_if_consumed(100)
        assert not released.expire_if_due(100)
        assert not expired.release_if_consumed(100)
        assert not expired.expire_if_due(100)


class TestLaneOverlay:
    """The pure reservation overlay (no submit seam involved)."""

    def test_uses_local_read_when_no_reservations(self) -> None:
        lane = NonceLane(relay_urls=["http://relay-a"])
        assert lane.reserve_base(42, size=2) == 42

    def test_raises_high_water_over_stale_local_read(self) -> None:
        """The overlay invariant: reserved_next = max(local_read, last_top)."""
        lane = NonceLane(relay_urls=["http://relay-a"])
        lane.reserve_base(42, size=2)
        assert lane.reserve_base(42, size=1) == 44  # local read is stale
        # A PARTIAL advance (43 < top 45) leaves the still-pending slot held.
        assert lane.reserve_base(43, size=1) == 45

    def test_disabled_lane_returns_local_read(self) -> None:
        lane = NonceLane(relay_urls=[])
        assert lane.reserve_base(42, size=2) == 42
        assert lane.reserve_base(42, size=2) == 42  # repeat allowed
        assert lane.active_reservations == ()

    def test_relay_urls_survive_mutation(self) -> None:
        lane = NonceLane(relay_urls=["http://relay-a", "http://relay-b"])
        urls = lane.relay_urls
        urls.clear()
        assert lane.relay_urls == ["http://relay-a", "http://relay-b"]
