"""Deterministic contract address derivation."""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot._ffi import keccak256
from degenbot.checksum_cache import get_checksum_address
from degenbot.utils.bytes import to_bytes

if TYPE_CHECKING:
    from degenbot._ffi import ChecksummedAddress


def create2_address(
    deployer: str | bytes,
    salt: bytes | str,
    init_code_hash: bytes | str,
) -> ChecksummedAddress:
    """Generate the deterministic CREATE2 address.

    Given a deployer, salt, and the keccak hash of the contract creation
    (init) bytecode.

    References:
        - https://eips.ethereum.org/EIPS/eip-1014
        - https://docs.openzeppelin.com/cli/2.8/deploying-with-create2

    Returns:
        The computed value.

    """
    return get_checksum_address(
        keccak256(b"\xff" + to_bytes(deployer) + to_bytes(salt) + to_bytes(init_code_hash))[
            -20:
        ],  # Contract address is the least significant 20 bytes from the 32 byte hash
    )
