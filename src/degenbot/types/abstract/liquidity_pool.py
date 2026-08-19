"""Abstract liquidity pool protocol definition."""

from __future__ import annotations

from abc import ABC, abstractmethod
from typing import TYPE_CHECKING

from degenbot.types.address_comparable import AddressComparable

if TYPE_CHECKING:
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types.chain import ChecksummedAddress


class AbstractLiquidityPool(AddressComparable, ABC):
    """AbstractLiquidityPool class."""

    address: ChecksummedAddress
    name: str

    @property
    @abstractmethod
    def tokens(self) -> tuple[Erc20Token, ...]:
        """Tokens."""
        ...

    def __str__(self) -> str:
        """Return a string representation.

        Returns:
            A string representation of the object.

        """
        return self.name
