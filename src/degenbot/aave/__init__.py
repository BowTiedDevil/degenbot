"""Aave V3 lending market models, operations, and updater.

Bridges two Rust crates:
- ``degenbot-price`` — the Aave price oracle (``AavePriceOracle``)
- ``degenbot-aave-updater`` — the Aave V3 updater loop + verification
  (``run_aave_update``, ``activate_aave_market``, etc.)

The updater functions are thin PyO3 wrappers over the pure-Rust
``degenbot-aave-updater`` core crate. ``CancelHandle`` is re-exported via
``degenbot.updater``.

The Python enrichment/processing pipeline is GONE: the Rust ``degenbot-aave``
crate owns the full RPC fetch + decode + apply + verify loop end-to-end (git
history carries the deleted modules). The ``analysis/`` read-back package is a
thin driver shell over the same Rust core's position-analysis math, and the
Aave math primitives live in that crate's ``wad_ray_math`` /
``percentage_math`` modules, which own the deployed rounding semantics.
"""

from degenbot._ffi.aave import (
    activate_aave_market,
    cleanup_zero_balance_positions,
    deactivate_aave_market,
    run_aave_update,
)
from degenbot._ffi.price import AavePriceOracle

__all__ = [
    "AavePriceOracle",
    "activate_aave_market",
    "cleanup_zero_balance_positions",
    "deactivate_aave_market",
    "run_aave_update",
]
