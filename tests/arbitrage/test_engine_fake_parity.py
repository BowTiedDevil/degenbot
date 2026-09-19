"""Parity between the shared engine fake and the real ``ArbitrageEngine``.

The Python driver shell depends on a narrow slice of the engine surface. This
module binds that slice in one place: the fake implements every seam member,
the real engine exposes every seam member, the stub declares every seam member,
and the fake defines none of the retired methods. A surface change that breaks
any of those identities fails here instead of drifting silently.

Where a real path exists the fake is also driven against the real engine's
behaviour (default registration order and the enable/disable vocabulary), so
the double cannot quietly grow a different posture.
"""

from __future__ import annotations

import asyncio
import re
from pathlib import Path

import pytest

from degenbot._ffi import ArbitrageEngine, Bot
from degenbot.runner._consume import consume_result_batches
from degenbot.runner.bot_runner import _SessionState
from degenbot.runner.config import ArbitrageConfig
from tests.fakes.engine import (
    ENGINE_SEAM_MEMBERS,
    RETIRED_ENGINE_MEMBERS,
    FakeEngine,
    FakeEngineRegistry,
)
from tests.fakes.runner_pipelines import StubPipeline

_REPO_ROOT = Path(__file__).resolve().parents[2]
_STUB = _REPO_ROOT / "src/degenbot/_ffi/__init__.pyi"


@pytest.fixture(autouse=True)
def _rpc_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("DEGENBOT_RPC_HTTP_CHAINID_1", "http://localhost:8545")
    monkeypatch.setenv("DEGENBOT_RPC_WS_CHAINID_1", "ws://localhost:8546")


def _cfg() -> ArbitrageConfig:
    return ArbitrageConfig.from_env(
        {
            "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
            "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
            "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
            "INJECT_EXECUTOR_CODE": "0",
        },
        live=True,
        permutation=None,
    )


def _stub_arbitrage_engine_members() -> set[str]:
    source = _STUB.read_text(encoding="utf-8")
    block = source.split("class ArbitrageEngine:", 1)[1].split("\nclass ", 1)[0]
    return set(re.findall(r"^    def (\w+)", block, flags=re.MULTILINE))


def test_fake_implements_every_seam_member() -> None:
    missing = [name for name in ENGINE_SEAM_MEMBERS if not hasattr(FakeEngine, name)]
    assert missing == [], f"FakeEngine is missing seam members: {missing}"


def test_real_engine_exposes_every_seam_member() -> None:
    missing = [
        name for name in ENGINE_SEAM_MEMBERS if not hasattr(ArbitrageEngine, name)
    ]
    assert missing == [], f"ArbitrageEngine no longer exposes seam members: {missing}"


def test_stub_declares_every_seam_member() -> None:
    declared = _stub_arbitrage_engine_members()
    missing = [name for name in ENGINE_SEAM_MEMBERS if name not in declared]
    assert missing == [], f"the ArbitrageEngine stub is missing seam members: {missing}"


def test_fake_defines_no_retired_engine_surface() -> None:
    regrown = [name for name in RETIRED_ENGINE_MEMBERS if hasattr(FakeEngine, name)]
    assert regrown == [], f"FakeEngine regrew retired engine surface: {regrown}"
    for name in RETIRED_ENGINE_MEMBERS:
        assert not hasattr(ArbitrageEngine, name), (
            f"retired member {name} is back on the real engine"
        )


def test_fake_and_real_agree_on_default_registration_order() -> None:
    real = ArbitrageEngine(py_bot=Bot(1))
    assert FakeEngine().strategies() == real.strategies() == [
        ("settlement", "registered", None),
        ("backrun", "registered", None),
    ]


def test_fake_and_real_agree_on_enable_disable_vocabulary() -> None:
    real = ArbitrageEngine(py_bot=Bot(1))
    fake = FakeEngine()

    assert fake.enable_strategy("settlement") == real.enable_strategy("settlement") == "enabled"
    assert fake.strategies()[0] == real.strategies()[0] == ("settlement", "enabled", None)

    fake.disable_strategy("settlement")
    real.disable_strategy("settlement")
    assert fake.strategies()[0] == real.strategies()[0] == ("settlement", "disabled", None)


