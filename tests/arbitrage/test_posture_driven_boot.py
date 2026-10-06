"""Posture-driven boot (case B): the runner runs only the activated facets.

The hosted Python process is settlement-shaped (build_paths -> sims -> submit
arm), but the operator's config owns which facets run: with the settlement
facet off, the runner boots the same engine, enables the active hosted arms
(the backrun drivers resume() starts), and NEVER registers paths or trims
python state. An all-inactive fleet refuses at the posture gate.
"""

from __future__ import annotations

import signal

import pytest

from degenbot.runner import BotRunner
from degenbot.runner.config import ArbitrageConfig
from degenbot.strategy import validate_strategy_readiness
from tests.helpers.boot_actors import FakeBootReadiness, boot_runner, noop_coro
from tests.helpers.identity_env import identity_env
from tests.helpers.rpc_env import rpc_env


@pytest.fixture(autouse=True)
def _rpc_env(monkeypatch: pytest.MonkeyPatch) -> None:
    rpc_env(monkeypatch)


@pytest.fixture(autouse=True)
def _restore_sigint() -> None:
    yield
    signal.signal(signal.SIGINT, signal.SIG_DFL)


def _runner(
    path_builder,
    *,
    settlement_arm: bool = False,
    readiness=None,
    settlement_endpoints=None,
) -> BotRunner:
    with identity_env(
        {
            "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
            "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
            "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
        }
    ):
        cfg = ArbitrageConfig.build(live=True, permutation=None)
    return boot_runner(
        cfg,
        path_builder=path_builder,
        settlement_arm=settlement_arm,
        readiness=readiness,
        settlement_endpoints=settlement_endpoints,
    )


async def test_a_backrun_only_boot_never_builds_paths() -> None:
    events: list[str] = []

    def _builder(**kwargs):
        events.append("path_builder")

    session = _runner(_builder)
    await session.start()
    await session.run()

    assert "path_builder" not in events, (
        "a settlement-deactivated boot must not register settlement paths"
    )


async def test_a_backrun_only_boot_does_not_trim_python_state() -> None:
    session = _runner(lambda **kw: None)
    await session.start()
    await session.run()
    assert not session._injected_bot.released, (
        "with no settlement registration there is no hot-loop trim"
    )


async def test_a_backrun_only_boot_enables_the_active_hosted_arms() -> None:
    """resume() starts hosted loops only for facets the operator ENABLED, so
    the runner enables every facet the readiness resolution reports active.
    The expectation derives from that resolution, not from pinned ambient
    state."""
    session = _runner(lambda **kw: None)
    await session.start()
    await session.run()

    readiness = validate_strategy_readiness()
    expected_facets = list(readiness.active_backrun_facets)
    assert session.engine_registry.engine.resume_facets == expected_facets, (
        "resume() must receive the active hosted arms"
    )
    records = dict((name, state) for name, state, _halt in session.engine_registry.engine.strategies())
    active_facets = set(expected_facets)
    for facet in ("mevblocker_backrun", "txpool_backrun"):
        assert records[facet] == ("enabled" if facet in active_facets else "registered"), (
            f"{facet}: admission must follow strategy.{facet}.active"
        )
    # The settlement arm's engine state is advisory for its pump arm; it is
    # never gated through enable_strategy.
    assert records["settlement"] == "registered"


async def test_a_settlement_active_boot_hosts_no_backrun_arms() -> None:
    """A settlement-active boot passes NO hosted arms to resume, even when a
    backrun facet is also active: the settlement pump arm is this runner's arm,
    so the enabled-facet set is empty under that disposition."""
    session = _runner(
        lambda **kw: noop_coro(),
        settlement_arm=True,
        readiness=lambda: FakeBootReadiness(settlement_active=True),
        settlement_endpoints=lambda: ["http://relay-a"],
    )
    await session.start()
    await session.run()

    assert session.engine_registry.engine.resume_facets == [], (
        "a settlement-active boot must not host backrun lanes"
    )
    records = dict((name, state) for name, state, _halt in session.engine_registry.engine.strategies())
    assert records["mevblocker_backrun"] == "registered"
    assert records["txpool_backrun"] == "registered"
