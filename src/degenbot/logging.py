"""Logging configuration for the degenbot package.

This module owns two concerns:

1. The package logger (``degenbot.logging``) for Python-side records.
2. The **Rust bridge** loggers that the Rust core's ``tracing`` subscriber
   forwards records into.

Rust ``tracing`` events — and ``log::`` records, bridged into ``tracing`` by
``tracing_log::LogTracer`` — are forwarded to Python ``logging`` by the
``PythonLogLayer`` installed by ``init_logging_subscriber`` in
``rust/crates/degenbot-python/src/python_log_layer.rs`` during ``degenbot_rs``
module init. The layer derives each record's Python logger name from the Rust
target (``::`` → ``.``): an event for ``degenbot_bot::bot_core::block_pump``
lands on the Python logger ``degenbot_bot.bot_core.block_pump``.

Without base config those records are silent: the crate-root loggers
(``degenbot_bot``, ``degenbot_core``, ``degenbot_rs``, ``degenbot_rpc``,
``degenbot_decoders``, ``degenbot_uniswap``) inherit the root logger's default
``WARNING`` level and have no handler, so every forwarded ``INFO``/``DEBUG``
record is dropped at the Python logger-level gate *before* reaching a handler
(and ``WARN``/``ERROR`` only escape via stdlib ``lastResort`` on stderr,
bypassing this module's stdout handler and format).

The fix lives here, in the base config that runs at ``import degenbot`` time —
*before* any Rust code logs (Rust logs fire only once a pump/verify/register
operation runs, never during import). Configuring the crate-root loggers up
front at the lowered level makes forwarded records visible with no caller
wiring. The Rust-side ``tracing`` ``EnvFilter`` (``RUST_LOG``) is the first
gate; Python ``logging`` is the second.
"""

import atexit
import logging
import logging.handlers
import os
import queue
import sys

"""
Create a global logger instance.
"""

logger = logging.getLogger(__name__)
logger.propagate = False

# Check DEGENBOT_DEBUG environment variable for debug mode
if os.environ.get("DEGENBOT_DEBUG", "").lower() in {"1", "true", "yes"}:
    _LOG_LEVEL = logging.DEBUG
else:
    _LOG_LEVEL = logging.INFO

logger.setLevel(_LOG_LEVEL)

# The real stdout writer — owned SOLELY by the ``QueueListener`` thread below
# (never attached directly to any logger). When stdout is piped (e.g. the
# bot driver ``run_bot.sh`` redirects stdout as
# ``> >(tee -a "$LOG" > /dev/null) 2>&1``), ``sys.stdout`` is block-buffered;
# ``StreamHandler.emit`` then calls ``stream.flush()`` which blocks on a full
# pipe or on the ``BufferedWriter._write_lock`` futex under concurrent writers
# — holding the GIL across the I/O wait. The Rust ``PythonLogLayer`` flushes
# each batch to Python ``logging`` under one ``Python::attach`` and runs the
# full Python ``logging`` pipeline under it, so a slow stdout flush stalls every
# thread waiting on the GIL (the asyncio main loop, the pump's tokio worker).
# The ``QueueHandler``/``QueueListener`` pair below decouples producers from
# the slow writer: producers do a fast non-blocking ``put_nowait`` (no stream
# I/O, no GIL held across slow writes); the listener thread is the only thread
# that ever calls ``stream.flush()`` (single writer → no lock contention,
# slow ``os.write`` blocks only the listener thread whose sole job is
# writing). See ``tests/test_rust_logging_bridge.py`` (the
# ``QueueHandler-not-StreamHandler`` + listener-drains guards).
_STDOUT_HANDLER = logging.StreamHandler(sys.stdout)

# In-process queue + listener. ``SimpleQueue`` is ``thread.Lock``-based (no
# ``Condition``, no notifier thread) — ``put_nowait`` is a few microseconds and never
# blocks. ``respect_handler_level=True`` so the listener still honors each
# destination handler's level (mirrors direct-emit semantics).
_LOG_QUEUE: queue.SimpleQueue = queue.SimpleQueue()
_QUEUED_HANDLER = logging.handlers.QueueHandler(_LOG_QUEUE)
_QUEUED_HANDLER.setLevel(_LOG_LEVEL)
_LOG_LISTENER = logging.handlers.QueueListener(
    _LOG_QUEUE,
    _STDOUT_HANDLER,
    respect_handler_level=True,
)
_LOG_LISTENER.start()
# ``atexit`` rather than ``__del__``: ``QueueListener``'s thread is a daemon
# by default, so without an explicit ``stop()`` the listener could be torn
# down by interpreter shutdown while a queued record is mid-``emit`` → a
# truncated final log line. ``atexit`` drains the queue before shutdown.
atexit.register(_LOG_LISTENER.stop)

logger.addHandler(_QUEUED_HANDLER)

