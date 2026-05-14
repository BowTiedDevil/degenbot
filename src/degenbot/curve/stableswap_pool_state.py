"""State mixin for Curve StableSwap pools.

Holds immutable data attributes and read-only properties that access them.
No calculation logic — calculations stay in CurveStableswapPool due to
deep integration with fetcher callbacks and block-scoped caches.

Curve StableSwap state is significantly different from V2/V3/V4:
- Multiple tokens (not just 2)
- A coefficient (amplification parameter) with ramping
- Strategy enums for variant computation
- Many fetcher callbacks for on-chain data
- Block-scoped caches for rate/virtual price data
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token


class StableswapPoolState:
    """State for Curve StableSwap pools.

    Immutable data set at construction:
    - _tokens: the pool's ERC-20 tokens (2 or more)
    - Various configuration attributes (a_coefficient, fee, admin_fee, etc.)
    """

    # Immutable — set once at construction
    _tokens: tuple[Erc20Token, ...]

    @property
    def tokens(self) -> tuple[Erc20Token, ...]:
        return self._tokens
