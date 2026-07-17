"""Stable companion home for Aerodrome CREATE2 pool-address derivation.

Re-exports the Rust-backed address-compute pyfunctions from the
``degenbot._ffi`` flat root. Importers should use::

    from degenbot.aerodrome.address import compute_aerodrome_v2_pool_address

rather than reaching into ``degenbot._ffi`` directly — this path is stable
across future Rust reshuffles (ADR-013: the ``_ffi`` extension is private;
one barrier per domain, and that barrier is an ``__init__.py``).

This mirrors the ``degenbot.aerodrome.math`` barrier (the Solidly/Aerodrome
math leaves). The functions are thin PyO3 wrappers over the pure-Rust
``degenbot_uniswap::create2::compute_aerodrome_v2_address`` /
``compute_aerodrome_v3_address`` leaves; see that crate's docs for the CREATE2
derivation. The Python ``generate_aerodrome_v{2,3}_pool_address`` shells in
``degenbot.aerodrome.functions`` layer EIP-55 checksum normalization on top
of these raw returns.
"""

from degenbot._ffi import (
    compute_aerodrome_v2_pool_address,
    compute_aerodrome_v3_pool_address,
)

__all__ = [
    "compute_aerodrome_v2_pool_address",
    "compute_aerodrome_v3_pool_address",
]
