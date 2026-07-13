"""Hashing + selector helpers owned by degenbot.

Replaces ``web3.Web3.keccak`` (which web3 re-exports from
:mod:`eth_utils.crypto`) with degenbot-owned surfaces so source files no
longer import ``Web3`` solely to compute a function selector or a keccak
digest.

Two surfaces:

- :func:`function_selector` — the 4-byte Solidity selector for a function
  signature. Delegates to the Rust core's
  :func:`degenbot_rs.get_function_selector` (alloy ``keccak256`` under the
  hood) so the selector computation is owned by Rust, not by a Python
  keccak re-export.
- :func:`keccak256` — the full 32-byte keccak digest of a byte string.
  Currently wraps :func:`eth_utils.crypto.keccak` (the same implementation
  ``Web3.keccak`` used). This is the one remaining non-Rust hashing surface;
  a follow-up will expose a Rust ``keccak256`` pyfunction and this wrapper
  will delegate to it, dropping the ``eth_utils`` dependency from the
  hashing path.
"""

from __future__ import annotations

from eth_utils.crypto import keccak as _keccak
from hexbytes import HexBytes

from degenbot.degenbot_rs import get_function_selector


def function_selector(signature: str) -> bytes:
    r"""Return the 4-byte Solidity function selector for ``signature``.

    Args:
        signature: A Solidity function signature, e.g.
            ``"transfer(address,uint256)"``.

    Returns:
        The 4-byte selector (e.g. ``b"\\xa9\\x05\\x9c\\xbb"``).

    """
    # get_function_selector returns "0x"-prefixed hex; strip the prefix +
    # decode to the 4 raw bytes.
    return bytes.fromhex(get_function_selector(signature).removeprefix("0x"))


def keccak256(data: bytes) -> HexBytes:
    """Return the 32-byte keccak256 digest of ``data``.

    Args:
        data: The input bytes to hash.

    Returns:
        The 32-byte digest as :class:`~hexbytes.HexBytes` (so callers retain
        the ``to_0x_hex()`` / ``hex()`` surface ``Web3.keccak`` provided).

    """
    return HexBytes(_keccak(data))
