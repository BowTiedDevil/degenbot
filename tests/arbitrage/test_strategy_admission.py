"""Strategy admission is host-owned (ADR-057).

Selection lives in the per-facet ``strategy.<facet>.active`` flags (ADR-055);
the retired single-arm ``strategy.name`` selector is gone from the Rust schema
(pinned by ``strategy_arm_selector_is_retired``) and must be gone from the
Python surface too. The settlement runner carries no Python-side arm gate: the
host boot registers each configured facet and ``enable_strategy`` surfaces the
typed refusal, so there is one admission authority. These tests pin the retired
selector's refusal and the host's typed refusal; the runner is deliberately
silent on the arm.
"""

from __future__ import annotations

import pytest

from degenbot._ffi import ArbitrageEngine, Bot, UnconfiguredStrategyError
from degenbot.config import DegenbotConfig
from degenbot.runner.bot_runner import BotRunner
from degenbot.strategy import validate_strategy_readiness

STRATEGY_ENV = "DEGENBOT_STRATEGY_NAME"


@pytest.mark.usefixtures("monkeypatch")
class TestRetiredStrategySelector:
    def test_init_kwarg_is_refused(self, monkeypatch):
        monkeypatch.delenv(STRATEGY_ENV, raising=False)
        with pytest.raises(ValueError, match="strategy"):
            DegenbotConfig(strategy_name="settlement")

    def test_retired_dotted_file_key_is_refused(self, monkeypatch):
        monkeypatch.delenv(STRATEGY_ENV, raising=False)
        with pytest.raises(ValueError, match="strategy"):
            DegenbotConfig.model_validate({"strategy": {"name": "settlement"}})

    def test_config_exposes_no_strategy_name_attribute(self, monkeypatch):
        monkeypatch.delenv(STRATEGY_ENV, raising=False)
        assert not hasattr(DegenbotConfig(), "strategy_name")

    def test_retired_env_name_is_inert(self, monkeypatch):
        monkeypatch.setenv(STRATEGY_ENV, "settlement")
        assert not hasattr(DegenbotConfig(), "strategy_name")


@pytest.mark.usefixtures("monkeypatch")
class TestAdmissionIsHostOwned:
    def test_runner_carries_no_python_arm_gate(self, monkeypatch):
        """The runner constructs under the retired env; admission is the host's.

        A placeholder cfg is honest here — construction never touches it and no
        runner-side env check runs (the retired env name is inert).
        """
        monkeypatch.setenv(STRATEGY_ENV, "peer_backrun")
        assert BotRunner(None) is not None  # type: ignore[arg-type]

    def test_host_admission_follows_the_facet_config(self):
        """The host's typed refusal is the one admission surface, and it reads
        the SAME facet activation the readiness resolution reports: enabled
        iff `strategy.<facet>.active`. The test derives the expectation from
        that resolution rather than pinning ambient holder state — the boot
        installs the ambient config, so a workspace with an activated backrun
        facet must admit it exactly as a defaulted one refuses."""
        engine = ArbitrageEngine(py_bot=Bot(1))
        readiness = validate_strategy_readiness()
        for facet, active in (
            ("mevblocker_backrun", readiness.mevblocker_backrun_active),
            ("peer_backrun", readiness.peer_backrun_active),
        ):
            if active:
                engine.enable_strategy(facet)
            else:
                with pytest.raises(UnconfiguredStrategyError):
                    engine.enable_strategy(facet)


@pytest.fixture(autouse=True)
def _no_strategy_env(monkeypatch):
    monkeypatch.delenv(STRATEGY_ENV, raising=False)
