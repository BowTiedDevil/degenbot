"""Pool simulation protocols.

Define structural interfaces for pool behavior. Pools satisfy these
protocols by implementing the required methods — no inheritance needed.

Three pool-shape protocols replace the former ABCs:
- ConstantProductPool: replaces AbstractUniswapV2Pool + AbstractAerodromeV2Pool
- ConcentratedLiquidityPool: replaces AbstractConcentratedLiquidityPool
- StableswapPool: replaces direct CurveStableswapPool references
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol, runtime_checkable

if TYPE_CHECKING:
    from fractions import Fraction

    from eth_typing import ChecksumAddress

    from degenbot.arbitrage.types import AbstractSwapAmounts
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types.abstract import AbstractPoolState
    from degenbot.types.concrete import Subscriber
    from degenbot.types.hop_types import HopType


# ── Pool-shape protocols (replace ABCs) ─────────────────────────────
#
# These structural protocols identify pool families by the attributes
# they expose, not by inheritance. A pool satisfies ConstantProductPool
# if it has token0, token1, fee_token0, fee_token1, reserves_token0,
# reserves_token1 -- regardless of its class hierarchy.


@runtime_checkable
class ConstantProductPool(Protocol):
    """Any pool using the x*y=k invariant (V2-style, Aerodrome volatile/stable).

    Replaces AbstractUniswapV2Pool and AbstractAerodromeV2Pool for
    isinstance dispatch. Aerodrome pools satisfy this protocol too —
    they have the same structural shape (directional fees, reserves).
    The stable flag is accessed separately when needed.
    """

    @property
    def address(self) -> ChecksumAddress: ...

    @property
    def name(self) -> str: ...

    @property
    def token0(self) -> Erc20Token: ...

    @property
    def token1(self) -> Erc20Token: ...

    @property
    def fee_token0(self) -> Fraction: ...

    @property
    def fee_token1(self) -> Fraction: ...

    @property
    def reserves_token0(self) -> int: ...

    @property
    def reserves_token1(self) -> int: ...


@runtime_checkable
class ConcentratedLiquidityPool(Protocol):
    """Any pool using concentrated liquidity (tick-based: V3, V4).

    Replaces AbstractConcentratedLiquidityPool for isinstance dispatch.
    """

    @property
    def address(self) -> ChecksumAddress: ...

    @property
    def name(self) -> str: ...

    @property
    def token0(self) -> Erc20Token: ...

    @property
    def token1(self) -> Erc20Token: ...

    @property
    def fee(self) -> int: ...

    @property
    def liquidity(self) -> int: ...

    @property
    def sqrt_price_x96(self) -> int: ...

    @property
    def tick(self) -> int: ...

    @property
    def tick_spacing(self) -> int: ...


@runtime_checkable
class StableswapPool(Protocol):
    """Any pool using the Curve StableSwap invariant.

    Structural check for multi-token stable pools. Used sparingly
    since Curve pools are typically dispatched by concrete type
    in the builder registry.
    """

    @property
    def tokens(self) -> tuple[Erc20Token, ...]: ...


@dataclass(slots=True, frozen=True, kw_only=True)
class SimulationResult:
    """
    Pool-agnostic simulation output.

    Returned by PoolSimulation.simulate_swap for all pool types.
    """

    amount_in: int
    amount_out: int
    initial_state: AbstractPoolState
    final_state: AbstractPoolState


@runtime_checkable
class PoolSimulation(Protocol):
    """
    Required interface for all pools.

    Supports exact-input swap simulation and pub/sub for state updates.
    """

    @property
    def address(self) -> ChecksumAddress: ...

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult: ...

    def subscribe(self, subscriber: Subscriber) -> None: ...

    def unsubscribe(self, subscriber: Subscriber) -> None: ...


@runtime_checkable
class ReverseSimulatablePool(Protocol):
    """
    Optional interface for pools that support exact-output simulation.

    Not all pool types can compute input from desired output.
    """

    def simulate_swap_for_output(
        self,
        token_in: ChecksumAddress,
        token_out: ChecksumAddress,
        amount_out: int,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult: ...


@runtime_checkable
class StateManageablePool(Protocol):
    """
    Optional interface for pools with on-chain state management.

    Curve and Balancer pools typically don't implement this.
    """

    def external_update(self, update: object) -> None: ...

    def discard_states_before_block(self, block: int) -> None: ...

    def restore_state_before_block(self, block: int) -> None: ...


@runtime_checkable
class CacheablePool(Protocol):
    """
    A pool whose reserves and fee can be registered in the Rust solver cache.

    Used by ArbPoolCacheAdapter. Replaces getattr-based introspection
    with explicit methods.
    """

    def reserves_for_cache(self) -> tuple[int, int]: ...

    """Return (reserve_token0, reserve_token1) for the Rust cache."""

    def fee_for_cache(self) -> Fraction: ...

    """Return the pool fee as a Fraction (e.g. Fraction(3, 1000))."""

    def subscribe(self, subscriber: Subscriber) -> None: ...

    """Subscribe to pool state updates (provided by PublisherMixin)."""


@runtime_checkable
class ArbitrageCapablePool(PoolSimulation, Protocol):
    """
    Interface for pools participating in arbitrage paths.

    Extends PoolSimulation with hop state conversion and fee extraction,
    absorbing the old PoolAdapter protocol.
    """

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: AbstractPoolState | None = None,
        *,
        token_in: Erc20Token | None = None,
        token_out: Erc20Token | None = None,
    ) -> HopType: ...

    def extract_fee(self, zero_for_one: bool) -> Fraction: ...  # noqa: FBT001


@runtime_checkable
class TwoTokenSwapCalculation(Protocol):
    """
    A 2-token pool that can calculate output from input.

    token_out is implied by token_in since there are exactly two tokens.
    """

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: AbstractPoolState | None = None,
    ) -> int: ...


@runtime_checkable
class MultiTokenSwapCalculation(Protocol):
    """
    An N-token pool requiring explicit token_out for swap calculation.

    Curve and Balancer pools need token_out specified because they have
    more than two tokens.
    """

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        token_in_quantity: int,
        override_state: AbstractPoolState | None = None,
    ) -> int: ...


@runtime_checkable
class ArbitragePathPool(PoolSimulation, Protocol):
    """
    A pool that can participate in a cyclic arbitrage path.

    Extends PoolSimulation with directional token access, swap calculation,
    hop state conversion, and fee extraction. Intentionally excludes
    multi-token pools (Curve, Balancer) whose calculate_tokens_out_from_tokens_in
    signature requires an explicit token_out parameter.
    """

    @property
    def token0(self) -> Erc20Token: ...

    @property
    def token1(self) -> Erc20Token: ...

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: AbstractPoolState | None = None,
    ) -> int: ...

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: AbstractPoolState | None = None,
        *,
        token_in: Erc20Token | None = None,
        token_out: Erc20Token | None = None,
    ) -> HopType: ...

    def extract_fee(self, zero_for_one: bool) -> Fraction: ...  # noqa: FBT001

    def build_swap_amount(
        self,
        zero_for_one: bool,  # noqa: FBT001
        amount_in: int,
        amount_out: int,
    ) -> AbstractSwapAmounts: ...
