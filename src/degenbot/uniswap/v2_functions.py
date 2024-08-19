import itertools
from typing import TYPE_CHECKING, Iterable, List, Sequence

from eth_typing import ChecksumAddress
from eth_utils import keccak
from eth_utils.address import to_checksum_address
from hexbytes import HexBytes

from ..functions import create2_salt

if TYPE_CHECKING:
    from .managers import UniswapV2LiquidityPoolManager
    from .v2_liquidity_pool import LiquidityPool


def generate_v2_pool_address(
    token_addresses: Sequence[ChecksumAddress | str],
    factory_address: ChecksumAddress | str,
    init_hash: str,
) -> ChecksumAddress:
    """
    Generate the deterministic pool address from the token addresses.

    Adapted from https://github.com/Uniswap/universal-router/blob/deployed-commit/contracts/modules/uniswap/v2/UniswapV2Library.sol
    """

    token_addresses = sorted([address.lower() for address in token_addresses])

    return to_checksum_address(
        keccak(
            HexBytes(0xFF)
            + HexBytes(factory_address)
            + create2_salt(
                salt_types=["address", "address"],
                salt_values=token_addresses,
            )
            + HexBytes(init_hash)
        )[-20:]
    )


def get_v2_pools_from_token_path(
    tx_path: Iterable[ChecksumAddress | str],
    pool_manager: "UniswapV2LiquidityPoolManager",
) -> List["LiquidityPool"]:
    return [
        pool_manager.get_pool(
            token_addresses=token_addresses,
            silent=True,
        )
        for token_addresses in itertools.pairwise(tx_path)
    ]
