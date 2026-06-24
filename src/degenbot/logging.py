"""Logging configuration for the degenbot package.

This module owns two concerns:

1. The package logger (``degenbot.logging``) for Python-side records.
2. The **Rust bridge** loggers that ``pyo3-log`` forwards Rust ``log::`` records
   into.

Rust ``log::info!`` / ``log::warn!`` / ``log::error!`` calls (e.g. in
``block_pump.rs``, ``verify.rs``, ``register.rs``) are bridged to Python by
``pyo3_log::init()`` (called from the ``degenbot_rs`` ``#[pymodule]``). The
bridge forwards each Rust record to ``logging.getLogger(<rust target>.replace("::", "."))
`` — so a ``log::info!`` in ``degenbot_bot::bot_core::block_pump`` lands on the
Python logger ``degenbot_bot.bot_core.block_pump``.

Without base config those records are silent: the crate-root loggers
(``degenbot_bot``, ``degenbot_core``, ``degenbot_rs``, ``degenbot_rpc``,
``degenbot_decoders``, ``degenbot_uniswap``) inherit the root logger's default
``WARNING`` level and have no handler, so every Rust ``INFO``/``DEBUG`` record
is dropped at the logger-level gate *before* reaching a handler (and
``WARN``/``ERROR`` only escape via stdlib ``lastResort`` on stderr — bypassing
this module's stdout handler and format). Worse, ``pyo3-log`` caches the
effective level per Rust target on first use, so *later* Python-side
``logging.basicConfig(...)`` calls from a caller never restore visibility.

The fix lives here, in the base config that runs at ``import degenbot`` time —
*before* any Rust code logs (Rust logs fire only once a pump/verify/register
operation runs, never during import). Configuring the crate-root loggers up
front means ``pyo3-log``'s first-use cache stores the lowered level for every
target, so no caller wiring is required and no cache reset is needed.
"""

import functools
import inspect
import logging
import os
import sys
from collections.abc import Callable

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

_STDOUT_HANDLER = logging.StreamHandler(sys.stdout)
logger.addHandler(_STDOUT_HANDLER)

#: The Rust crate-root Python logger names that ``pyo3-log`` forwards ``log::``
#: records into. Each Rust target ``degenbot_<crate>::...`` maps to the Python
#: logger ``degenbot_<crate>.<...>``; the dotted crate root is the top ancestor
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
    # ``degenbot_rs.<...>``. A previous entry of ``degenbot_python`` matched
    # the directory name, which no Rust target ever uses — the ``[verify]``
    # success/failure lines and the register/snapshot logs were dropped.
    "degenbot_rs",
    "degenbot_rpc",
    "degenbot_decoders",
    "degenbot_uniswap",
)

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
    _rust_logger.addHandler(_STDOUT_HANDLER)
    _rust_logger.propagate = False


def set_log_level(level: int) -> None:
    """Set the degenbot + Rust-bridge log level from one knob.

    Mirrors the historical conftest behaviour of bumping the package logger to
    ``DEBUG`` for the test run, extended to cover the Rust ``log::`` bridge so
    Rust ``debug!`` records are not left behind by the crate-root level
    ``pyo3-log`` cached at import (the cache is only consulted on the *first*
    log per target; lowering the level here before that first call is effective
    because no Rust path logs at import time).
    """
    logger.setLevel(level)
    for name in RUST_BRIDGE_LOGGER_NAMES:
        logging.getLogger(name).setLevel(level)


# Check DEGENBOT_DEBUG_FUNCTION_CALLS environment variable for function call logging
_FUNCTION_CALL_LOGGING_ENABLED = os.environ.get("DEGENBOT_DEBUG_FUNCTION_CALLS", "").lower() in {
    "1",
    "true",
    "yes",
}


def log_function_call[**P, R](func: Callable[P, R]) -> Callable[P, R]:
    """Log function calls when DEGENBOT_DEBUG_FUNCTION_CALLS is enabled.

    Returns:
        The computed value.

    """
    if not _FUNCTION_CALL_LOGGING_ENABLED:
        return func

    func_name = getattr(func, "__name__", repr(func))

    if inspect.iscoroutinefunction(func):

        @functools.wraps(func)
        async def async_wrapper(*args: P.args, **kwargs: P.kwargs) -> R:
            sig = inspect.signature(func)
            bound = sig.bind(*args, **kwargs)
            bound.apply_defaults()
            arg_str = ", ".join(f"{k}={v!r}" for k, v in bound.arguments.items())
            logger.debug("%s(%s)", func_name, arg_str)
            result: R = await func(*args, **kwargs)
            logger.debug("%s -> %r", func_name, result)
            return result

        return async_wrapper  # ty:ignore[invalid-return-type]

    @functools.wraps(func)
    def sync_wrapper(*args: P.args, **kwargs: P.kwargs) -> R:
        sig = inspect.signature(func)
        bound = sig.bind(*args, **kwargs)
        bound.apply_defaults()
        arg_str = ", ".join(f"{k}={v!r}" for k, v in bound.arguments.items())
        logger.debug("%s(%s)", func_name, arg_str)
        result: R = func(*args, **kwargs)
        logger.debug("%s -> %r", func_name, result)
        return result

    return sync_wrapper
