"""
Uniswap V3 library functions.

Provides Rust-accelerated tick math functions. Python implementations
are available directly from ``tick_math`` for testing.
"""

from degenbot.degenbot_rs import get_sqrt_ratio_at_tick, get_tick_at_sqrt_ratio

from .tick_math import (
    MAX_SQRT_RATIO,
    MAX_TICK,
    MIN_SQRT_RATIO,
    MIN_TICK,
)

__all__ = [
    "MAX_SQRT_RATIO",
    "MAX_TICK",
    "MIN_SQRT_RATIO",
    "MIN_TICK",
    "get_sqrt_ratio_at_tick",
    "get_tick_at_sqrt_ratio",
]
