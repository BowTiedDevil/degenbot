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
from degenbot.runner.bot_runner import InjectedActors
from degenbot.runner.config import ArbitrageConfig
from degenbot.strategy import validate_strategy_readiness
from tests.fakes.engine import FakeEngineRegistry as _FakeEngineRegistry


@pytest.fixture(autouse=True)
def _rpc_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("DEGENBOT_RPC_HTTP_CHAINID_1", "http://localhost:8545")
    monkeypatch.setenv("DEGENBOT_RPC_WS_CHAINID_1", "ws://localhost:8546")


@pytest.fixture(autouse=True)
def _restore_sigint() -> None:
    yield
    signal.signal(signal.SIGINT, signal.SIG_DFL)


def _noop_coro():
    async def _n() -> None:
        pass

    return _n()


class _FakeBot:
    def __init__(self) -> None:
        self.chain_id = 1
        self.released = False
        self.block_stream_calls = 0

    def release_python_state(self) -> None:
        self.released = True

    def block_stream(self):
        self.block_stream_calls += 1
        return iter(())  # immediately exhausted


class _FakeEth:
    async def get_block(self, block_identifier: str):
        return {"number": 12_345, "baseFeePerGas": 10**9, "gasUsed": 0, "gasLimit": 30_000_000}

    async def get_transaction_count(self, address: str):
        return 7


class _FakeAsyncW3:
    def __init__(self) -> None:
        self.eth = _FakeEth()

    async def get_block(self, block_identifier: str):
        return await self.eth.get_block(block_identifier)

    async def get_transaction_count(self, address: str):
        return await self.eth.get_transaction_count(address)

    async def make_request(self, method: str, params: list):
        return {}

    @property
    def rpc_url(self) -> str:
        return "http://fake:8545"

    def as_async_alloy(self) -> None:
        return None


def _runner(path_builder) -> BotRunner:
    return BotRunner(
        ArbitrageConfig.from_env(
            {
                "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
                "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
                "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
                "INJECT_EXECUTOR_CODE": "0",
            },
            live=True,
            permutation=None,
        ),
        actors=InjectedActors(
            bot=_FakeBot(),
            engine_registry=_FakeEngineRegistry(backfill_target=12_000),
            async_w3=_FakeAsyncW3(),
            snapshots=(None, None, None, None),
            path_builder=path_builder,
            consumer=lambda **kw: _noop_coro(),
            settlement_arm=False,
        ),
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
    records = dict((name, state) for name, state, _halt in session.engine_registry.engine.strategies())
    for facet, active in (
        ("mevblocker_backrun", readiness.mevblocker_backrun_active),
        ("peer_backrun", readiness.peer_backrun_active),
    ):
        assert records[facet] == ("enabled" if active else "registered"), (
            f"{facet}: admission must follow strategy.{facet}.active"
        )
    # The settlement arm's engine state is advisory for its pump arm; it is
    # never gated through enable_strategy.
    assert records["settlement"] == "registered"
