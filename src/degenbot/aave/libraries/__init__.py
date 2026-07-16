"""Aave Wad/Ray math library for 18/27-decimal fixed-point arithmetic.

The other Aave V3 math libraries (PercentageMath, PoolMath, TokenMath,
GhoMath) lived here in Python solely to feed the now-retired enrichment
pipeline; the ``degenbot-aave-updater`` Rust core crate owns their
equivalents (via ``degenbot-evm-math``) and was the sole consumer after
the pipeline cutover. They were removed; this package now exposes only
``wad_ray_math`` because ``degenbot.aave.analysis.core`` still reads it
on the (Python-side) read-back path.
"""
