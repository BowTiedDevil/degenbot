from typing import Sequence

from eth_typing import ChecksumAddress
from ..functions import create2_salt
from eth_utils.address import to_checksum_address
from eth_utils import keccak
from hexbytes import HexBytes


def generate_ramses_pool_address(
    token_addresses: Sequence[ChecksumAddress | str],
    deployer: ChecksumAddress,
    stable: bool,
    init_hash: str | None = None,
) -> ChecksumAddress:
    """
    Generate the deterministic pool address from the token addresses and type.
    """

    RAMSES_VOL_POOL_INIT_HASH = "0x915fb916d8f48e09e22ab9a09f127a3419f9abc6d51730324f003b3c53f578cb"
    RAMSES_STABLE_POOL_INIT_HASH = (
        "0xa77e84da9a14a7270882f31b1042615d939daabc0557e093eee47b8da9cb89de"
    )

    if init_hash is None:
        init_hash = RAMSES_STABLE_POOL_INIT_HASH if stable else RAMSES_VOL_POOL_INIT_HASH

    token_addresses = sorted([address.lower() for address in token_addresses])

    return to_checksum_address(
        keccak(
            HexBytes(0xFF)
            + HexBytes(deployer)
            + create2_salt(
                salt_types=["address", "address", "bool"],
                salt_values=token_addresses + [stable],
            )
            + HexBytes(init_hash)
        )[-20:]
    )
