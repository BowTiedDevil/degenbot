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
    logger.setLevel(logging.DEBUG)
else:
    logger.setLevel(logging.INFO)

logger.addHandler(logging.StreamHandler(sys.stdout))

# Check DEGENBOT_DEBUG_FUNCTION_CALLS environment variable for function call logging
_FUNCTION_CALL_LOGGING_ENABLED = os.environ.get("DEGENBOT_DEBUG_FUNCTION_CALLS", "").lower() in {
    "1",
    "true",
    "yes",
}


def log_function_call[**P, R](func: Callable[P, R]) -> Callable[P, R]:
    """
    Log function calls when DEGENBOT_DEBUG_FUNCTION_CALLS is enabled.
    """

    if not _FUNCTION_CALL_LOGGING_ENABLED:
        return func

    if inspect.iscoroutinefunction(func):

        @functools.wraps(func)
        async def async_wrapper(*args: P.args, **kwargs: P.kwargs) -> R:
            sig = inspect.signature(func)
            bound = sig.bind(*args, **kwargs)
            bound.apply_defaults()
            arg_str = ", ".join(f"{k}={v!r}" for k, v in bound.arguments.items())
            logger.debug("%s(%s)", func.__name__, arg_str)
            result: R = await func(*args, **kwargs)
            logger.debug("%s -> %r", func.__name__, result)
            return result

        return async_wrapper  # type: ignore[return-value]

    @functools.wraps(func)
    def sync_wrapper(*args: P.args, **kwargs: P.kwargs) -> R:
        sig = inspect.signature(func)
        bound = sig.bind(*args, **kwargs)
        bound.apply_defaults()
        arg_str = ", ".join(f"{k}={v!r}" for k, v in bound.arguments.items())
        logger.debug("%s(%s)", func.__name__, arg_str)
        result: R = func(*args, **kwargs)
        logger.debug("%s -> %r", func.__name__, result)
        return result

    return sync_wrapper
