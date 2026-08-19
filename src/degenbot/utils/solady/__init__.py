"""Solady LibZip: flz compress/decompress for bytecode.

Thin delegation shim over the Rust ``degenbot-core`` core, exposed via the
``degenbot._ffi.solady`` extension module (Rust cdylib crate
``degenbot_rs``). The pure-Python implementation has been retired
(per ADR-005 sub-step C — ``docs/migration-guides/three-layer-transition.md``
§3.3); the Rust core (``rust/crates/degenbot-core/src/libzip.rs``) is the
single implementation, ``#[cfg(test)]`` corpus the regression set.
"""

from degenbot._ffi.solady import flz_compress as _flz_compress_rust
from degenbot._ffi.solady import flz_decompress as _flz_decompress_rust
from degenbot.types.chain import HexStr

__all__ = ["flz_compress", "flz_decompress"]


def flz_compress(uncompressed_data: str | bytes) -> bytes:
    """Compress data using Solady's FastLZ algorithm.

    ref: https://github.com/Vectorized/solady/blob/main/js/solady.js

    Accepts a hex string (with optional ``0x`` prefix) or ``bytes``.

    Returns:
        The compressed data.

    """
    return _flz_compress_rust(uncompressed_data)


def flz_decompress(compressed_data: bytes | bytearray | HexStr) -> bytes:
    """Decompress data using Solady's FastLZ algorithm.

    ref: https://github.com/Vectorized/solady/blob/main/js/solady.js

    Accepts a hex string (with optional ``0x`` prefix), ``bytes``,
    ``bytearray``, or a hex string.

    Returns:
        The decompressed data.

    """
    return _flz_decompress_rust(compressed_data)
