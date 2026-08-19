"""Abstract pool state and cacheable state types."""

from dataclasses import dataclass

from degenbot._ffi import ChecksummedAddress
from degenbot.types.aliases import BlockNumber


@dataclass(slots=True, frozen=True, kw_only=True)
class AbstractPoolState:
    """AbstractPoolState class."""

    address: ChecksummedAddress
    block: BlockNumber | None
