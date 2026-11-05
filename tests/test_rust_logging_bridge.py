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

import io
import logging
import sys
import threading

import degenbot  # ruff: ignore[unused-import]  (import triggers base config)

#: The Rust crate-root logger names that ``pyo3-log`` forwards ``log::`` records
#: into (each Rust target ``degenbot_<crate>::...`` maps to the Python logger
#: ``degenbot_<crate>.<...>``; the dotted crate root is the top ancestor whose
#: level/handlers gate every descendant).
RUST_BRIDGE_LOGGER_NAMES = (
    "degenbot_bot",
    "degenbot_core",
    # The PyO3 binding crate lives in ``crates/shells/degenbot-python/`` but its
    # Cargo ``name`` is ``degenbot_rs`` (set in its ``Cargo.toml``), so every
    # bare ``log::info!`` in that crate emits under ``degenbot_rs::...`` →
    # Python logger ``degenbot._ffi.<...>``. The directory name ``degenbot_python``
    # is never a Rust target — using it here drops the ``[verify]`` and
    # register/snapshot logs.
    "degenbot_rs",
    "degenbot_rpc",
    "degenbot_decoders",
    "degenbot_uniswap",
    # The in-process sim engine + the settlement-arbitrage strategy. The divergence probe
    # (``[sim-divergence]``) + the bridge-probe
    # (``[bridge-probe]``) emit ``log::info!`` from these crates; the base
    # config in ``degenbot.logging`` lowers them so the records are visible.
    "degenbot_simulation",
    "degenbot_arbitrage",
    # The in-process backrun strategy driver (tx 0x3dcfe follow-up): the
    # frame-decision INFO line (``degenbot_strategy.backrun_driver.driver_loop``)
    # and the connector-index boot line dropped here until the crate root was
    # configured.
    "degenbot_strategy",
    # The third-party alloy umbrella: its WARN/ERROR records (pubsub reconnect
    # attempts, transport failures) otherwise escape via stdlib ``lastResort``
    # as raw unformatted stderr lines that pytest cannot capture or attribute.
    "alloy",
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


# ── (T4): arbitrage-subtree visibility ──────────────────────────

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

    The S2 probe found the recurring verifier's ``[verify] (recurring)``
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
            "— recurring-verify lines are silenced"
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
# parked at ``await executor.next_outcome()`` whose Rust future is the one
# parked mid-``log::info!``. The in-loop header-staleness watchdog cannot fire
# because the pump's tokio worker is parked before its select.
#
# The canonical stdlib-blessed decoupling: attach ``QueueHandler`` to every
# configured logger and drain via a dedicated ``QueueListener`` thread that
# owns the only ``StreamHandler`` / stdout write path. The producer side
# (``QueueHandler.emit``) does a fast non-blocking ``put_nowait`` — no stream
# I/O, no GIL held across slow writes; the listener thread is the only thread
# that ever calls ``stream.flush()`` (single writer → no lock contention).


class _WriteRecordingStream(io.StringIO):
    """A stdout probe that records WHICH thread wrote each chunk.

    The single-writer discipline is the whole point of the queue pair: the
    producing thread must never touch the stream, only the listener thread
    may. Recording the writer's identity turns that into a deterministic
    assertion — no timing window between an emit and a read.
    """

    def __init__(self) -> None:
        super().__init__()
        self.writers: list[int] = []

    def write(self, text: str) -> int:
        self.writers.append(threading.get_ident())
        return super().write(text)


def test_configured_loggers_hand_records_off_instead_of_writing_the_stream() -> None:
    """Every configured logger queues its record; the producer never writes stdout.

    The stall class this closes is a record written to a blocked stdout while
    the producer holds the GIL. Two behavioural facts pin it: the record still
    reaches the console (the handoff is lossless), and the console write came
    from another thread than the emitter (the handoff is a handoff — nobody
    writes the stream inline).

    The probe asserts on the CONSOLE writer, whose level the suite raises to
    ``WARNING`` (``tests/conftest.py``), so these records are warnings: an
    ``INFO`` record would stop one gate earlier and prove nothing about the
    handoff.
    """
    import degenbot.logging as dl
    from degenbot.logging import PY_PACKAGE_ROOT_LOGGER_NAMES

    dl.flush_console_log()
    probe = _WriteRecordingStream()
    original = sys.stdout
    sys.stdout = probe
    tokens = []
    try:
        for name in (*RUST_BRIDGE_LOGGER_NAMES, *PY_PACKAGE_ROOT_LOGGER_NAMES, "degenbot.logging"):
            token = f"queued-handoff {name}"
            tokens.append(token)
            logging.getLogger(name).warning(token)
        dl.flush_console_log()
    finally:
        sys.stdout = original
    written = probe.getvalue()
    for token in tokens:
        assert token in written, f"{token} was lost between the queue and the console writer"
    emitter = threading.get_ident()
    assert probe.writers, "the console writer never wrote — the probe saw nothing"
    assert emitter not in probe.writers, (
        "a record was written to stdout on the emitting thread — the pipeline "
        "wrote the stream inline instead of handing it to the listener "
        "(the GIL-held-across-flush stall class)"
    )


def test_a_record_emitted_after_a_drain_still_reaches_the_console() -> None:
    """The drain barrier leaves the pipeline armed: a later record is not lost.

    ``flush_console_log`` stops and restarts the writer thread. A record
    emitted after that cycle still has to land on the console — a listener
    that came back up without its queue, or a queue swapped out from under the
    producers, would silently drop everything after the first flush.
    """

    import degenbot.logging as dl

    probe = io.StringIO()
    original = sys.stdout
    sys.stdout = probe
    try:
        dl.flush_console_log()
        logging.getLogger(RUST_BRIDGE_LOGGER_NAMES[0]).warning("late record after a drain")
        dl.flush_console_log()
    finally:
        sys.stdout = original
    assert "late record after a drain" in probe.getvalue(), (
        "a record emitted after a drain never reached the console — the "
        "listener is queueing into a dead hand-off"
    )


def test_rust_info_record_still_reaches_the_console_via_the_queue() -> None:
    """Regression guard: routing through the queue does NOT drop records.

    The contract ``test_rust_info_record_is_visible`` pins (a record on a deep
    Rust target reaches the crate-root handler) must still hold after the
    QueueHandler/QueueListener decoupling — the listener must forward through
    to the console writer the process actually has. A warning is used so the
    record clears the suite's ``WARNING`` console gate.
    """

    import degenbot.logging as dl

    crate_root = RUST_BRIDGE_LOGGER_NAMES[0]
    probe = io.StringIO()
    original = sys.stdout
    sys.stdout = probe
    try:
        logging.getLogger(f"{crate_root}.bot_core.block_pump").warning(
            "queue-bridged info %s", crate_root
        )
        dl.flush_console_log()
    finally:
        sys.stdout = original
    assert f"queue-bridged info {crate_root}" in probe.getvalue(), (
        f"INFO record on {crate_root} did not reach the console writer via the "
        "queue — records are lost"
    )


def test_the_console_writer_resolves_the_stream_current_at_emit_time() -> None:
    """The console write resolves ``sys.stdout`` per record, never pins it.

    ``degenbot.logging`` is imported while pytest's global capture owns
    ``sys.stdout``, and pytest closes that capture object at session teardown.
    A writer that pinned the import-time stream then sprays
    ``--- Logging error ---`` (``ValueError: I/O operation on closed file``)
    for every record flushed after teardown — swallowed by
    ``Handler.handleError``, so the suite stays green while logging is broken.
    Late binding lands each record in the CURRENT stdout: the active capture
    during the session, whatever stream is installed after it. A warning is
    used so the record clears the suite's ``WARNING`` console gate.
    """

    import degenbot.logging as dl

    probe = io.StringIO()
    original = sys.stdout
    sys.stdout = probe
    try:
        logging.getLogger("degenbot.logging").warning("late-bound emit")
        dl.flush_console_log()
    finally:
        sys.stdout = original
    assert "late-bound emit" in probe.getvalue(), (
        "the record must land in the stream that is current at emit time, "
        "not the one that was current when the writer was configured"
    )


def test_a_closed_console_stream_falls_back_without_a_logging_error() -> None:
    """A closed active stdout (post-teardown race) falls back, never raises.

    ``Handler.handleError`` swallows an emit failure into a
    ``--- Logging error ---`` block on stderr, so a broken console pipeline
    leaves the suite green. The contract is behavioural: emit while
    ``sys.stdout`` is closed and the pipeline reports no handler error (it
    writes to the process original instead), and a record emitted afterwards
    still lands. Warnings clear the suite's ``WARNING`` console gate.
    """

    import degenbot.logging as dl

    closed = io.StringIO()
    closed.close()
    errors = io.StringIO()
    probe = io.StringIO()
    original_out, original_err = sys.stdout, sys.stderr
    sys.stdout = closed
    sys.stderr = errors
    try:
        logging.getLogger("degenbot.logging").warning("closed-stream probe")
        dl.flush_console_log()
        sys.stdout = probe
        logging.getLogger("degenbot.logging").warning("record after the closed stream")
        dl.flush_console_log()
    finally:
        sys.stdout = original_out
        sys.stderr = original_err
    assert "Logging error" not in errors.getvalue(), (
        "the console writer raised on a closed stream — every post-teardown "
        f"record sprays a handler error:\n{errors.getvalue()}"
    )
    assert "record after the closed stream" in probe.getvalue(), (
        "the console pipeline did not recover after a closed stream"
    )
