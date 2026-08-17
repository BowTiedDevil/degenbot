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
import logging.handlers

import degenbot  # ruff: ignore[unused-import]  (import triggers base config)

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
    # The in-process sim engine + the backrun strategy. The divergence probe
    # (``[sim-divergence]``, ergo task 4C33DP / epic TR6GWT) + the bridge-probe
    # (``[bridge-probe]``) emit ``log::info!`` from these crates; the base
    # config in ``degenbot.logging`` lowers them so the records are visible.
    "degenbot_simulation",
    "degenbot_arbitrage",
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


# ── Async-logging decoupling (GIL-hold-across-stdout-flush class) ─────────
#
# A Rust-side ``log::info!`` (e.g. ``[debug-v4-solve]``, ``[sim]``, ``[verify]``)
# is bridged to Python via ``pyo3_log::init()`` (see ``degenbot-python/src/
# lib.rs``). ``pyo3-log``'s ``Log::log`` impl acquires the GIL with
# ``Python::attach`` for EVERY enabled record and runs the full Python
# ``logging`` pipeline under the GIL. When stdout is piped (e.g. ``run_bot.sh``
# redirects stdout as ``> >(tee -a "$LOG" > /dev/null) 2>&1``), ``sys.stdout``
# is block-buffered; ``StreamHandler.emit`` calls ``stream.flush()`` which
# blocks on a full pipe or on the ``BufferedWriter._write_lock`` futex under
# concurrent writers — holding the GIL across the I/O wait. Under a burst of
# concurrent Rust log calls (chatty V4 solve debug, multi-candidate dispatch)
# this stalls every thread waiting on the GIL, including the asyncio main loop
# parked at ``await dispatch_profitable_py(...)`` whose Rust future is the one
# parked mid-``log::info!``. The in-loop header-staleness watchdog cannot fire
# because the pump's tokio worker is parked before its select.
#
# The canonical stdlib-blessed decoupling: attach ``QueueHandler`` to every
# configured logger and drain via a dedicated ``QueueListener`` thread that
# owns the only ``StreamHandler`` / stdout write path. The producer side
# (``QueueHandler.emit``) does a fast non-blocking ``put_nowait`` — no stream
# I/O, no GIL held across slow writes; the listener thread is the only thread
# that ever calls ``stream.flush()`` (single writer → no lock contention).


def test_configured_loggers_attach_queue_handler_not_stream_handler() -> None:
    """The GIL-held-across-flush stall class is closed by routing records
    through ``QueueHandler`` instead of a bare ``StreamHandler``.

    A direct ``StreamHandler(sys.stdout)`` on a piped stdout blocks on
    ``stream.flush()`` under pyo3-log's ``Python::attach`` → GIL held across
    the I/O wait. ``QueueHandler.emit`` is a fast non-blocking queue put —
    no stream I/O, no GIL held across slow writes.
    """
    import degenbot.logging as dl
    from degenbot.logging import (
        PY_PACKAGE_ROOT_LOGGER_NAMES,
        RUST_BRIDGE_LOGGER_NAMES,
    )

    # Pytest's logging plugin attaches its own ``LogCaptureHandler`` (a
    # ``StreamHandler`` subclass) to each logger at test time for capture —
    # that is test-only infra, not the production config, so we filter it
    # out (module starts with ``_pytest.``) and check the contract on the
    # handlers degenbot's base config actually attached: the ``QueueHandler``
    # is on every configured logger, and the bare ``_STDOUT_HANDLER`` is on
    # NONE (it lives only as a ``QueueListener`` destination).
    for name in (*RUST_BRIDGE_LOGGER_NAMES, *PY_PACKAGE_ROOT_LOGGER_NAMES, "degenbot.logging"):
        lg = logging.getLogger(name)
        own_handlers = [h for h in lg.handlers if not type(h).__module__.startswith("_pytest.")]
        assert any(h is dl._QUEUED_HANDLER for h in own_handlers), (
            f"{name} is not attached to degenbot.logging._QUEUED_HANDLER — records "
            "flow directly to a StreamHandler and the GIL is held across stdout "
            "flush (the piped-stdout stall class)"
        )
        assert dl._STDOUT_HANDLER not in own_handlers, (
            f"{name} attaches _STDOUT_HANDLER directly — records bypass the queue "
            "and block on stdout flush under the GIL"
        )


def test_queue_listener_started_and_drains_to_stdout_handler() -> None:
    """A dedicated listener thread owns the only stdout write path.

    ``QueueListener``'s listener is the single writer — slow ``os.write`` to a
    full pipe blocks only the listener thread (whose sole job is writing), not
    any GIL-holding Rust log call. The listener MUST be started at import
    (``is_alive()``) so records are never silently queued-and-dropped.
    """
    import degenbot.logging as dl

    assert hasattr(dl, "_LOG_LISTENER"), (
        "degenbot.logging does not expose _LOG_LISTENER — no queue drain thread"
    )
    listener = dl._LOG_LISTENER
    assert isinstance(listener, logging.handlers.QueueListener), (
        f"_LOG_LISTENER is {type(listener).__name__}, not a QueueListener"
    )
    assert listener._thread is not None, (
        "_LOG_LISTENER thread is None — QueueListener was never started"
    )
    assert listener._thread.is_alive(), (
        "_LOG_LISTENER thread is not running — queued records would never drain"
    )


def test_rust_info_record_still_reaches_handler_via_queue() -> None:
    """Regression guard: routing through the queue does NOT drop records.

    The contract ``test_rust_info_record_is_visible`` pins (an INFO record on
    a deep Rust target reaches the crate-root handler) must still hold after
    the QueueHandler/QueueListener decoupling — the listener must forward
    through to its destination handler.
    """
    import degenbot.logging as dl
    from degenbot.logging import RUST_BRIDGE_LOGGER_NAMES

    crate_root = RUST_BRIDGE_LOGGER_NAMES[0]
    capture = _CaptureHandler()
    # Attach the capture as a QueueListener destination so the record is
    # observed after the listener thread drains the queue.
    listener = dl._LOG_LISTENER
    listener.handlers = (*listener.handlers, capture)
    try:
        logging.getLogger(f"{crate_root}.bot_core.block_pump").info(
            "queue-bridged info %s", crate_root
        )
        # Drain: the listener thread processes the queue asynchronously.
        # A bounded retry loop because the listener runs on its own thread.
        import time

        for _ in range(100):
            visible = [r for r in capture.records if r.levelno == logging.INFO]
            if any("queue-bridged" in r.getMessage() for r in visible):
                return
            time.sleep(0.01)
        msg = (
            f"INFO record on {crate_root} did not reach the listener's "
            "destination handler via the queue - records are lost"
        )
        raise AssertionError(msg)
    finally:
        listener.handlers = tuple(h for h in listener.handlers if h is not capture)
