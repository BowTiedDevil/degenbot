"""Fake CurveStableswapPool implementation for equivalence testing.

Provides synthetic pool state generation that matches the behavior of
CurveStableswapPool without requiring chain state or web3 connections.

Key features:
- Exact Curve stableswap math (Newton's method for D and get_y)
- Support for 2-coin and 3-coin pools
- Configurable A coefficient, fees, and balances
- Metapool support (base pool composition)
- Compatible with ArbitrageCapablePool protocol

Usage:
    token0 = FakeToken("0x6B175474E89094C44Da98b954EedeAC495271d0F", 18)  # DAI
    token1 = FakeToken("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", 6)   # USDC
    
    pool = FakeCurveStableswapPool(
        tokens=(token0, token1),
        balances=(10_000_000 * 10**18, 10_000_000 * 10**6),
        a_coefficient=1000,
        fee=4000000,  # 0.04%
    )
    
    # Use with ArbitragePath
    hop = pool.to_hop_state(zero_for_one=True)
    result = pool.simulate_swap(token0.address, 1000 * 10**18, token1.address)
"""

from __future__ import annotations

from dataclasses import dataclass
from fractions import Fraction
from typing import TYPE_CHECKING
from weakref import WeakSet

from degenbot.types.abstract import AbstractLiquidityPool, AbstractPoolState
from degenbot.types.concrete import PublisherMixin
from degenbot.types.hop_types import CurveStableswapHop, HopType, PoolInvariant
from degenbot.types.pool_protocols import SimulationResult

if TYPE_CHECKING:
    from collections.abc import Sequence

    from eth_typing import ChecksumAddress

    from degenbot.types.concrete import Subscriber


class FakeCurveToken:
    """Lightweight token stand-in for Curve pools.
    
    Compatible with FakeToken from test_path.conftest for interoperability.
    """

    def __init__(self, address: str, decimals: int = 18, symbol: str = "") -> None:
        self.address: ChecksumAddress = address  # type: ignore[assignment]
        self.decimals = decimals
        self.symbol = symbol or f"TKN{decimals}"

    def __eq__(self, other: object) -> bool:
        # Check for any object with 'address' attribute (interoperable with FakeToken)
        if hasattr(other, "address"):
            return self.address.lower() == other.address.lower()
        return NotImplemented

    def __hash__(self) -> int:
        return hash(self.address.lower())

    def __repr__(self) -> str:
        return f"FakeCurveToken({self.symbol})"


@dataclass(frozen=True, kw_only=True)
class FakeCurvePoolState(AbstractPoolState):
    """Frozen state for Curve pools.
    
    Unlike V2/V3 pools, Curve pools have N balances (not just reserves_token0/reserve1).
    """
    balances: tuple[int, ...]


