"""Settled-block arm gate: a settlement runner may never boot under strategy.name=backrun (ADR-055, X6P5GN).

``strategy.name`` selects exactly one arm (ADR-055 D5: per-strategy .enabled
and runtime registration belong to the Phase C host). Python's settlement
runner consuming DEGENBOT_STRATEGY_NAME loudly when it selects the backrun
arm is the interim refusal behavior — the operator must boot the sidecar
instead.
"""

from __future__ import annotations

import pytest

from degenbot.config import DegenbotConfig, strategy_arm_from_env
from degenbot.runner.bot_runner import BotRunner

STRATEGY_ENV = "DEGENBOT_STRATEGY_NAME"


@pytest.mark.usefixtures("monkeypatch")
class TestStrategyArmEnv:
    def test_env_unset_selects_default_arm(self, monkeypatch):
        monkeypatch.delenv(STRATEGY_ENV, raising=False)
        assert strategy_arm_from_env() is None

    @pytest.mark.parametrize("value", ["settlement", "backrun"])
    def test_env_accepts_typed_names(self, monkeypatch, value):
        monkeypatch.setenv(STRATEGY_ENV, value)
        assert strategy_arm_from_env() == value

    def test_env_rejects_unknown_names(self, monkeypatch):
        monkeypatch.setenv(STRATEGY_ENV, "v6-sniper")
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
        cfg = DegenbotConfig(strategy_name="backrun")
        assert cfg.strategy_name == "backrun"


@pytest.mark.usefixtures("monkeypatch")
class TestSettlementRunnerGate:
    def test_settlement_runner_refuses_backrun_env(self, monkeypatch):
        monkeypatch.setenv(STRATEGY_ENV, "backrun")
        # The arm gate fires before any cfg attribute access, so a placeholder
        # is honest here; a real ArbitrageConfig is heavy and irrelevant.
        with pytest.raises(ValueError, match="settlement runner refuses"):
            BotRunner(None)  # type: ignore[arg-type]
