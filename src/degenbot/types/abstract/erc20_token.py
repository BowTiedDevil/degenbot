"""Abstract ERC-20 token protocol definition."""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.types.address_comparable import AddressComparable

if TYPE_CHECKING:
    from degenbot.types.chain import ChecksummedAddress


class AbstractErc20Token(AddressComparable):
    """AbstractErc20Token class."""

    address: ChecksummedAddress

    # Token metadata is read-only: concrete tokens (``Erc20Token``) surface it
    # from Rust-owned state, so the abstract contract is property-shaped. A
    # writable annotation here would make every concrete read-only property an
    # invalid override.
    @property
    def symbol(self) -> str:
        """Token symbol."""
        raise NotImplementedError

    @property
    def name(self) -> str:
        """Token name."""
        raise NotImplementedError

    @property
    def decimals(self) -> int:
        """Token decimals."""
        raise NotImplementedError

    def __str__(self) -> str:
        """Return a string representation.

        Returns:
            The computed value.

        """
        return self.symbol
