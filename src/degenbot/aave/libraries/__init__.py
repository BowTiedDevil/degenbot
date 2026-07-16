"""Aave Wad/Ray math library for 18/27-decimal fixed-point arithmetic.

The other Aave V3 math libraries (PercentageMath, PoolMath, TokenMath,
GhoMath) lived here in Python solely to feed the now-retired enrichment
pipeline; the ``degenbot-aave`` Rust core crate owns their equivalents
(via ``degenbot-evm-math``) and was the sole consumer after the pipeline
cutover. They were removed; this package now exposes only ``wad_ray_math``
— the last Python Aave math, retained Step D will retire it once
``degenbot.aave.analysis.core`` is gone (Step C deletes it).
"""
