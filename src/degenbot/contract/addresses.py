"""Deterministic contract address derivation."""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot._ffi import create2_address as _rs_create2_address
from degenbot.utils.bytes import to_0x_hex

if TYPE_CHECKING:
    from degenbot._ffi import ChecksummedAddress


def create2_address(
    deployer: str | bytes,
    salt: bytes | str,
    init_code_hash: bytes | str,
) -> ChecksummedAddress:
    """Generate the deterministic CREATE2 address.

    Given a deployer, salt, and the keccak hash of the contract creation
    (init) bytecode. Delegating shell over
    ``degenbot._ffi.create2_address`` (TD1 — the pure-Python keccak/CREATE2
    chain is retired; the Rust ``degenbot_uniswap::create2`` module owns it).

    References:
        - https://eips.ethereum.org/EIPS/eip-1014
        - https://docs.openzeppelin.com/cli/2.8/deploying-with-create2

    Returns:
        The computed value.

    """
    return _rs_create2_address(
        to_0x_hex(deployer),
        to_0x_hex(salt),
        to_0x_hex(init_code_hash),
    )
