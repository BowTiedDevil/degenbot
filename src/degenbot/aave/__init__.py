"""Aave V3 lending market models, operations, and updater.

Bridges two Rust crates:
- ``degenbot-price`` — the Aave price oracle (``PyAavePriceOracle``)
- ``degenbot-aave-updater`` — the Aave V3 updater loop + verification
  (``run_aave_update``, ``activate_aave_market``, etc.)

The updater functions are thin PyO3 wrappers over the pure-Rust
``degenbot-aave-updater`` core crate. ``DatabaseSchemaStale`` and
``CancelHandle`` are re-exported via ``degenbot.db`` and
``degenbot.updater`` respectively.

The former Python in-memory enrichment/processing pipeline
(``extraction``/``operations``/``events``/``models``/``calculator``/
``liquidation_patterns``/``pattern_types``/``operation_types`` + the
``enrichment/`` and ``processors/`` packages) was retired once the Rust
``degenbot-aave`` crate reached feature parity (it owns the full RPC fetch +
decode + apply + verify loop end-to-end). The ``analysis/`` read-back
package is a thin driver shell over the same Rust core's position-analysis
math; the ``libraries/`` math primitives remain Python (pending Step D).
"""

from degenbot._ffi.aave import (
    activate_aave_market,
    cleanup_zero_balance_positions,
    deactivate_aave_market,
    run_aave_update,
)
from degenbot._ffi.price import PyAavePriceOracle as AavePriceOracle

__all__ = [
    "AavePriceOracle",
    "activate_aave_market",
    "cleanup_zero_balance_positions",
    "deactivate_aave_market",
    "run_aave_update",
]
