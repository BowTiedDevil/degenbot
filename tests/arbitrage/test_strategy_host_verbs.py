"""Strategy-host operator verbs (Phase C slice C4).

The engine attaches to a host-minted hub; these tests drive the host's driver
FSM through the Python verbs. No network: `Bot` + `ArbitrageEngine` are built
offline, and the verbs touch only the host's in-process records.
"""

from __future__ import annotations

import pytest

from degenbot._ffi import (
    ArbitrageEngine,
    Bot,
    StrategyHostError,
    UnconfiguredStrategyError,
    UnknownStrategyError,
)


@pytest.fixture
def engine() -> ArbitrageEngine:
    return ArbitrageEngine(py_bot=Bot(1))


def test_default_boot_registers_both_strategies_unenabled(engine: ArbitrageEngine) -> None:
    """A settlement-only boot registers both facets; neither is enabled."""
    assert engine.strategies() == [
        ("settlement", "registered", None),
        ("backrun", "registered", None),
    ]


def test_enable_then_disable_walks_the_fsm(engine: ArbitrageEngine) -> None:
    assert engine.enable_strategy("settlement") == "enabled"
    assert engine.strategies()[0] == ("settlement", "enabled", None)

    engine.disable_strategy("settlement")
    assert engine.strategies()[0] == ("settlement", "disabled", None)


def test_a_disabled_tombstone_is_terminal_and_never_restarts(engine: ArbitrageEngine) -> None:
    engine.enable_strategy("settlement")
    engine.disable_strategy("settlement")

    # Disabled is terminal: a second enable is a typed lifecycle refusal, and
    # the record stays frozen.
    with pytest.raises(StrategyHostError):
        engine.enable_strategy("settlement")
    assert engine.strategies()[0] == ("settlement", "disabled", None)


def test_unknown_strategy_raises_a_typed_error(engine: ArbitrageEngine) -> None:
    with pytest.raises(UnknownStrategyError):
        engine.enable_strategy("ghost")
    with pytest.raises(UnknownStrategyError):
        engine.disable_strategy("ghost")


def test_unconfigured_backrun_raises_a_typed_error(engine: ArbitrageEngine) -> None:
    """The default boot names no backrun keys, so enabling it fails loudly."""
    with pytest.raises(UnconfiguredStrategyError):
        engine.enable_strategy("backrun")
    assert engine.strategies()[1] == ("backrun", "registered", None)


def test_errors_share_the_strategy_host_base(engine: ArbitrageEngine) -> None:
    assert issubclass(UnknownStrategyError, StrategyHostError)
    assert issubclass(UnconfiguredStrategyError, StrategyHostError)
    assert issubclass(StrategyHostError, RuntimeError)