class _AlloyW3:
    """Async w3 double whose provider arm is present (the reconcile precondition)."""

    async def get_block(self, block_identifier: str) -> dict[str, object]:
        return {
            "number": 12_345,
            "baseFeePerGas": 10**9,
            "gasUsed": 0,
            "gasLimit": 30_000_000,
        }

    def as_async_alloy(self) -> object:
        return object()


class _Dispatcher:
    current_block = 12_345

    def record_block_time(self, block_number: int, block_timestamp: int) -> None:
        pass

    def block_time_count(self) -> int:
        return 0

    def block_times_oldest(self) -> tuple[int, int]:  # pragma: no cover - gate off
        return (0, 0)

    def advance_block(self, block_number: int) -> None:
        self.current_block = block_number

    def block_timestamp_for(self, block_number: int) -> int | None:
        return 1_700_000_000

    def discard_path(self, path_id: int) -> None:
        pass


class _AsyncOnce:
    def __init__(self, item: object) -> None:
        self._item = item
        self._done = False

    def __aiter__(self) -> _AsyncOnce:
        return self

    async def __anext__(self) -> object:
        if self._done:
            raise StopAsyncIteration
        self._done = True
        return self._item


def _tick(number: int = 12_347) -> dict[str, int]:
    return {
        "number": number,
        "timestamp": 1_700_000_000 + number,
        "base_fee_per_gas": 1_000_000_000,
        "gas_used": 15_000_000,
        "gas_limit": 30_000_000,
    }


def _empty_batch() -> dict[str, object]:
    return {
        "fresh": [],
        "updated": [],
        "removed": [],
        "solve_block": 12_346,
        "base_fee_per_gas": 1_000_000_000,
        "gas_used": 15_000_000,
        "gas_limit": 30_000_000,
    }


async def _drive_one_head(engine: FakeEngine) -> None:
    session = _SessionState(
        engine_registry=FakeEngineRegistry(engine),  # type: ignore[arg-type]
        async_w3=_AlloyW3(),  # type: ignore[arg-type]
        sim_ctx=None,
        dispatcher=_Dispatcher(),  # type: ignore[arg-type]
        cfg=_cfg(),
        current_block=12_345,
    )
    await consume_result_batches(
        session,
        block_stream=_AsyncOnce(_tick()),
        result_iter=_AsyncOnce(_empty_batch()),
        allow_quiet_end=True,
    )


async def test_head_tick_reconciles_once(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def _noop_fee_history(**kwargs: object) -> None:
        await asyncio.sleep(0)

    monkeypatch.setattr("degenbot.runner._consume.fetch_fee_history", _noop_fee_history)
    monkeypatch.setattr("degenbot.runner._consume.SimSubmitPipeline", StubPipeline)
    StubPipeline.instances.clear()

    engine = FakeEngine(hosted_activity=True)
    await _drive_one_head(engine)

    assert len(engine.reconcile_calls) == 1, (
        "the accepted head must invoke the hosted reconcile exactly once"
    )
    assert engine.reconcile_calls[0]["operator_address"] == _cfg().operator_address


async def test_reconcile_guard_short_circuits_without_hosted_activity(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def _noop_fee_history(**kwargs: object) -> None:
        await asyncio.sleep(0)

    monkeypatch.setattr("degenbot.runner._consume.fetch_fee_history", _noop_fee_history)
    monkeypatch.setattr("degenbot.runner._consume.SimSubmitPipeline", StubPipeline)
    StubPipeline.instances.clear()

    engine = FakeEngine(hosted_activity=False)
    await _drive_one_head(engine)
    assert engine.reconcile_calls, "the reconcile call still runs on every head"
    assert engine.reconcile_chain_reads == 0, "the closed guard pays no chain read"

    engine.hosted_activity = True
    await _drive_one_head(engine)
    assert engine.reconcile_chain_reads == 1, "an open guard performs the chain read"
