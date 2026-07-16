"""Stub for the dynamically-created ``degenbot._ffi.solady`` submodule.

Created at runtime by ``add_solady_module`` in the PyO3 wrapper crate
(``degenbot-python/src/solady/mod.rs``). The functions are thin PyO3
wrappers over the pure-Rust ``degenbot-core::libzip`` (Solady FastLZ)
core.
"""

from hexbytes import HexBytes

def flz_compress(uncompressed_data: str | bytes) -> HexBytes: ...
def flz_decompress(compressed_data: bytes | bytearray | str) -> HexBytes: ...

__all__ = ["flz_compress", "flz_decompress"]
