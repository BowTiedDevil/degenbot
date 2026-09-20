"""Relay posture + nonce issuance at the submit seam.

What this suite guards against: Python computing the operator nonce. The
process-wide Rust ``NonceAuthority`` is the one issuer, and the settlement
seam forwards the submission-time chain read unchanged so the authority can
seed from it and lease the sign-time nonce. The retired Python reservation
ledger is a loud deprecation shim only.

The seam is driven with CONSTRUCTOR-INJECTED fakes — a recording submitter and
injected relay providers (the ``submitter``/``relay_providers`` seams on
``_submit_batch_records``), no live RPC, no monkeypatching, no env mutation.
"""

from __future__ import annotations

import types
import warnings
from typing import Any

from degenbot.runner._dispatch import SubmissionSmoke, _submit_batch_records
from degenbot.runner._nonce_lane import NonceLane


class _RecordingSubmitter:
    """Fake ``dispatch_and_submit`` companion: records the submit kwargs."""

    def __init__(self) -> None:
        self.calls: list[dict[str, Any]] = []

    async def __call__(self, **kwargs: Any) -> list[Any]:
        self.calls.append(kwargs)
        return []


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
        submission_smoke=SubmissionSmoke(),
    )


def _relays() -> list[Any]:
    return [types.SimpleNamespace(as_async_alloy=_opaque_rust_provider)]


def _candidate(path_id: int = 7) -> Any:
    return types.SimpleNamespace(
        path_id=path_id, solve_block=1, net_profit=1, gas_used=1, execute_calldata=None
    )


def _outcome(candidates: list[Any]) -> Any:
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


class TestRelayPosture:
    """The posture holder and the deprecated reservation shim."""

    def test_relay_posture_reads_the_configured_urls(self) -> None:
        lane = NonceLane(relay_urls=["http://relay-a"])
        assert lane.enabled
        assert lane.relay_urls == ["http://relay-a"]

    def test_disabled_posture_is_empty(self) -> None:
        lane = NonceLane(relay_urls=[])
        assert not lane.enabled
        assert lane.relay_urls == []

    def test_reserve_base_is_a_loud_passthrough(self) -> None:
        """Nonce issuance lives in Rust; the shim warns and returns the
        caller's nonce unchanged rather than booking a reservation."""
        lane = NonceLane(relay_urls=["http://relay-a"])
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            assert lane.reserve_base(42, size=2) == 42
            assert lane.reserve_base(43, size=1) == 43
        assert len(caught) == 2
        assert all(item.category is DeprecationWarning for item in caught)

    def test_relay_urls_survive_mutation(self) -> None:
        lane = NonceLane(relay_urls=["http://relay-a", "http://relay-b"])
        urls = lane.relay_urls
        urls.clear()
        assert lane.relay_urls == ["http://relay-a", "http://relay-b"]


class TestSubmitSeamForwardsTheCallersNonce:
    """Python forwards the chain read; the Rust authority issues the nonce."""

    async def test_relay_batches_forward_the_same_nonce_read(self) -> None:
        """Two relay batches with the same local chain read forward that value
        unchanged. The authority, not Python, advances the nonce."""
        lane = NonceLane(relay_urls=["http://relay-a"])
        submitter = _RecordingSubmitter()
        session = _session(lane)

        await _submit_relay(session, [_candidate()], operator_nonce=42, submitter=submitter)
        await _submit_relay(session, [_candidate()], operator_nonce=42, submitter=submitter)

        assert submitter.calls[0]["context"].broadcast_providers is not None
        assert [call["context"].operator_nonce for call in submitter.calls] == [42, 42]

    async def test_multi_candidate_batch_forwards_the_read_unchanged(self) -> None:
        lane = NonceLane(relay_urls=["http://relay-a"])
        submitter = _RecordingSubmitter()
        session = _session(lane)

        await _submit_relay(
            session, [_candidate(1), _candidate(2)], operator_nonce=42, submitter=submitter
        )

        assert [call["context"].operator_nonce for call in submitter.calls] == [42]


class TestNoRelayPosture:
    """No-relay posture keeps public-mempool semantics."""

    async def test_no_relay_posture_sends_no_broadcast_providers(self) -> None:
        lane = NonceLane(relay_urls=[])
        submitter = _RecordingSubmitter()
        session = _session(lane)

        await _submit_relay(session, [_candidate()], operator_nonce=42, submitter=submitter)
        await _submit_relay(session, [_candidate()], operator_nonce=42, submitter=submitter)

        assert all(call["context"].broadcast_providers is None for call in submitter.calls)
        assert [call["context"].operator_nonce for call in submitter.calls] == [42, 42]
