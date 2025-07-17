from dataclasses import dataclass

from eth_typing import ChecksumAddress

from degenbot.types.aliases import BlockNumber


@dataclass(slots=True, frozen=True, kw_only=True)
class AbstractPoolState:
    address: ChecksumAddress
    block: BlockNumber | None
