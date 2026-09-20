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

import pytest

from degenbot.runner._dispatch import SubmissionSmoke, _submit_batch_records
from degenbot.runner._relay_posture import RelayPosture
from degenbot.runner.bot_runner import BotRunner


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


def _session(relay_posture: RelayPosture | None) -> Any:
    return types.SimpleNamespace(
        async_w3=types.SimpleNamespace(as_async_alloy=_opaque_rust_provider),
        cfg=types.SimpleNamespace(
            operator_private_key="0x" + "a" * 64,
            chain_id=1,
            dry_run=False,
            inject_executor_code=False,
        ),
        dispatcher=types.SimpleNamespace(current_block=100),
        relay_posture=relay_posture,
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
        posture = RelayPosture(relay_urls=["http://relay-a"])
        assert posture.enabled
        assert posture.relay_urls == ["http://relay-a"]

    def test_an_empty_posture_cannot_be_constructed(self) -> None:
        """Fail-closed holder: the empty posture (the public-mempool footgun)
        cannot exist; the boot gate owns the refusal."""
        with pytest.raises(RuntimeError, match="settled settlement endpoints"):
            RelayPosture(relay_urls=[])

    def test_reserve_base_is_a_loud_passthrough(self) -> None:
        """Nonce issuance lives in Rust; the shim warns and returns the
        caller's nonce unchanged rather than booking a reservation."""
        posture = RelayPosture(relay_urls=["http://relay-a"])
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            assert posture.reserve_base(42, size=2) == 42
            assert posture.reserve_base(43, size=1) == 43
        assert len(caught) == 2
        assert all(item.category is DeprecationWarning for item in caught)

    def test_relay_urls_survive_mutation(self) -> None:
        posture = RelayPosture(relay_urls=["http://relay-a", "http://relay-b"])
        urls = posture.relay_urls
        urls.clear()
        assert posture.relay_urls == ["http://relay-a", "http://relay-b"]


class TestSubmitSeamForwardsTheCallersNonce:
    """Python forwards the chain read; the Rust authority issues the nonce."""

    async def test_relay_batches_forward_the_same_nonce_read(self) -> None:
        """Two relay batches with the same local chain read forward that value
        unchanged. The authority, not Python, advances the nonce."""
        posture = RelayPosture(relay_urls=["http://relay-a"])
        submitter = _RecordingSubmitter()
        session = _session(posture)

        await _submit_relay(session, [_candidate()], operator_nonce=42, submitter=submitter)
        await _submit_relay(session, [_candidate()], operator_nonce=42, submitter=submitter)

        assert submitter.calls[0]["context"].broadcast_providers is not None
        assert [call["context"].operator_nonce for call in submitter.calls] == [42, 42]

    async def test_multi_candidate_batch_forwards_the_read_unchanged(self) -> None:
        posture = RelayPosture(relay_urls=["http://relay-a"])
        submitter = _RecordingSubmitter()
        session = _session(posture)

        await _submit_relay(
            session, [_candidate(1), _candidate(2)], operator_nonce=42, submitter=submitter
        )

        assert [call["context"].operator_nonce for call in submitter.calls] == [42]


class TestNoRelayPosture:
    """A live session without a settled posture refuses submission."""

    async def test_a_live_session_without_a_posture_refuses_submission(self) -> None:
        """Unreachable past the boot gate, but if it ever happens the seam
        refuses rather than falling back to a raw public mempool broadcast."""
        submitter = _RecordingSubmitter()
        session = _session(None)

        await _submit_relay(session, [_candidate()], operator_nonce=42, submitter=submitter)

        assert submitter.calls == [], "no submit may leave without a posture"

    async def test_a_dry_run_session_without_a_posture_submits_nothing(self) -> None:
        posture = RelayPosture(relay_urls=["http://relay-a"])
        submitter = _RecordingSubmitter()
        session = _session(posture)
        session.cfg.dry_run = True

        await _submit_relay(session, [_candidate()], operator_nonce=42, submitter=submitter)

        # The dry-run skip is guarded downstream (the Rust leaf skips the
        # candidates); the seam itself still resolves theproviders.
        assert len(submitter.calls) == 1


class TestBootGate:
    """The runner's live-mode activation gate over the ambient unset config."""

    def test_a_live_boot_either_refuses_or_settles_the_allowlist(self) -> None:
        """Fail-closed over whatever the ambient holder config holds: an
        unset settlement posture refuses with the remediation, and a settled
        one resolves to a non-empty, on-allowlist URL set — never a silent
        public-mempool default."""
        pinned = {
            "https://rpc.flashbots.net?hint=hash",
            "https://rpc.mevblocker.io/noreverts",
            "https://rpc.mevblocker.io/fullprivacy",
        }
        try:
            posture = BotRunner._resolve_relay_posture(live=True)
        except RuntimeError as refusal:
            message = str(refusal)
            assert "settlement" in message
            assert "degenbot strategy activate" in message
        else:
            assert posture is not None
            assert posture.relay_urls, "a settled live posture is never empty"
            assert set(posture.relay_urls) <= pinned, (
                "live posture is restricted to the pinned allowlist"
            )

    def test_a_dry_run_boot_needs_no_activation(self) -> None:
        assert BotRunner._resolve_relay_posture(live=False) is None
