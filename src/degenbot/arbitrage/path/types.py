"""Types for arbitrage path construction and solver input."""

from dataclasses import dataclass

from degenbot.erc20 import Erc20Token


@dataclass(frozen=True, slots=True)
class SwapVector:
    """SwapVector class."""

    token_in: Erc20Token
    token_out: Erc20Token
    zero_for_one: bool


class PathValidationError(Exception):
    """Raise when an arbitrage path fails validation."""