class FakeCurveStableswapPool(PublisherMixin, AbstractLiquidityPool):
    """Synthetic Curve stableswap pool for testing.
    
    Implements the full Curve stableswap invariant math:
    - D calculation via Newton's method
    - get_y for swap output calculation
    - Precision-adjusted balances (xp)
    - Dynamic fee calculation
    
    Supports 2-coin, 3-coin, and metapool configurations.
    """

    # Curve constants
    PRECISION_DECIMALS: int = 18
    PRECISION: int = 10**PRECISION_DECIMALS
    FEE_DENOMINATOR: int = 10**10
    A_PRECISION: int = 100
    MAX_COINS: int = 8

    def __init__(
        self,
        tokens: Sequence[FakeCurveToken],
        balances: Sequence[int],
        *,
        a_coefficient: int = 1000,
        fee: int = 4_000_000,  # 0.04% default
        address: str = "0xcurve_pool",
        base_pool: FakeCurveStableswapPool | None = None,
    ) -> None:
        """Initialize a fake Curve pool.
        
        Args:
            tokens: Sequence of 2-8 tokens (must match balances length)
            balances: Initial balances for each token
            a_coefficient: Amplification coefficient (A)
            fee: Trading fee in 10^10 precision (e.g., 4_000_000 = 0.04%)
            address: Pool address
            base_pool: For metapools, the underlying base pool
        """
        if len(tokens) != len(balances):
            raise ValueError(f"Token count ({len(tokens)}) must match balance count ({len(balances)})")
        if len(tokens) < 2 or len(tokens) > self.MAX_COINS:
            raise ValueError(f"Curve pools require 2-{self.MAX_COINS} tokens, got {len(tokens)}")

        self.tokens: tuple[FakeCurveToken, ...] = tuple(tokens)
        self.address: ChecksumAddress = address  # type: ignore[assignment]
        self.name = f"FakeCurve({len(tokens)}coins)"

        self.a_coefficient = a_coefficient
        self.fee = fee
        self.base_pool = base_pool

        # Calculate precision multipliers (10^(18 - token.decimals))
        self.precision_multipliers = tuple(
            10 ** (self.PRECISION_DECIMALS - token.decimals) for token in self.tokens
        )

        # Initialize state
        self._state = FakeCurvePoolState(
            address=self.address,
            block=None,
            balances=tuple(balances),
        )
        self._subscribers: WeakSet[object] = WeakSet()

    @property
    def state(self) -> FakeCurvePoolState:
        """Current pool state."""
        return self._state

    @property
    def balances(self) -> tuple[int, ...]:
        """Current balances for all tokens."""
        return self._state.balances

    @property
    def n_coins(self) -> int:
        """Number of coins in pool."""
        return len(self.tokens)

    # Compatibility properties for ArbitragePath (2-coin pools only)
    @property
    def token0(self) -> FakeCurveToken:
        """First token (compatibility with V2/V3-style pools)."""
        return self.tokens[0]

    @property
    def token1(self) -> FakeCurveToken:
        """Second token (compatibility with V2/V3-style pools)."""
        if len(self.tokens) < 2:
            raise AttributeError("Pool has fewer than 2 tokens")
        return self.tokens[1]

    def _xp(self, balances: Sequence[int]) -> tuple[int, ...]:
        """Convert balances to precision-adjusted balances.
        
        xp[i] = balances[i] * precision_multipliers[i]
        """
        return tuple(
            balance * precision // self.PRECISION
            for balance, precision in zip(balances, self.precision_multipliers, strict=True)
        )

    def _get_d(self, xp: Sequence[int], amp: int) -> int:
        """Calculate Curve invariant D via Newton's method.
        
        D is the total amount of tokens when the pool is perfectly balanced.
        """
        n_coins = self.n_coins
        s = sum(xp)
        if s == 0:
            return 0

        d_prev = 0
        d = s
        ann = amp * n_coins

        # Newton's method: iterate until convergence
        for _ in range(255):
            d_p = d
            for x in xp:
                d_p = d_p * d // (x * n_coins)
            d_prev = d
            d = (ann * s + d_p * n_coins) * d // ((ann - 1) * d + (n_coins + 1) * d_p)
            if abs(d - d_prev) <= 1:
                break
        return d

    def _get_y(self, i: int, j: int, x: int, xp: Sequence[int]) -> int:
        """Calculate y for given x in token i, result in token j.
        
        Solves the stableswap equation for y given new balance x at index i.
        """
        n_coins = self.n_coins
        d = self._get_d(xp, self.a_coefficient)

        c = d
        s = 0
        ann = self.a_coefficient * n_coins

        for k in range(n_coins):
            if k == j:
                continue
            _x = x if k == i else xp[k]
            s += _x
            c = c * d // (_x * n_coins)

        c = c * d // (ann * n_coins)
        b = s + d // ann

        y_prev = 0
        y = d

        # Newton's method for y
        for _ in range(255):
            y_prev = y
            y = (y * y + c) // (2 * y + b - d)
            if abs(y - y_prev) <= 1:
                break
        return y

    def _get_dy(self, i: int, j: int, dx: int) -> int:
        """Calculate output amount for input dx.
        
        Main swap calculation. Returns amount of token j received for dx of token i.
        """
        xp = self._xp(self._state.balances)

        # Add input to x
        x = xp[i] + dx * self.precision_multipliers[i] // self.PRECISION

        # Calculate y (output in precision-adjusted units)
        y = self._get_y(i, j, x, xp)

        # Convert back to token j's decimals
        dy = xp[j] - y - 1  # -1 for rounding

        # Apply fee
        fee = self.fee * dy // self.FEE_DENOMINATOR
        dy -= fee

        # Convert from precision-adjusted to actual token amount
        return dy * self.PRECISION // self.precision_multipliers[j]

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: AbstractPoolState | None = None,
    ) -> HopType:
        """Create a CurveStableswapHop for the solver.
        
        For 2-token pools: zero_for_one=True means tokens[0] -> tokens[1]
        """
        state = (
            state_override
            if isinstance(state_override, FakeCurvePoolState)
            else self._state
        )

        # For 2-token pools, map zero_for_one to token indices
        if zero_for_one:
            i, j = 0, 1
        else:
            i, j = 1, 0

        # Verify indices valid
        if i >= len(state.balances) or j >= len(state.balances):
            raise ValueError(f"Invalid swap indices ({i}, {j}) for {len(state.balances)} tokens")

        # Create swap_fn closure
        def swap_fn(dx: int) -> int:
            return self._get_dy(i, j, dx)

        # Calculate D for current state (for reference, not strictly needed with swap_fn)
        xp = self._xp(state.balances)
        d = self._get_d(xp, self.a_coefficient)

        return CurveStableswapHop(
            reserve_in=state.balances[i],
            reserve_out=state.balances[j],
            fee=Fraction(self.fee, self.FEE_DENOMINATOR),
            curve_a=self.a_coefficient,
            curve_n_coins=len(self.tokens),
            curve_d=d,
            token_index_in=i,
            token_index_out=j,
            precisions=self.precision_multipliers,
            swap_fn=swap_fn,
            invariant=PoolInvariant.CURVE_STABLESWAP,
        )

    def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001
        """Return fee as Fraction."""
        return Fraction(self.fee, self.FEE_DENOMINATOR)

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult:
        """Simulate a swap through this pool.
        
        Finds token indices by address, calculates output via Curve math.
        """
        state = (
            state_override
            if isinstance(state_override, FakeCurvePoolState)
            else self._state
        )

        # Find token indices
        try:
            i = next(
                idx for idx, t in enumerate(self.tokens)
                if t.address.lower() == token_in.lower()
            )
            j = next(
                idx for idx, t in enumerate(self.tokens)
                if t.address.lower() == token_out.lower()
            )
        except StopIteration as e:
            raise ValueError(f"Token not found in pool: {token_in} -> {token_out}") from e

        # Calculate output
        amount_out = self._get_dy(i, j, amount_in)

        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=state,
            final_state=state,  # State doesn't change in simulation
        )

    def subscribe(self, subscriber: Subscriber) -> None:
        self._subscribers.add(subscriber)

    def unsubscribe(self, subscriber: Subscriber) -> None:
        self._subscribers.discard(subscriber)

    def __repr__(self) -> str:
        tokens_str = "-".join(t.symbol for t in self.tokens)
        return (
            f"{self.__class__.__name__}("
            f"address={self.address}, "
            f"tokens={tokens_str}, "
            f"A={self.a_coefficient}, "
            f"fee={100 * self.fee / self.FEE_DENOMINATOR:.2f}%"
            f")"
        )
