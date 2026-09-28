"""Strategy admission is host-owned (ADR-057).

Selection lives in the per-facet ``strategy.<facet>.active`` flags (ADR-055);
the retired single-arm ``strategy.name`` selector is gone from the Rust schema
(pinned by ``strategy_arm_selector_is_retired``) and the Python config model
that mirrored it is gone too, so there is no second spelling left to refuse.
The settlement runner carries no Python-side arm gate: the host boot registers
each configured facet and ``enable_strategy`` surfaces the typed refusal, so
there is one admission authority. These tests pin the host's typed refusal;
the runner is deliberately silent on the arm.
"""

from __future__ import annotations

import pytest

from degenbot._ffi import ArbitrageEngine, Bot, UnconfiguredStrategyError
from degenbot.runner.bot_runner import BotRunner
from degenbot.strategy import validate_strategy_readiness

STRATEGY_ENV = "DEGENBOT_STRATEGY_NAME"


@pytest.mark.usefixtures("monkeypatch")
class TestAdmissionIsHostOwned:
    def test_runner_carries_no_python_arm_gate(self, monkeypatch):
        """The runner constructs under the retired env; admission is the host's.

        A placeholder cfg is honest here — construction never touches it and no
        runner-side env check runs (the retired env name is inert).
        """
        monkeypatch.setenv(STRATEGY_ENV, "txpool_backrun")
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
        # The registered fleet is the engine's own vocabulary; the view's data
        # says which backrun arm is admitted, so the test restates no facet.
        registered = [name for name, _state, _halt in engine.strategies()]
        active_facets = set(readiness.active_backrun_facets)
        for facet in (name for name in registered if name != "settlement"):
            if facet in active_facets:
                assert engine.enable_strategy(facet) == "enabled"
            else:
                with pytest.raises(UnconfiguredStrategyError):
                    engine.enable_strategy(facet)


@pytest.fixture(autouse=True)
def _no_strategy_env(monkeypatch):
    monkeypatch.delenv(STRATEGY_ENV, raising=False)
