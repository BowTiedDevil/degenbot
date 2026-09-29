"""Block identifier resolution helpers.

Shared shaping logic for the sync (``degenbot.provider.sync``) and async
(``degenbot.provider.async_provider``) provider trees: both ``get_block``
twins resolve identifiers here, and both ``get_block_timestamp`` twins
extract the timestamp here, so the block-tag contract has one home.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, cast

from degenbot.exceptions import DegenbotValueError
from degenbot.types.rpc_types import BlockIdentifier, BlockTag

if TYPE_CHECKING:
    from degenbot.provider import AlloyProvider
    from degenbot.types.aliases import BlockNumber
    from degenbot.types.rpc_types import BlockData


def get_number_for_block_identifier(
    identifier: BlockIdentifier | None,
    provider: AlloyProvider,
) -> BlockNumber:
    """Convert a block identifier to a block number.

    Args:
        identifier: Block identifier (None, int, or string tag like 'latest')
        provider: AlloyProvider instance

    Returns:
        Block number as integer

    Raises:
        DegenbotValueError: If the block identifier is invalid or block not found.

    """
    match identifier:
        case None:
            return provider.get_block_number()
        case int() as block_number_as_int:
            return block_number_as_int
        case "latest" | "earliest" | "pending" | "safe" | "finalized" as block_tag:
            block = provider.get_block(cast("int | str", identifier))
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
                    message=f"Invalid block identifier {identifier!r}",
                ) from None
        case bytes() as block_number_as_bytes:
            return int.from_bytes(block_number_as_bytes, byteorder="big")
        case _:
            raise DegenbotValueError(message=f"Invalid block identifier {identifier!r}")


def resolve_block_tag(
    block_identifier: int | str,
    current_block_number: BlockNumber,
) -> BlockNumber:
    """Resolve a provider ``get_block`` identifier to a block number.

    Integer identifiers pass through unchanged; string tags resolve through
    the :class:`BlockTag.parse` ladder against the provider's current block
    number (``'latest'`` -> head, ``'earliest'`` -> 0, ``'pending'`` -> head
    + 1). This is the mixin-level seam; :func:`get_number_for_block_identifier`
    above serves the broader public ``BlockIdentifier`` surface with its own
    error contract.

    Args:
        block_identifier: Block number, or one of 'latest', 'earliest', 'pending'.
        current_block_number: The provider's current block number.

    Returns:
        The concrete block number to query.

    """
    if isinstance(block_identifier, str):
        return BlockTag.parse(block_identifier).to_block_number(current_block_number)
    return block_identifier


def block_timestamp_from(block_data: BlockData | None, requested_block: int | None) -> int:
    """Extract a timestamp from fetched block data, failing on a missing block.

    Args:
        block_data: The block data returned by the provider, or None when the
            block was not found.
        requested_block: The block number the caller asked for (used verbatim
            in the error message; None means the ``'latest'`` default).

    Returns:
        The block timestamp as an integer (Unix seconds).

    Raises:
        ValueError: If the block data is absent.

    """
    if block_data is None:
        msg = f"Block {requested_block} not found"
        raise ValueError(msg)
    return block_data["timestamp"]
