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


def eip_1167_clone_address(
    deployer: ChecksumAddress | str | bytes,
    implementation_contract: ChecksumAddress | str | bytes,
    salt: bytes,
) -> ChecksumAddress:
    """Calculate the address for an EIP-1167 minimal proxy.

    Uses `deployer`, `salt`, delegating calls to the contract
    at `implementation` address.

    References:
        - https://github.com/ethereum/ercs/blob/master/ERCS/erc-1167.md
        - https://github.com/OpenZeppelin/openzeppelin-contracts/blob/master/contracts/proxy/Clones.sol
        - https://www.rareskills.io/post/eip-1167-minimal-proxy-standard-with-initialization-clone-pattern

    Returns:
        The computed value.

    """
    minimal_proxy_code = (
        HexBytes("0x3d602d80600a3d3981f3")
        + HexBytes("0x363d3d373d3d3d363d73")
        + HexBytes(implementation_contract)
        + HexBytes("0x5af43d82803e903d91602b57fd5bf3")
    )

    return create2_address(
        deployer=deployer,
        salt=salt,
        init_code_hash=keccak(minimal_proxy_code),
    )
