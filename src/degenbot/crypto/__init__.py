"""EVM hashing + selector helpers — the stable mirror home for crypto surfaces.

Provides three public functions:

- :func:`function_selector` — the 4-byte Solidity selector for a function
  signature. Delegates to :func:`degenbot.contract.get_function_selector`
  (alloy ``keccak256`` under the hood) so the selector computation is owned
  by Rust, not by a Python keccak re-export.
- :func:`keccak256` — the full 32-byte keccak digest of a byte string.
  Currently wraps :func:`eth_utils.crypto.keccak`. This is the one remaining
  non-Rust hashing surface; a follow-up will expose a Rust ``keccak256``
  pyfunction and this wrapper will delegate to it, dropping the
  ``eth_utils`` dependency from the hashing path.
- :func:`event_topic` — the 32-byte event topic hash for an ABI event entry.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from eth_utils.abi import event_abi_to_log_topic
from eth_utils.crypto import keccak as _keccak
from hexbytes import HexBytes

from degenbot.contract import get_function_selector

if TYPE_CHECKING:
    from eth_typing import ABIEvent

__all__ = (
    "event_topic",
    "function_selector",
    "keccak256",
)


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


def event_topic(event_abi_entry: ABIEvent) -> HexBytes:
    r"""Return the 32-byte event topic hash for an ABI event entry.

    Replaces ``Web3().eth.contract(abi=...).events.<Name>().topic``.
    Computes ``keccak256(canonical_event_signature)`` from the ABI entry
    (the canonical signature omits ``indexed`` markers and uses the
    parameter types in order), so the result is byte-identical to the
    web3-derived topic without importing web3.

    Args:
        event_abi_entry: A single ABI dict with ``"type": "event"``.

    Returns:
        The 32-byte topic as :class:`~hexbytes.HexBytes`.

    """
    return HexBytes(event_abi_to_log_topic(event_abi_entry))
