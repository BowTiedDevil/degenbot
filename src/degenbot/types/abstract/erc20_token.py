"""Abstract ERC-20 token protocol definition."""
from eth_typing import ChecksumAddress

from degenbot.types.address_comparable import AddressComparable


class AbstractErc20Token(AddressComparable):
    """AbstractErc20Token class."""

    address: ChecksumAddress
    symbol: str
    name: str
    decimals: int

    def __str__(self) -> str:
        """Return a string representation."""
        return self.symbol
