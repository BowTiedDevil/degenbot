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

import re
from pathlib import Path

import pytest

from degenbot._ffi import ArbitrageEngine, Bot
from degenbot.runner.config import ArbitrageConfig
from degenbot.strategy import validate_strategy_readiness as readiness
from tests.fakes.engine import (
    ENGINE_SEAM_MEMBERS,
    RETIRED_ENGINE_MEMBERS,
    FakeEngine,
)
from tests.helpers.identity_env import identity_env
from tests.helpers.rpc_env import rpc_env

_REPO_ROOT = Path(__file__).resolve().parents[2]
_STUB = _REPO_ROOT / "src/degenbot/_ffi/__init__.pyi"


@pytest.fixture(autouse=True)
def _rpc_env(monkeypatch: pytest.MonkeyPatch) -> None:
    rpc_env(monkeypatch)


def _cfg() -> ArbitrageConfig:
    with identity_env({
        "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
        "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
        "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
    }):
        return ArbitrageConfig.build(live=True, permutation=None)


def _stub_arbitrage_engine_members() -> set[str]:
    source = _STUB.read_text(encoding="utf-8")
    block = source.split("class ArbitrageEngine:", 1)[1].split("\nclass ", 1)[0]
    return set(re.findall(r"^    def (\w+)", block, flags=re.MULTILINE))


def test_fake_implements_every_seam_member() -> None:
    missing = [name for name in ENGINE_SEAM_MEMBERS if not hasattr(FakeEngine, name)]
    assert missing == [], f"FakeEngine is missing seam members: {missing}"


def test_real_engine_exposes_every_seam_member() -> None:
    missing = [name for name in ENGINE_SEAM_MEMBERS if not hasattr(ArbitrageEngine, name)]
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
    assert (
        FakeEngine().strategies()
        == real.strategies()
        == [
            ("settlement", "registered", None),
            ("mevblocker_backrun", "registered", None),
            ("txpool_backrun", "registered", None),
        ]
    )


def test_fake_and_real_agree_on_enable_disable_vocabulary() -> None:
    """Drives the vocabulary walk on a facet the ambient config admits: the
    real engine's admission reads `strategy.<facet>.active`, so an inactive
    facet refuses and the fake — a pure FSM double — cannot mirror that."""
    real = ArbitrageEngine(py_bot=Bot(1))
    fake = FakeEngine()

    view = readiness()
    candidates = list(view.active_backrun_facets)
    if view.settlement_active:
        candidates.insert(0, "settlement")
    facet = next(iter(candidates), None)
    if facet is None:
        pytest.skip(
            "the ambient config activates no facet; the real engine's "
            "admission refuses every enable until one is activated"
        )

    assert fake.enable_strategy(facet) == real.enable_strategy(facet) == "enabled"
    fake_state = next(r for r in fake.strategies() if r[0] == facet)
    real_state = next(r for r in real.strategies() if r[0] == facet)
    assert fake_state == real_state == (facet, "enabled", None)

    fake.disable_strategy(facet)
    real.disable_strategy(facet)
    fake_state = next(r for r in fake.strategies() if r[0] == facet)
    real_state = next(r for r in real.strategies() if r[0] == facet)
    assert fake_state == real_state == (facet, "disabled", None)
