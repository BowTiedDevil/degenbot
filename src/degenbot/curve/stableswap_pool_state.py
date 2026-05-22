"""State mixin for Curve StableSwap pools.

Holds immutable data attributes and read-only properties that access them.
No calculation logic — calculations stay in CurveStableswapPool.

The state mixin follows the same pattern as V2PoolState and V3PoolState:
underscore-prefixed private attributes set by the concrete pool's __init__,
with public @property accessors defined here.

Curve StableSwap state is significantly different from V2/V3/V4:
- Multiple tokens (not just 2)
- A coefficient (amplification parameter) with ramping
- Strategy enums for variant computation
- CurveDataProvider for on-chain data access
- Block-scoped caches for rate/virtual price data
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
    from degenbot.curve.strategies import PoolStrategies
    from degenbot.erc20 import Erc20Token


class StableswapPoolState:
    """State for Curve StableSwap pools.

    Immutable data set at construction:
    - _tokens: the pool's ERC-20 tokens (2 or more)
    - Configuration attributes (a_coefficient, fee, admin_fee, etc.)
    - Strategy enums and variant computation selectors

    All attributes are set by the concrete pool's __init__.
    Properties provide read access; mutation happens through external_update().
    """

    # ── Immutable: set once at construction ──

    _tokens: tuple[Erc20Token, ...]
    _a_coefficient: int
    _fee: int
    _admin_fee: int
    _rate_multipliers: tuple[int, ...]
    _precision_multipliers: tuple[int, ...]
    _base_pool: CurveStableswapPool | None
    _tokens_underlying: tuple[Erc20Token, ...] | None
    _lp_token: Erc20Token
    _use_lending: tuple[bool, ...]
    _initial_a_coefficient: int | None
    _future_a_coefficient: int | None
    _initial_a_coefficient_time: int | None
    _future_a_coefficient_time: int | None
    _create_timestamp: int | None
    _fee_gamma: int
    _mid_fee: int
    _out_fee: int
    _gamma: int
    _offpeg_fee_multiplier: int
    _strategies: PoolStrategies
    _coin_index_type: str
    _name: str

    # ── Immutable properties ──

    @property
    def tokens(self) -> tuple[Erc20Token, ...]:
        """Tokens."""
        return self._tokens

    @property
    def a_coefficient(self) -> int:
        """Return a coefficient."""
        return self._a_coefficient

    @property
    def fee(self) -> int:
        """Return fee."""
        return self._fee

    @property
    def admin_fee(self) -> int:
        """Return admin fee."""
        return self._admin_fee

    @property
    def rate_multipliers(self) -> tuple[int, ...]:
        """Rate multipliers."""
        return self._rate_multipliers

    @property
    def precision_multipliers(self) -> tuple[int, ...]:
        """Precision multipliers."""
        return self._precision_multipliers

    @property
    def base_pool(self) -> CurveStableswapPool | None:
        """Base pool."""
        return self._base_pool

    @property
    def tokens_underlying(self) -> tuple[Erc20Token, ...] | None:
        """Tokens underlying."""
        return self._tokens_underlying

    @property
    def lp_token(self) -> Erc20Token:
        """Lp token."""
        return self._lp_token

    @property
    def use_lending(self) -> tuple[bool, ...]:
        """Use lending."""
        return self._use_lending

    @property
    def initial_a_coefficient(self) -> int | None:
        """Return initial a coefficient."""
        return self._initial_a_coefficient

    @property
    def future_a_coefficient(self) -> int | None:
        """Return future a coefficient."""
        return self._future_a_coefficient

    @property
    def initial_a_coefficient_time(self) -> int | None:
        """Return initial a coefficient time."""
        return self._initial_a_coefficient_time

    @property
    def future_a_coefficient_time(self) -> int | None:
        """Return future a coefficient time."""
        return self._future_a_coefficient_time

    @property
    def fee_gamma(self) -> int:
        """Return fee gamma."""
        return self._fee_gamma

    @property
    def mid_fee(self) -> int:
        """Return mid fee."""
        return self._mid_fee

    @property
    def out_fee(self) -> int:
        """Return out fee."""
        return self._out_fee

    @property
    def gamma(self) -> int:
        """Return gamma."""
        return self._gamma

    @property
    def offpeg_fee_multiplier(self) -> int:
        """Return offpeg fee multiplier."""
        return self._offpeg_fee_multiplier

    @property
    def name(self) -> str:
        """Return name."""
        return self._name

    # ── Mutable properties (state accessed through the mixin) ──

    # Note: _state, base_cache_updated, and base_virtual_price are set on the
    # pool class __init__ (they interact with caches/locks). The properties
    # below provide read access through the mixin for locality.
