"""Block identifier resolution helpers."""

from typing import TYPE_CHECKING

from web3.types import BlockIdentifier

from degenbot.exceptions import DegenbotValueError
from degenbot.provider import ProviderAdapter
from degenbot.provider.interface import AsyncProviderAdapter
from degenbot.types.aliases import BlockNumber


def get_number_for_block_identifier(
    identifier: BlockIdentifier | None,
    provider: ProviderAdapter,
) -> BlockNumber:
    """
    Convert a block identifier to a block number.

    Args:
        identifier: Block identifier (None, int, or string tag like 'latest')
        provider: ProviderAdapter instance

    Returns:
        Block number as integer
    """

    match identifier:
        case None:
            return provider.get_block_number()
        case int() as block_number_as_int:
            return block_number_as_int
        case "latest" | "earliest" | "pending" | "safe" | "finalized" as block_tag:
            block = provider.get_block(block_tag)
            if block is None:
                raise DegenbotValueError(message=f"Block {block_tag} not found")
            block_number = block.get("number")
            if TYPE_CHECKING:
                assert block_number is not None
            return block_number
        case str() as block_number_as_str:
            try:
                return int(block_number_as_str, 16)
            except ValueError:
                raise DegenbotValueError(
                    message=f"Invalid block identifier {identifier!r}"
                ) from None
        case bytes() as block_number_as_bytes:
            return int.from_bytes(block_number_as_bytes, byteorder="big")
        case _:
            raise DegenbotValueError(message=f"Invalid block identifier {identifier!r}")


async def get_number_for_block_identifier_async(
    identifier: BlockIdentifier | None,
    provider: AsyncProviderAdapter,
) -> BlockNumber:
    match identifier:
        case None:
            return await provider.get_block_number()
        case int() as block_number_as_int:
            return block_number_as_int
        case "latest" | "earliest" | "pending" | "safe" | "finalized" as block_tag:
            block = await provider.get_block(block_tag)
            block_number = block.get("number")
            if TYPE_CHECKING:
                assert block_number is not None
            return block_number
        case str() as block_number_as_str:
            try:
                return int(block_number_as_str, 16)
            except ValueError:
                raise DegenbotValueError(
                    message=f"Invalid block identifier {identifier!r}"
                ) from None
        case bytes() as block_number_as_bytes:
            return int.from_bytes(block_number_as_bytes, byteorder="big")
        case _:
            raise DegenbotValueError(message=f"Invalid block identifier {identifier!r}")
