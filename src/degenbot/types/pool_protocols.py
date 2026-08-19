"""Pool simulation protocols.

Define structural interfaces for pool behavior. Pools satisfy these protocols by implementing the
required methods — no inheritance needed.

Three pool-shape protocols:
- ConstantProductPool — V2-family pools
- ConcentratedLiquidityPool — V3/V4-family pools
- StableswapPool — Curve-family pools
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol, runtime_checkable

if TYPE_CHECKING:
    from fractions import Fraction

    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types.abstract import AbstractPoolState
    from degenbot.types.chain import ChecksummedAddress


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
    def address(self) -> ChecksummedAddress:
        """Pool contract address."""
        ...

    @property
    def name(self) -> str:
        """Human-readable pool name."""
        ...

    @property
    def token0(self) -> Erc20Token:
        """The lower-addressed token in the pair."""
        ...

    @property
    def token1(self) -> Erc20Token:
        """The higher-addressed token in the pair."""
        ...

    @property
    def fee_token0(self) -> Fraction:
        """Fee ratio for token0 swaps."""
        ...

    @property
    def fee_token1(self) -> Fraction:
        """Fee ratio for token1 swaps."""
        ...

    @property
    def reserves_token0(self) -> int:
        """Raw reserve amount for token0."""
        ...

    @property
    def reserves_token1(self) -> int:
        """Raw reserve amount for token1."""
        ...


@runtime_checkable
class ConcentratedLiquidityPool(Protocol):
    """Any pool using concentrated liquidity (tick-based: V3, V4).

    Replaces AbstractConcentratedLiquidityPool for isinstance dispatch.
    """

    @property
    def address(self) -> ChecksummedAddress:
        """Pool contract address."""
        ...

    @property
    def name(self) -> str:
        """Human-readable pool name."""
        ...

    @property
    def token0(self) -> Erc20Token:
        """The lower-addressed token in the pair."""
        ...

    @property
    def token1(self) -> Erc20Token:
        """The higher-addressed token in the pair."""
        ...

    @property
    def fee(self) -> int:
        """Pool fee in hundredths of a bip."""
        ...

    @property
    def liquidity(self) -> int:
        """Active liquidity in the current tick range."""
        ...

    @property
    def sqrt_price_x96(self) -> int:
        """Current sqrt price as a Q64.96 value."""
        ...

    @property
    def tick(self) -> int:
        """Current tick."""
        ...

    @property
    def tick_spacing(self) -> int:
        """Minimum spacing between initialized ticks."""
        ...


@runtime_checkable
class StableswapPool(Protocol):
    """Any pool using the Curve StableSwap invariant.

    Structural check for multi-token stable pools. Used sparingly
    since Curve pools are typically dispatched by concrete type
    in the builder registry.
    """

    @property
    def tokens(self) -> tuple[Erc20Token, ...]:
        """Tuple of all pool tokens."""
        ...


@runtime_checkable
class PoolSimulation(Protocol):
    """Required interface for all pools.

    Supports exact-input swap simulation.
    """

    @property
    def address(self) -> ChecksummedAddress:
        """Pool contract address."""
        ...


@runtime_checkable
class StateManageablePool(Protocol):
    """Optional interface for pools with on-chain state management.

    Curve and Balancer pools typically don't implement this.
    """

    def external_update(self, update: object) -> None:
        """Apply an external state update to the pool."""
        ...

    def discard_states_before_block(self, block: int) -> None:
        """Remove cached states before the given block."""
        ...

    def restore_state_before_block(self, block: int) -> None:
        """Restore the most recent state before the given block."""
        ...


@runtime_checkable
class TwoTokenSwapCalculation(Protocol):
    """A 2-token pool that can calculate output from input.

    token_out is implied by token_in since there are exactly two tokens.
    """

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: AbstractPoolState | None = None,
    ) -> int:
        """Calculate output token amount for a given input."""
        ...


@runtime_checkable
class MultiTokenSwapCalculation(Protocol):
    """An N-token pool requiring explicit token_out for swap calculation.

    Curve and Balancer pools need token_out specified because they have
    more than two tokens.
    """

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_out: Erc20Token,
        token_in_quantity: int,
        override_state: AbstractPoolState | None = None,
    ) -> int:
        """Calculate output token amount for a given input with explicit token_out."""
        ...
