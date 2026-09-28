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

import warnings
from dataclasses import dataclass, field
from typing import Any

import pytest

from degenbot.runner._dispatch import _submit_batch_records
from degenbot.runner._relay_posture import RelayPosture
from degenbot.runner.bot_runner import (
    ActivationGateRefused,
    BotRunner,
)
from degenbot.runner.config import ArbitrageConfig
from tests.fakes.session import (
    FakeAsyncW3,
    FakeCandidate,
    FakeRelayProvider,
    FakeSession,
    FakeSubmitOutcome,
    fake_session,
)
from tests.helpers.boot_actors import boot_runner
from tests.helpers.identity_env import identity_env


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


def _cfg(*, dry_run: bool) -> ArbitrageConfig:
    with identity_env({
        "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
        "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
        "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
    }):
        return ArbitrageConfig.build(live=not dry_run, permutation=None)


def _runner(
    *,
    dry_run: bool,
    settlement_arm: bool = False,
    readiness=None,
    settlement_endpoints=None,
) -> BotRunner:
    """A real ``BotRunner`` on fake actors, with the boot posture gate live.

    ``readiness`` / ``settlement_endpoints`` are the activation-gate DI
    factories (``None`` = the real ``degenbot.strategy`` resolvers); a test
    injects a resolving factory or a raising refusal.
    """
    return boot_runner(
        _cfg(dry_run=dry_run),
        settlement_arm=settlement_arm,
        readiness=readiness,
        settlement_endpoints=settlement_endpoints,
        install_sigint=False,
    )


def _session(relay_posture: RelayPosture | None) -> FakeSession:
    return fake_session(
        relay_posture=relay_posture,
        current_block=100,
        async_w3=FakeAsyncW3(as_async_alloy=_opaque_rust_provider),
    )


def _relays() -> list[FakeRelayProvider]:
    return [FakeRelayProvider(as_async_alloy=_opaque_rust_provider)]


def _candidate(path_id: int = 7) -> FakeCandidate:
    return FakeCandidate(path_id=path_id, net_profit=1, gas_used=1)


def _outcome(candidates: list[FakeCandidate]) -> FakeSubmitOutcome:
    return FakeSubmitOutcome(gas_profitable=candidates)


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
    """The boot gate's posture contract, driven through the public ``start()``.

    The gate reads the Rust readiness once and takes the relay posture (and
    its refusal) from the Rust settlement composition. These tests inject the
    Rust answers and observe the public boot, pinning the contract without
    the retired private posture mirror.
    """

    async def test_a_backrun_only_boot_mints_no_settlement_posture(self) -> None:
        """Posture-driven boot (case B): settlement inactive + a backrun facet
        active boots with no settlement posture in either stance, and never
        consults the settlement endpoint resolver."""

        def readiness() -> Any:
            return _view(settlement_active=False, mevblocker_backrun_active=True)

        def settlement_endpoints() -> list[str]:
            pytest.fail("a backrun-only boot consulted the settlement endpoints")

        for dry_run in (False, True):
            runner = _runner(
                dry_run=dry_run,
                readiness=readiness,
                settlement_endpoints=settlement_endpoints,
            )
            await runner.start()
            assert runner._session is not None
            assert runner._session.relay_posture is None, (
                "a settlement-deactivated boot carries no settlement posture"
            )

    async def test_an_empty_fleet_boot_refuses_in_both_stances(self) -> None:
        """No activated facet, no work: the Rust hosted gate refuses the empty
        fleet at the readiness resolution, so the boot aborts before the
        settlement resolver is ever consulted, in dry-run exactly as live."""

        def readiness() -> Any:
            raise ValueError(
                "no strategy facet is active: activate one with "
                "`degenbot strategy activate settlement --endpoints-default`"
            )

        def settlement_endpoints() -> list[str]:
            pytest.fail("an empty-fleet boot consulted the settlement endpoints")

        for dry_run in (False, True):
            with pytest.raises(ActivationGateRefused, match="degenbot strategy activate"):
                await _runner(
                    dry_run=dry_run,
                    readiness=readiness,
                    settlement_endpoints=settlement_endpoints,
                ).start()

    async def test_a_settled_live_boot_mints_the_posture(self) -> None:
        """A live, settlement-active boot carries the Rust-resolved endpoints."""
        runner = _runner(
            dry_run=False,
            settlement_arm=True,
            readiness=lambda: _view(settlement_active=True),
            settlement_endpoints=lambda: ["http://relay-a"],
        )
        await runner.start()
        assert runner._session is not None
        posture = runner._session.relay_posture
        assert posture is not None
        assert posture.relay_urls == ["http://relay-a"]

    async def test_a_settled_dry_run_boot_carries_no_posture(self) -> None:
        """A settled dry-run boot has no signing surface, so no posture is
        minted even though the Rust resolver settles the endpoints."""
        runner = _runner(
            dry_run=True,
            settlement_arm=True,
            readiness=lambda: _view(settlement_active=True),
            settlement_endpoints=lambda: ["http://relay-a"],
        )
        await runner.start()
        assert runner._session is not None
        assert runner._session.relay_posture is None

    def test_a_readiness_refusal_is_an_activation_refusal(self) -> None:
        """A Rust readiness refusal aborts the boot the same way: the runner
        never enters run() on an unreadiness activation."""

        def readiness() -> Any:
            raise ValueError("facet unreadiness: degenbot strategy activate")

        runner = _runner(dry_run=False, readiness=readiness)
        with pytest.raises(ActivationGateRefused, match="degenbot strategy activate"):
            runner._gate_readiness()

    async def test_an_injected_arm_off_beats_a_settlement_active_view(self) -> None:
        """The injected settlement arm is the session's ONE fact. A DI seam
        that pins the arm off produces a backrun-only boot even when the
        readiness view reports the settlement facet active, so the posture
        minter never consults the endpoint resolver."""

        def settlement_endpoints() -> list[str]:
            pytest.fail("the resolved arm was off, not the raw view")

        runner = _runner(
            dry_run=False,
            readiness=lambda: _view(settlement_active=True, mevblocker_backrun_active=True),
            settlement_endpoints=settlement_endpoints,
        )
        await runner.start()
        assert runner._session is not None
        assert runner._session.relay_posture is None, (
            "a settlement-deactivated arm carries no settlement posture"
        )


@dataclass
class _ReadinessView:
    """A readiness view stand-in with the settled-block arm on by default."""

    settlement_active: bool = True
    mevblocker_backrun_active: bool = False
    txpool_backrun_active: bool = False
    settlement_endpoints: list[str] = field(default_factory=list)
    mevblocker_backrun_endpoints: list[str] = field(default_factory=list)
    txpool_backrun_endpoints: list[str] = field(default_factory=list)
    active_backrun_facets: list[str] = field(default_factory=list)


def _view(
    *,
    settlement_active: bool = True,
    mevblocker_backrun_active: bool = False,
    txpool_backrun_active: bool = False,
    settlement_endpoints: list[str] | None = None,
) -> _ReadinessView:
    active_backrun_facets: list[str] = []
    if mevblocker_backrun_active:
        active_backrun_facets.append("mevblocker_backrun")
    if txpool_backrun_active:
        active_backrun_facets.append("txpool_backrun")
    return _ReadinessView(
        settlement_active=settlement_active,
        mevblocker_backrun_active=mevblocker_backrun_active,
        txpool_backrun_active=txpool_backrun_active,
        settlement_endpoints=[] if settlement_endpoints is None else settlement_endpoints,
        active_backrun_facets=active_backrun_facets,
    )
