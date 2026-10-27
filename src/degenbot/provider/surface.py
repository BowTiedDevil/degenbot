"""The provider surface — the ONE home of the sync/async trees' shared contract logic (ADR-086).

The two provider trees are two call-convention adapters over the Rust pyclass
pair; their agreement is pinned by :mod:`tests.provider.test_sync_async_parity`,
never maintained by hand. The logic that is *not* a one-line delegation lives
here, once, so the trees shrink to delegation: the ``get_logs`` filter
normalization and validation, and the typed payload coercions both trees call.

The block-tag resolution precedent (:mod:`degenbot.provider.block_helpers`)
stays block-only; this module carries the get-logs + payload coercions.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from degenbot.types.aliases import BlockNumber


@dataclass(frozen=True, slots=True)
class LogFilter:
    """Filter criteria for log fetching.

    Args:
        from_block: Starting block number (inclusive)
        to_block: Ending block number (inclusive)
        addresses: Contract addresses to filter (optional)
        topics: Event topic signatures, nested by position (optional)

    Example:
        >>> filter = LogFilter(
        ...     from_block=18_000_000,
        ...     to_block=18_010_000,
        ...     addresses=["0xContractAddress..."],
        ...     topics=[["0xTransfer..."]],  # Match first topic
        ... )

    """

    from_block: BlockNumber
    to_block: BlockNumber
    addresses: list[str] = field(default_factory=list)
    topics: list[list[str]] = field(default_factory=list)

    def __post_init__(self) -> None:
        """Post-initialization hook.

        Raises:
            ValueError: If to_block is less than from_block.

        """
        if self.to_block < self.from_block:
            msg = "to_block must be >= from_block"
            raise ValueError(msg)


def normalize_get_logs(
    filter_param: LogFilter | None,
    *,
    from_block: BlockNumber | None,
    to_block: BlockNumber | None,
    addresses: list[str] | None,
    topics: list[list[str]] | None,
) -> tuple[BlockNumber, BlockNumber, list[str] | None, list[list[str]] | None]:
    """Resolve the two ``get_logs`` input shapes to one validated filter.

    Both trees accept a :class:`LogFilter` *or* individual keyword arguments;
    the range is validated once here so the two call conventions answer the
    same contract. A ``LogFilter`` is already range-validated at construction;
    the keyword path validates on entry.

    Returns:
        ``(from_block, to_block, addresses, topics)`` resolved and validated.

    Raises:
        ValueError: If neither a filter nor a block range is provided, or if
            the keyword range is reversed.

    """
    if filter_param is not None:
        return (
            filter_param.from_block,
            filter_param.to_block,
            filter_param.addresses or None,
            filter_param.topics or None,
        )
    if from_block is None or to_block is None:
        msg = "Either filter_param or from_block/to_block must be provided"
        raise ValueError(msg)
    if from_block > to_block:
        msg = f"from_block ({from_block}) must be <= to_block ({to_block})"
        raise ValueError(msg)
    return (from_block, to_block, addresses, topics)
