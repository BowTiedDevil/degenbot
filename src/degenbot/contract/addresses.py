"""Deterministic contract address derivation."""

from eth_typing import ChecksumAddress
from eth_utils.crypto import keccak
from hexbytes import HexBytes

from degenbot.checksum_cache import get_checksum_address


def create2_address(
    deployer: str | bytes,
    salt: bytes | str,
    init_code_hash: bytes | str,
) -> ChecksumAddress:
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
        keccak(HexBytes(0xFF) + HexBytes(deployer) + HexBytes(salt) + HexBytes(init_code_hash))[
            -20:
        ],  # Contract address is the least significant 20 bytes from the 32 byte hash
    )
