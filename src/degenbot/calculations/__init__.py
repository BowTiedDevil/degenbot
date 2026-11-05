"""Stable import path for the pure-math formulas the driver reads per block.

``next_base_fee`` (EIP-1559) is re-exported from the Rust core
(``degenbot._ffi.eip_1559``) as the canonical driver entry, so the bot reads the
formula from the single Rust source of truth rather than re-implementing it in
pure Python. The pure-function math modules this package once held are GONE;
the Rust math crates own the pool invariants and quote math, and what remains
here is the one entry the driver still calls.
"""

from degenbot._ffi.eip_1559 import next_base_fee

__all__ = ["next_base_fee"]
