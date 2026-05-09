"""Auto-registration adapter that syncs pool state to the Rust solver cache.

When a pool's state changes, the adapter receives the notification and
updates the solver's Rust pool cache, eliminating the need for manual
cache management.
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING, Any

from degenbot.types.concrete import AbstractPublisherMessage, Subscriber

if TYPE_CHECKING:
    from degenbot.arbitrage.optimizers.solver import ArbSolver
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool


class ArbPoolCacheAdapter(Subscriber):
    """
    Subscribes to pool state updates and auto-registers them in the
    ArbSolver's Rust pool cache.

    Each pool is registered in both reserve orientations (token0→token1
    and token1→token0), since the solver's cache stores direction-specific
    reserve pairs. The adapter returns the primary pool_id; the reverse
    orientation's ID is ``primary_id + 1``.

    Currently supports UniswapV2Pool and AerodromeV2Pool (volatile)
    pools with constant-product invariant. V3/V4 concentrated-liquidity
    pools require virtual reserves and tick range data that is not
    suitable for the simple Rust cache path.
    """

    def __init__(self, *, solver: ArbSolver) -> None:
        self._solver = solver
        self._pool_to_ids: dict[int, tuple[int, int]] = {}  # id(pool) → (forward_id, reverse_id)

    def register(self, pool: AbstractLiquidityPool) -> int:
        """
        Register a pool for auto-updates.

        Subscribes to the pool's state notifications and registers both
        reserve orientations in the solver's cache.

        Returns the primary (forward) pool ID.
        """
        pool.subscribe(self)

        # Extract current state to register immediately
        reserves = self._get_reserves(pool)
        fee = self._get_fee(pool)

        if reserves is None or fee is None:
            msg = f"Cannot extract reserves/fee from pool {pool.address}"
            raise ValueError(msg)

        reserve_in, reserve_out = reserves

        # Register forward orientation
        forward_id = self._solver.register_pool(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee,
        )

        # Register reverse orientation
        reverse_id = self._solver.register_pool(
            reserve_in=reserve_out,
            reserve_out=reserve_in,
            fee=fee,
        )

        self._pool_to_ids[id(pool)] = (forward_id, reverse_id)
        return forward_id

    def notify(self, publisher: Any, message: AbstractPublisherMessage) -> None:
        """
        Called when a pool publishes a state update.

        Updates both reserve orientations in the Rust cache.
        """
        pool = publisher
        ids = self._pool_to_ids.get(id(pool))
        if ids is None:
            return  # Pool not registered with this adapter

        reserves = self._get_reserves(pool)
        fee = self._get_fee(pool)

        if reserves is None or fee is None:
            return

        reserve_in, reserve_out = reserves
        forward_id, reverse_id = ids

        # Update both orientations
        self._solver.update_pool(forward_id, reserve_in, reserve_out, fee)
        self._solver.update_pool(reverse_id, reserve_out, reserve_in, fee)

    def get_pool_ids(self, pool: AbstractLiquidityPool) -> tuple[int, int] | None:
        """Return (forward_id, reverse_id) for a registered pool, or None."""
        return self._pool_to_ids.get(id(pool))

    @staticmethod
    def _get_reserves(pool: Any) -> tuple[int, int] | None:
        """Extract (reserve_token0, reserve_token1) from a pool object."""
        # V2/Aerodrome pools have state.reserves_token0/1
        state = getattr(pool, "state", None)
        if state is not None:
            r0 = getattr(state, "reserves_token0", None)
            r1 = getattr(state, "reserves_token1", None)
            if isinstance(r0, int) and isinstance(r1, int):
                return r0, r1

        # Fallback: reserves tuple
        reserves = getattr(pool, "reserves", None)
        if isinstance(reserves, tuple) and len(reserves) == 2:
            return reserves[0], reserves[1]

        return None

    @staticmethod
    def _get_fee(pool: Any) -> Fraction | None:
        """Extract the pool fee as a Fraction."""
        # V2/Aerodrome volatile pools: fee is a Fraction
        fee = getattr(pool, "fee", None)
        if isinstance(fee, Fraction):
            return fee

        # V2 pools with fee_token0/fee_token1 (directional fees)
        # Use fee_token0 as the default forward direction fee
        fee_token0 = getattr(pool, "fee_token0", None)
        if isinstance(fee_token0, Fraction):
            return fee_token0

        # V3/V4 pools: fee is int, need FEE_DENOMINATOR
        fee_int = getattr(pool, "fee", None)
        fee_denom = getattr(pool, "FEE_DENOMINATOR", None)
        if isinstance(fee_int, int) and isinstance(fee_denom, int):
            return Fraction(fee_int, fee_denom)

        return None
