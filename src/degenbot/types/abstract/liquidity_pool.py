"""Abstract liquidity pool protocol definition."""
from __future__ import annotations

from abc import ABC, abstractmethod
from typing import TYPE_CHECKING

from degenbot.types.address_comparable import AddressComparable

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types.abstract.pool_state import AbstractPoolState
    from degenbot.types.pool_protocols import SimulationResult


class AbstractLiquidityPool(AddressComparable, ABC):
    """AbstractLiquidityPool class."""

    address: ChecksumAddress
    name: str

    @property
    @abstractmethod
    def tokens(self) -> tuple[Erc20Token, ...]:
        """Tokens."""
        ...

    @abstractmethod
    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult:
        """Simulate swap."""
        ...

    def __str__(self) -> str:
        """Tokens."""
        """Simulate swap."""
        """Return a string representation."""
        return self.name
