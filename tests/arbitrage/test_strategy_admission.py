"""Strategy admission is host-owned (ADR-057).

``strategy.name`` selects exactly one arm (ADR-055 D5: per-strategy ``.enabled``
and runtime registration belong to the Phase C host). The settlement runner
carries no Python-side arm gate: the host boot registers each configured facet
and ``enable_strategy`` surfaces the typed refusal, so there is one admission
authority. These tests pin the config-layer env accessor and the host's typed
refusal; the runner itself is deliberately silent on the arm.
"""

from __future__ import annotations

import pytest

from degenbot._ffi import ArbitrageEngine, Bot, UnconfiguredStrategyError
from degenbot.config import DegenbotConfig, strategy_arm_from_env
from degenbot.runner.bot_runner import BotRunner

STRATEGY_ENV = "DEGENBOT_STRATEGY_NAME"


@pytest.mark.usefixtures("monkeypatch")
class TestStrategyArmEnv:
    def test_env_unset_selects_default_arm(self, monkeypatch):
        monkeypatch.delenv(STRATEGY_ENV, raising=False)
        assert strategy_arm_from_env() is None

    @pytest.mark.parametrize(
        "value", ["settlement", "mevblocker_backrun", "peer_backrun"]
    )
    def test_env_accepts_typed_names(self, monkeypatch, value):
        monkeypatch.setenv(STRATEGY_ENV, value)
        assert strategy_arm_from_env() == value

    def test_env_rejects_unknown_names(self, monkeypatch):
        monkeypatch.setenv(STRATEGY_ENV, "v6-sniper")
        with pytest.raises(ValueError, match=STRATEGY_ENV):
            strategy_arm_from_env()

    def test_env_rejects_the_retired_backrun_name(self, monkeypatch):
        monkeypatch.setenv(STRATEGY_ENV, "backrun")
        with pytest.raises(ValueError, match=STRATEGY_ENV):
            strategy_arm_from_env()


@pytest.mark.usefixtures("monkeypatch")
class TestStrategyArmField:
    def test_config_carries_strategy_name(self, monkeypatch):
        monkeypatch.delenv(STRATEGY_ENV, raising=False)
        cfg = DegenbotConfig()
        assert cfg.strategy_name is None

    def test_config_file_layer_accepts_typed_value(self, monkeypatch):
        monkeypatch.delenv(STRATEGY_ENV, raising=False)
        cfg = DegenbotConfig(strategy_name="settlement")
        assert cfg.strategy_name == "settlement"
        cfg = DegenbotConfig(strategy_name="mevblocker_backrun")
        assert cfg.strategy_name == "mevblocker_backrun"
        cfg = DegenbotConfig(strategy_name="peer_backrun")
        assert cfg.strategy_name == "peer_backrun"


@pytest.mark.usefixtures("monkeypatch")
class TestAdmissionIsHostOwned:
    def test_runner_carries_no_python_arm_gate(self, monkeypatch):
        """The runner constructs under the peer-backrun env; admission is the host's.

        A placeholder cfg is honest here — construction never touches it and no
        runner-side env check runs.
        """
        monkeypatch.setenv(STRATEGY_ENV, "peer_backrun")
        assert BotRunner(None) is not None  # type: ignore[arg-type]

    def test_host_refuses_unconfigured_backrun(self):
        """The host's typed refusal is the one admission surface."""
        engine = ArbitrageEngine(py_bot=Bot(1))
        with pytest.raises(UnconfiguredStrategyError):
            engine.enable_strategy("mevblocker_backrun")
        with pytest.raises(UnconfiguredStrategyError):
            engine.enable_strategy("peer_backrun")


@pytest.fixture(autouse=True)
def _no_strategy_env(monkeypatch):
    monkeypatch.delenv(STRATEGY_ENV, raising=False)
