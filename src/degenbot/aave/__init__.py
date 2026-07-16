"""Aave V3 lending market models, operations, and updater.

Bridges two Rust crates:
- ``degenbot-price`` — the Aave price oracle (``PyAavePriceOracle``)
- ``degenbot-aave-updater`` — the Aave V3 updater loop + verification
  (``run_aave_update``, ``activate_aave_market``, etc.)

The updater functions are thin PyO3 wrappers over the pure-Rust
``degenbot-aave-updater`` core crate. ``DatabaseSchemaStale`` and
``CancelHandle`` are re-exported via ``degenbot.db`` and
``degenbot.updater`` respectively.
"""

from degenbot._ffi.aave import (
    activate_aave_market,
    cleanup_zero_balance_positions,
    deactivate_aave_market,
    run_aave_update,
)
from degenbot._ffi.price import PyAavePriceOracle as AavePriceOracle
from degenbot.aave.operations import (
    Operation,
    ScaledTokenEvent,
)

__all__ = [
    "AavePriceOracle",
    "Operation",
    "ScaledTokenEvent",
    "activate_aave_market",
    "cleanup_zero_balance_positions",
    "deactivate_aave_market",
    "run_aave_update",
]
