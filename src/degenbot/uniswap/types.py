"""Uniswap-specific type definitions (fee tiers, pool identities)."""

import dataclasses

from degenbot.erc20 import Erc20Token


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapPoolSwapVector:
    """UniswapPoolSwapVector class."""

    token_in: Erc20Token
    token_out: Erc20Token
    zero_for_one: bool

    def __post_init__(self) -> None:
        """Post-initialization hook."""
        assert self.token_in != self.token_out
