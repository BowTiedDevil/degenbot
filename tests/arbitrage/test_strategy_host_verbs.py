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
from degenbot.strategy import validate_strategy_readiness


@pytest.fixture
def engine() -> ArbitrageEngine:
    return ArbitrageEngine(py_bot=Bot(1))


def _active_facet(engine: ArbitrageEngine) -> str:
    """A facet the ambient holder config admits, for FSM-walk tests.

    Admission is stance-independent over `strategy.<facet>.active` (the boot
    installs the ambient config in every Python process), so the walk needs a
    facet whose key is actually on; the probe runs on a THROWAWAY engine so
    the caller's instance stays untouched (a disabled facet is terminal and
    would poison the walk under test).
    """
    readiness = validate_strategy_readiness()
    for facet, key in (
        ("settlement", readiness.settlement_active),
        ("mevblocker_backrun", readiness.mevblocker_backrun_active),
        ("txpool_backrun", readiness.txpool_backrun_active),
    ):
        if not key:
            continue
        probe = ArbitrageEngine(py_bot=Bot(1))
        try:
            probe.enable_strategy(facet)
        except StrategyHostError:
            continue
        return facet
    pytest.skip(
        "the ambient config activates no admissible facet; stance-independent "
        "admission refuses every enable until one is activated"
    )


def test_default_boot_registers_all_strategies_unenabled(engine: ArbitrageEngine) -> None:
    """The default boot registers settlement plus both backrun facets; none is enabled."""
    assert engine.strategies() == [
        ("settlement", "registered", None),
        ("mevblocker_backrun", "registered", None),
        ("txpool_backrun", "registered", None),
    ]


def test_enable_then_disable_walks_the_fsm(engine: ArbitrageEngine) -> None:
    facet = _active_facet(engine)
    assert engine.enable_strategy(facet) == "enabled"
    record = next(r for r in engine.strategies() if r[0] == facet)
    assert record[1] == "enabled"

    engine.disable_strategy(facet)
    record = next(r for r in engine.strategies() if r[0] == facet)
    assert record[1] == "disabled"


def test_a_disabled_tombstone_is_terminal_and_never_restarts(engine: ArbitrageEngine) -> None:
    facet = _active_facet(engine)
    engine.enable_strategy(facet)
    engine.disable_strategy(facet)

    # Disabled is terminal: a second enable is a typed lifecycle refusal, and
    # the record stays frozen.
    with pytest.raises(StrategyHostError):
        engine.enable_strategy(facet)
    record = next(r for r in engine.strategies() if r[0] == facet)
    assert record[1] == "disabled"


def test_unknown_strategy_raises_a_typed_error(engine: ArbitrageEngine) -> None:
    with pytest.raises(UnknownStrategyError):
        engine.enable_strategy("ghost")
    with pytest.raises(UnknownStrategyError):
        engine.disable_strategy("ghost")


def test_unconfigured_backrun_raises_a_typed_error(engine: ArbitrageEngine) -> None:
    """The default boot names no backrun keys, so enabling one fails loudly."""
    with pytest.raises(UnconfiguredStrategyError):
        engine.enable_strategy("txpool_backrun")
    assert engine.strategies()[2] == ("txpool_backrun", "registered", None)


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