#: The Rust crate-root Python logger names that the forwarding layer maps
#: ``tracing`` events into. Each Rust target ``degenbot_<crate>::...`` maps to
#: the Python logger ``degenbot_<crate>.<...>``; the dotted crate root is the
#: top ancestor
#: whose level and handlers gate every descendant record, so configuring only
#: the root is sufficient — and ordering matters: this must run before the
#: first Rust log call (it does, since ``log::`` only fires at pump/verify/
#: register time, never at import).
RUST_BRIDGE_LOGGER_NAMES = (
    "degenbot_bot",
    "degenbot_core",
    # The PyO3 binding crate lives in ``crates/degenbot-python/`` but its
    # Cargo ``name`` is ``degenbot_rs`` (set in its ``Cargo.toml``), so every
    # bare ``log::info!`` in that crate (``verify.rs``, ``register.rs``,
    # ``json.rs``) emits under ``degenbot_rs::...`` → Python logger
    # ``degenbot._ffi.<...>``. A previous entry of ``degenbot_python`` matched
    # the directory name, which no Rust target ever uses — the ``[verify]``
    # success/failure lines and the register/snapshot logs were dropped.
    "degenbot_rs",
    "degenbot_rpc",
    "degenbot_decoders",
    "degenbot_uniswap",
    # The in-process sim engine + the settlement-arbitrage strategy. The divergence probe
    # (``[sim-divergence]``, ergo task 4C33DP / epic TR6GWT) + the bridge-probe
    # (``[bridge-probe]``) emit events from these crates; without configuring
    # the crate-root here their records are dropped at the Python logger-level
    # gate before reaching a handler (silent even with the env var on).
    "degenbot_simulation",
    "degenbot_arbitrage",
)

#: The Python package-tree root whose descendants include Python-side modules
#: that log via ``logging.getLogger("degenbot.arbitrage.<module>")`` (e.g. the
#: recurring verifier ``degenbot.arbitrage.recurring_verify``). These inherit
#: from the ``degenbot`` package logger — a SIBLING of ``degenbot.logging``
#: (which has ``propagate = False``), not an ancestor — so without configuring
#: the ``degenbot`` root here their INFO records fall through to the stdlib
#: root (WARNING, no handler) and are dropped. S2 (GTOD23-PB24RX) surfaced this:
#: the recurring verifier ran and detected drift but its ``[verify] (recurring)``
#: lines never reached stdout, and 0 ``(recurring)`` hits appeared across all 27
#: permutation runs. Configuring the ``degenbot`` package root with the same
#: stdout handler + INFO level makes the arbitrage subtree visible.
PY_PACKAGE_ROOT_LOGGER_NAMES = ("degenbot",)

# Attach the same stdout handler + level to each Rust crate-root logger so
# Rust records are visible through the package's stdout stream/format.
# ``propagate = False`` mirrors the package-logger convention above: the
# crate-root handler already guarantees visibility, so we never also fan records
# out to the root logger (which would either duplicate against a caller's root
# handler, or else limp through stdlib ``lastResort`` on stderr without the
# degenbot format).
for _name in RUST_BRIDGE_LOGGER_NAMES:
    _rust_logger = logging.getLogger(_name)
    _rust_logger.setLevel(_LOG_LEVEL)
    _rust_logger.addHandler(_QUEUED_HANDLER)
    _rust_logger.propagate = False

# Configure the Python package root so the ``degenbot.arbitrage.*`` subtree
# (and any other ``degenbot.<module>`` logger created via
# ``logging.getLogger("degenbot...")``) is visible at INFO. ``propagate = False``
# stops records at the ``degenbot`` root so they don't also limp through the
# stdlib root ``lastResort`` on stderr. ``degenbot.logging`` itself (a child of
# ``degenbot``) already has ``propagate = False`` + its own handler above, so
# configuring the shared parent does NOT duplicate its records.
for _name in PY_PACKAGE_ROOT_LOGGER_NAMES:
    _pkg_logger = logging.getLogger(_name)
    _pkg_logger.setLevel(_LOG_LEVEL)
    _pkg_logger.addHandler(_QUEUED_HANDLER)
    _pkg_logger.propagate = False


def set_log_level(level: int) -> None:
    """Set the degenbot + Rust-bridge log level from one knob.

    Mirrors the historical conftest behaviour of bumping the package logger to
    ``DEBUG`` for the test run, extended to cover the Rust bridge loggers so
    Rust ``debug!`` records are not left behind by the crate-root level set at
    import. Lowering the level here is effective because no Rust path logs at
    import time.
    """
    logger.setLevel(level)
    for name in RUST_BRIDGE_LOGGER_NAMES:
        logging.getLogger(name).setLevel(level)
    for name in PY_PACKAGE_ROOT_LOGGER_NAMES:
        logging.getLogger(name).setLevel(level)
