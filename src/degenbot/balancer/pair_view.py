"""BalancerPairView: N-token pool adapter for two-token arbitrage paths."""
from __future__ import annotations

from typing import TYPE_CHECKING
from weakref import WeakSet

from degenbot.exceptions import DegenbotValueError
from degenbot.types.concrete import (
    AbstractPublisherMessage,
    PoolStateMessage,
    Publisher,
    Subscriber,
)

if TYPE_CHECKING:
    from fractions import Fraction

    from eth_typing import ChecksumAddress

    from degenbot.balancer.pools import BalancerV2Pool
    from degenbot.balancer.stable_pools import BalancerV2StablePool
    from degenbot.balancer.swap_amounts import BalancerV2SwapAmounts
    from degenbot.erc20 import Erc20Token
    from degenbot.types.abstract import AbstractPoolState
    from degenbot.types.hop_types import HopType
    from degenbot.types.pool_protocols import SimulationResult


class BalancerPairView:
    """Adapts an N-token Balancer pool to a 2-token pair view for ArbitragePath.

    Delegates swap calculations and hop state to the underlying pool,
    for a specific (token_a, token_b) pair. Cheap to create (no I/O).

    Implements subscription relay: the view subscribes to the underlying
    pool and re-publishes notifications to its own subscribers with
    publisher=self. This ensures ArbitragePath._pool_index identity
    checks work correctly — the path only sees the view, not the pool.
    """

    def __init__(
        self,
        pool: BalancerV2Pool | BalancerV2StablePool,
        token_a: Erc20Token,
        token_b: Erc20Token,
    ) -> None:
        """Initialize the instance."""
        self._pool = pool
        self._token0 = token_a
        self._token1 = token_b
        self._subscribers: WeakSet[Subscriber] = WeakSet()
        # Subscribe to pool as a relay
        pool.subscribe(self)

    @property
    def address(self) -> str:
        """Return address."""
        return self._pool.address

    @property
    def token0(self) -> Erc20Token:
        """Token0."""
        return self._token0

    @property
    def token1(self) -> Erc20Token:
        """Token1."""
        return self._token1

    @property
    def fee(self) -> Fraction:
        """Fee."""
        return self._pool.fee

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: AbstractPoolState | None = None,
    ) -> int:
        """Calculate tokens out from tokens in.

        Returns:
            The computed integer value.

        Raises:
            DegenbotValueError: See function documentation.

        """
        if token_in == self._token0:
            token_out = self._token1
        elif token_in == self._token1:
            token_out = self._token0
        else:
            msg = f"token_in {token_in} not in pair"
            raise DegenbotValueError(message=msg)
        return self._pool.calculate_tokens_out_from_tokens_in(
            token_in=token_in,
            token_out=token_out,
            token_in_quantity=token_in_quantity,
            override_state=override_state,  # ty: ignore[invalid-argument-type]
        )

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult:
        """Simulate swap.

        Returns:
            The computed value.

        """
        return self._pool.simulate_swap(
            token_in=token_in,
            amount_in=amount_in,
            token_out=token_out,
            state_override=state_override,
        )

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: AbstractPoolState | None = None,
        *,
        token_in: Erc20Token | None = None,
        token_out: Erc20Token | None = None,
    ) -> HopType:
        """Convert to hop state.

        Returns:
            The computed value.

        """
        # Delegate to the pool's to_hop_state with explicit pair selection.
        # When token_in/token_out are provided by the caller, pass them through.
        # Otherwise, derive from the pair's token0/token1.
        if token_in is None and token_out is None:
            token_in = self._token0 if zero_for_one else self._token1
            token_out = self._token1 if zero_for_one else self._token0
        return self._pool.to_hop_state(
            zero_for_one=zero_for_one,
            state_override=state_override,  # ty: ignore[invalid-argument-type]
            token_in=token_in,
            token_out=token_out,
        )

    def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001, ARG002
        """Return the pool fee regardless of direction.

        Returns:
            The computed value.

        """
        return self._pool.fee

    def build_swap_amount(
        self,
        zero_for_one: bool,  # noqa: FBT001
        amount_in: int,
        amount_out: int,
    ) -> BalancerV2SwapAmounts:
        """Build swap amount.

        Returns:
            The computed value.

        """
        if zero_for_one:
            token_in = self._token0
            token_out = self._token1
        else:
            token_in = self._token1
            token_out = self._token0
        return self._pool.build_swap_amount(
            zero_for_one=zero_for_one,
            amount_in=amount_in,
            amount_out=amount_out,
            token_in=token_in,
            token_out=token_out,
        )

    # --- Subscription relay ---

    def subscribe(self, subscriber: Subscriber) -> None:
        """Subscribe to state updates from this view.

        The view relays notifications from the underlying pool.
        Subscribers receive messages with publisher=self (the view),
        not the underlying pool.
        """
        self._subscribers.add(subscriber)

    def unsubscribe(self, subscriber: Subscriber) -> None:
        """Perform unsubscribe."""
        self._subscribers.discard(subscriber)

    def notify(self, publisher: Publisher, message: AbstractPublisherMessage) -> None:  # noqa: ARG002
        """Relay notifications from the underlying pool.

        Re-publishes to this view's subscribers with publisher=self,
        so that ArbitragePath._pool_index identity checks work
        correctly. The view is the publisher from the path's
        perspective.
        """
        if not isinstance(message, PoolStateMessage):
            return
        for subscriber in self._subscribers:
            subscriber.notify(publisher=self, message=message)  # ty: ignore[invalid-argument-type]
