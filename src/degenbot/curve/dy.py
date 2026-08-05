"""Stable companion home for the Curve `get_dy` calculator seam.

Re-exports the Rust-backed calculator from the ``degenbot._ffi.curve_dy``
submodule with un-prefixed names, mirroring how ``degenbot.curve.math``
re-exports ``degenbot._ffi.curve_math``. A companion (``CurveStableswapPool``)
imports these directly; the ``_ffi`` module is an internal detail.

- ``DyCalculationInputs`` — a mutable builder snapshot the companion fills
  (mirrors the pure ``DyCalculationInputs`` core dataclass).
- ``calculate_dy`` / ``calculate_dy_underlying`` — the Rust pure-calc entry
  points (task ``CNEP47``, epic ``TV72EG``).
"""

from degenbot._ffi.curve_dy import DyCalculationInputs, calculate_dy, calculate_dy_underlying

__all__ = ["DyCalculationInputs", "calculate_dy", "calculate_dy_underlying"]
