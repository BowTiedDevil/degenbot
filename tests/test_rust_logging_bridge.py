"""Base-config visibility of Rust-side ``log::`` statements.

The Rust extension emits logs through the ``log`` crate (e.g. ``log::info!`` in
``block_pump.rs`` / ``verify.rs`` / ``register.rs``). ``pyo3-log`` bridges these
into Python's ``logging`` module by forwarding each record to
``logging.getLogger(<rust target with :: -> .>)`` — e.g. a ``log::info!`` in
``degenbot_bot::bot_core::block_pump`` lands on the Python logger named
``degenbot_bot.bot_core.block_pump``.

These tests pin the *base-config* contract: importing ``degenbot`` must, by
itself, make those Rust records visible at INFO without any caller wiring.
"""

import logging

import degenbot  # noqa: F401  (import triggers base config)

#: The Rust crate-root logger names that ``pyo3-log`` forwards ``log::`` records
#: into (each Rust target ``degenbot_<crate>::...`` maps to the Python logger
#: ``degenbot_<crate>.<...>``; the dotted crate root is the top ancestor whose
#: level/handlers gate every descendant).
RUST_BRIDGE_LOGGER_NAMES = (
    "degenbot_bot",
    "degenbot_core",
    # The PyO3 binding crate lives in ``crates/degenbot-python/`` but its
    # Cargo ``name`` is ``degenbot_rs`` (set in its ``Cargo.toml``), so every
    # bare ``log::info!`` in that crate emits under ``degenbot_rs::...`` →
    # Python logger ``degenbot._ffi.<...>``. The directory name ``degenbot_python``
    # is never a Rust target — using it here drops the ``[verify]`` and
    # register/snapshot logs.
    "degenbot_rs",
    "degenbot_rpc",
    "degenbot_decoders",
    "degenbot_uniswap",
)

#: The same contract is exported by ``degenbot.logging`` so other code (e.g. the
#: pytest conftest) can drive the Rust loggers through one knob.
try:
    from degenbot.logging import RUST_BRIDGE_LOGGER_NAMES as _EXPORTED_NAMES
except ImportError:  # pragma: no cover - red until implemented
    _EXPORTED_NAMES = None

#: A representative deep Rust target (crate-root + two path segments), matching
#: the real ``degenbot_bot::bot_core::block_pump`` target that ``pyo3-log``
#: forwards into Python as ``degenbot_bot.bot_core.block_pump``.
_SAMPLE_RUST_TARGETS = tuple(f"{root}.bot_core.block_pump" for root in RUST_BRIDGE_LOGGER_NAMES)


class _CaptureHandler(logging.Handler):
    def __init__(self) -> None:
        super().__init__(level=logging.DEBUG)
        self.records: list[logging.LogRecord] = []

    def emit(self, record: logging.LogRecord) -> None:
        self.records.append(record)


def test_rust_bridge_logger_names_exported() -> None:
    """``degenbot.logging`` exports the canonical set for conftest/external use."""
    assert _EXPORTED_NAMES is not None, "degenbot.logging does not export RUST_BRIDGE_LOGGER_NAMES"
    assert set(_EXPORTED_NAMES) == set(RUST_BRIDGE_LOGGER_NAMES)


def test_rust_bridge_loggers_configured_at_info_or_lower() -> None:
    """Base config lowers every Rust crate-root logger to <= INFO."""
    for name in RUST_BRIDGE_LOGGER_NAMES:
        logger = logging.getLogger(name)
        assert logger.getEffectiveLevel() <= logging.INFO, (
            f"{name} effective level is {logging.getLevelName(logger.getEffectiveLevel())}; "
            "INFO-level Rust logs would be suppressed"
        )


def test_rust_bridge_loggers_have_a_handler() -> None:
    """Each Rust crate-root logger has a handler so records actually emit."""
    for name in RUST_BRIDGE_LOGGER_NAMES:
        logger = logging.getLogger(name)
        assert logger.handlers, f"{name} has no handler — Rust records would never emit"


def test_rust_info_record_is_visible() -> None:
    """An INFO record on a deep Rust target reaches a handler on its crate root."""
    for target in _SAMPLE_RUST_TARGETS:
        crate_root = target.split(".", 1)[0]
        root_logger = logging.getLogger(crate_root)
        capture = _CaptureHandler()
        root_logger.addHandler(capture)
        try:
            logging.getLogger(target).info("rust-side info %s", target)
        finally:
            root_logger.removeHandler(capture)
        visible = [r for r in capture.records if r.levelno == logging.INFO]
        assert any("rust-side info" in r.getMessage() for r in visible), (
            f"INFO record on {target} was suppressed by base config"
        )


# ── GTOD23-YBEYKY (T4): arbitrage-subtree visibility ──────────────────────

#: Python modules under ``degenbot.arbitrage.*`` (e.g. ``recurring_verify``)
#: use ``logging.getLogger("degenbot.arbitrage.recurring_verify")`` — they
#: inherit from the ``degenbot`` package logger. S2 found these records were
#: SILENCED (the package logger ``degenbot.logging`` has ``propagate=False``
#: and is a sibling, not an ancestor, of ``degenbot.arbitrage.*``). The base
#: config must make the ``degenbot`` package subtree visible so recurring-verify
#: ``[verify] (recurring)`` lines reach stdout/file.
_SAMPLE_ARBITRAGE_TARGETS = (
    "degenbot.arbitrage.recurring_verify",
    "degenbot.arbitrage.engine_registry",
)


def test_degenbot_arbitrage_loggers_configured_at_info_or_lower() -> None:
    """Base config lowers the ``degenbot`` package subtree to <= INFO.

    S2 (GTOD23-PB24RX) found the recurring verifier's ``[verify] (recurring)``
    lines were dropped: the module logs under
    ``logging.getLogger("degenbot.arbitrage.recurring_verify")`` which inherits
    from ``degenbot`` — but base config only configured the Rust bridge roots +
    the ``degenbot.logging`` package logger (a sibling, not an ancestor).
    """
    for name in _SAMPLE_ARBITRAGE_TARGETS:
        logger = logging.getLogger(name)
        assert logger.getEffectiveLevel() <= logging.INFO, (
            f"{name} effective level is {logging.getLevelName(logger.getEffectiveLevel())}; "
            "recurring-verify [verify] (recurring) INFO lines would be suppressed"
        )


def test_degenbot_arbitrage_info_record_reaches_handler() -> None:
    """An INFO record on a ``degenbot.arbitrage.*`` target reaches a handler.

    This is the contract the recurring verifier relies on — without it, the
    drift-detection success/mismatch lines never reach stdout and the analyzer
    sees no recurring activity.
    """
    for target in _SAMPLE_ARBITRAGE_TARGETS:
        pkg_logger = logging.getLogger("degenbot")
        capture = _CaptureHandler()
        pkg_logger.addHandler(capture)
        try:
            logging.getLogger(target).info("[verify] (recurring) checking at block %d", 25398650)
        finally:
            pkg_logger.removeHandler(capture)
        visible = [r for r in capture.records if r.levelno == logging.INFO]
        assert any("[verify] (recurring)" in r.getMessage() for r in visible), (
            f"INFO record on {target} did NOT reach the `degenbot` package handler "
            "— recurring-verify lines are silenced (GTOD23-YBEYKY regression)"
        )
