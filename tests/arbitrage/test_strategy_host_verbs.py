"""Strategy-host operator verbs (Phase C slice C4).

The engine attaches to a host-minted hub; these tests drive the host's driver
FSM through the Python verbs. No network: `Bot` + `ArbitrageEngine` are built
offline, and the verbs touch only the host's in-process records.
"""

from __future__ import annotations

import re
from pathlib import Path

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


def test_default_boot_registers_all_strategies_unenabled(engine: ArbitrageEngine) -> None:
    """The default boot registers settlement plus both backrun facets; none is enabled."""
    assert engine.strategies() == [
        ("settlement", "registered", None),
        ("mevblocker_backrun", "registered", None),
        ("peer_backrun", "registered", None),
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
    """The default boot names no backrun keys, so enabling one fails loudly."""
    with pytest.raises(UnconfiguredStrategyError):
        engine.enable_strategy("peer_backrun")
    assert engine.strategies()[2] == ("peer_backrun", "registered", None)


def test_errors_share_the_strategy_host_base(engine: ArbitrageEngine) -> None:
    assert issubclass(UnknownStrategyError, StrategyHostError)
    assert issubclass(UnconfiguredStrategyError, StrategyHostError)
    assert issubclass(StrategyHostError, RuntimeError)


# The host FSM's state vocabulary lives ONCE in Rust. These helpers read the
# two Rust sources as text so the parity test binds the Python-facing names to
# the Rust enum rather than re-encoding them as a second hardcoded literal.
_REPO_ROOT = Path(__file__).resolve().parents[2]
_STRATEGY_HOST_RS = _REPO_ROOT / "rust/crates/degenbot-bot/src/strategy_host.rs"
_PY_STRATEGY_RS = _REPO_ROOT / "rust/crates/degenbot-python/src/bot/engine/strategy.rs"


def _rust_driver_pose_variants() -> set[str]:
    source = _STRATEGY_HOST_RS.read_text()
    block = source.split("pub enum DriverPose {", 1)[1].split("}", 1)[0]
    return set(re.findall(r"^\s*([A-Z][A-Za-z0-9]*),", block, flags=re.MULTILINE))


def _python_state_names() -> dict[str, str]:
    source = _PY_STRATEGY_RS.read_text()
    block = source.split("fn state_name(", 1)[1].split("}", 1)[0]
    return dict(re.findall(r'DriverPose::([A-Za-z0-9]+)\s*=>\s*"([a-z]+)"', block))


def test_python_state_vocabulary_binds_to_the_rust_driver_pose_enum() -> None:
    """The Python state names are derived from the Rust FSM, not duplicated.

    The host FSM transition table and its state vocabulary are pinned once in
    ``strategy_host.rs``. This test reads that enum and the Python translation
    in ``strategy.rs`` so a rename or an added state cannot desynchronize while
    both suites stay green.
    """
    rust_variants = _rust_driver_pose_variants()
    python_names = _python_state_names()
    assert rust_variants, "the Rust DriverPose enum must be source-readable"
    assert set(python_names) == rust_variants, (
        f"the Python state_name map must cover exactly the Rust DriverPose "
        f"variants: python={sorted(python_names)} rust={sorted(rust_variants)}"
    )
    assert len(set(python_names.values())) == len(python_names), (
        "each Rust driver state needs a distinct Python name"
    )
    assert all(name.islower() for name in python_names.values())
