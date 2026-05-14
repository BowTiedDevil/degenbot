"""Concentrated liquidity (Uniswap V3/V4) tick math.

This module is intentionally minimal — the bulk of concentrated liquidity math
already lives in well-organized DEX-specific libraries:

- ``degenbot.uniswap.v3_libraries/`` — tick_math, full_math, sqrt_price_math, swap_math
- ``degenbot.uniswap.v4_libraries/`` — same for V4

Those libraries are standalone pure functions and don't need to move here.
This module exists for any cross-DEX shared utilities that emerge.
"""
