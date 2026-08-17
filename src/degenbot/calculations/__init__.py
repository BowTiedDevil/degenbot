"""Standalone pure-math calculation functions for liquidity pools.

All functions in this module are pure: no ``self``, no class references, no I/O.
They accept numeric inputs and return numeric results.

Convention: functions are grouped by invariant family into sub-modules.
Each sub-module is importable independently.

``next_base_fee`` (EIP-1559) is re-exported from the Rust core
(``degenbot._ffi.eip_1559``) as the canonical driver entry, so the bot reads the
formula from the single Rust source of truth rather than re-implementing it in
pure Python (the parity util ``degenbot.calculations.evm_math.next_base_fee`` is
retained as a library/test oracle).
"""

from degenbot._ffi.eip_1559 import next_base_fee

__all__ = ["next_base_fee"]
