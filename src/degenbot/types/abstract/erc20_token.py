"""Abstract ERC-20 token protocol definition."""

from degenbot._ffi import ChecksummedAddress
from degenbot.types.address_comparable import AddressComparable


class AbstractErc20Token(AddressComparable):
    """AbstractErc20Token class."""

    address: ChecksummedAddress
    symbol: str
    name: str
    decimals: int

    def __str__(self) -> str:
        """Return a string representation.

        Returns:
            The computed value.

        """
        return self.symbol
