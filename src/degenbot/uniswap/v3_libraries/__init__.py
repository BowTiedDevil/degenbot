"""
Uniswap V3 library functions.

This module provides both Python and Rust implementations.
Python is the default for compatibility; use _rs suffix for Rust versions.

Example:
    >>> from degenbot.uniswap.v3_libraries import get_sqrt_ratio_at_tick
    >>> # Uses Python implementation (default)

    >>> from degenbot.uniswap.v3_libraries import get_sqrt_ratio_at_tick_rs
    >>> # Uses Rust implementation (faster)
"""

# Rust implementations (faster, from private module)
from degenbot.degenbot_rs import get_sqrt_ratio_at_tick as get_sqrt_ratio_at_tick_rs
from degenbot.degenbot_rs import get_tick_at_sqrt_ratio as get_tick_at_sqrt_ratio_rs

# Python implementations (kept for backward compatibility during CI/CD transition)
from .tick_math import (
    MAX_SQRT_RATIO,
    MAX_TICK,
    MIN_SQRT_RATIO,
    MIN_TICK,
)
from .tick_math import get_sqrt_ratio_at_tick as get_sqrt_ratio_at_tick_py
from .tick_math import get_tick_at_sqrt_ratio as get_tick_at_sqrt_ratio_py

# Default to Python for backward compatibility
get_sqrt_ratio_at_tick = get_sqrt_ratio_at_tick_py
get_tick_at_sqrt_ratio = get_tick_at_sqrt_ratio_py

__all__ = [
    "MAX_SQRT_RATIO",
    "MAX_TICK",
    "MIN_SQRT_RATIO",
    "MIN_TICK",
    "get_sqrt_ratio_at_tick",
    "get_sqrt_ratio_at_tick_py",
    "get_sqrt_ratio_at_tick_rs",
    "get_tick_at_sqrt_ratio",
    "get_tick_at_sqrt_ratio_py",
    "get_tick_at_sqrt_ratio_rs",
]
