# type: ignore[misc]
"""
Solver integration for arbitrage regression tests.

Provides utilities to run actual arbitrage solvers against fixtures.
"""

import time
from dataclasses import dataclass
from typing import Any

from eth_typing import ChecksumAddress

from degenbot.erc20.erc20 import Erc20Token
from degenbot.exceptions.arbitrage import ArbitrageError
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.uniswap.v2_types import UniswapV2PoolState
from degenbot.uniswap.v3_types import UniswapV3PoolState
from degenbot.uniswap.v4_types import UniswapV4PoolState
from tests.arbitrage.generator.fixtures import ArbitrageCycleFixture


@dataclass(frozen=True, slots=True)
class SolverResult:
    """Result of running a solver against a fixture."""

    fixture_id: str
    optimal_input: int
    profit: int
    calculation_time_ms: float
    success: bool
    error_message: str | None = None


class FakeErc20Token:
    """Minimal ERC20 token for testing."""

    def __init__(
        self,
        address: ChecksumAddress,
        symbol: str = "TKN",
        decimals: int = 18,
    ) -> None:
        self.address = address
        self.symbol = symbol
        self.decimals = decimals
        self.chain_id = 1  # Mainnet

    def __eq__(self, other: object) -> bool:
        if isinstance(other, FakeErc20Token):
            return self.address == other.address
        if isinstance(other, Erc20Token):
            return self.address == other.address
        return False

    def __hash__(self) -> int:
        return hash(self.address)

    def __str__(self) -> str:
        return f"{self.symbol} ({self.address})"

    def __repr__(self) -> str:
        return f"FakeErc20Token(address={self.address}, symbol={self.symbol})"


class FakeV2Pool(AbstractLiquidityPool):
    """
    Minimal V2 pool for testing.

    Does not connect to real contracts. Uses state overrides for calculations.
    """

    def __init__(
        self,
        address: ChecksumAddress,
        token0: FakeErc20Token | Erc20Token,
        token1: FakeErc20Token | Erc20Token,
        fee: float = 0.003,
        initial_state: UniswapV2PoolState | None = None,
    ) -> None:
        self.address = address
        self.token0 = token0
        self.token1 = token1
        self.fee = fee
        self.name = f"FakeV2Pool-{address[:8]}"
        self.chain_id = 1

        # Use provided state or create default
        if initial_state is not None:
            self._state = initial_state
        else:
            self._state = UniswapV2PoolState(
                address=address,
                block=0,
                reserves_token0=10**18,
                reserves_token1=10**18,
            )

    @property
    def state(self) -> UniswapV2PoolState:
        return self._state

    def external_update(self, update: Any) -> None:
        """Update pool state."""
        if hasattr(update, "reserves_token0"):
            self._state = UniswapV2PoolState(
                address=self.address,
                block=getattr(update, "block_number", 0),
                reserves_token0=update.reserves_token0,
                reserves_token1=update.reserves_token1,
            )

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: FakeErc20Token | Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """
        Calculate output tokens using x*y=k formula.

        Parameters
        ----------
        token_in : FakeErc20Token | Erc20Token
            The input token.
        token_in_quantity : int
            Amount of input tokens.
        override_state : UniswapV2PoolState | None
            Optional state to use instead of current state.

        Returns
        -------
        int
            Amount of output tokens.
        """
        state = override_state if override_state is not None else self._state

        # Determine which reserve is in/out
        if token_in == self.token0:
            reserve_in = state.reserves_token0
            reserve_out = state.reserves_token1
        else:
            reserve_in = state.reserves_token1
            reserve_out = state.reserves_token0

        # x*y=k with fee
        fee_multiplier = 1 - self.fee
        amount_in_with_fee = int(token_in_quantity * fee_multiplier)
        numerator = amount_in_with_fee * reserve_out
        denominator = reserve_in + amount_in_with_fee

        return numerator // denominator if denominator > 0 else 0

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out: FakeErc20Token | Erc20Token,
        token_out_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """Calculate input tokens needed for desired output."""
        state = override_state if override_state is not None else self._state

        if token_out == self.token1:
            reserve_in = state.reserves_token0
            reserve_out = state.reserves_token1
        else:
            reserve_in = state.reserves_token1
            reserve_out = state.reserves_token0

        fee_multiplier = 1 - self.fee
        numerator = reserve_in * token_out_quantity
        denominator = int((reserve_out - token_out_quantity) * fee_multiplier)

        return numerator // denominator + 1 if denominator > 0 else 0


class FakeV3Pool(AbstractLiquidityPool):
    """
    Minimal V3 pool for testing.

    Does not connect to real contracts. Uses state overrides for calculations.
    """

    def __init__(
        self,
        address: ChecksumAddress,
        token0: FakeErc20Token | Erc20Token,
        token1: FakeErc20Token | Erc20Token,
        tick_spacing: int = 60,
        initial_state: UniswapV3PoolState | None = None,
    ) -> None:
        self.address = address
        self.token0 = token0
        self.token1 = token1
        self.tick_spacing = tick_spacing
        self.name = f"FakeV3Pool-{address[:8]}"
        self.chain_id = 1

        if initial_state is not None:
            self._state = initial_state
        else:
            self._state = UniswapV3PoolState(
                address=address,
                block=0,
                liquidity=10**18,
                sqrt_price_x96=2**96,  # Price = 1
                tick=0,
                tick_bitmap={},
                tick_data={},
            )

    @property
    def state(self) -> UniswapV3PoolState:
        return self._state

    def external_update(self, update: Any) -> None:
        """Update pool state."""
        # V3 updates are more complex, handled via state overrides

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: FakeErc20Token | Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV3PoolState | None = None,
    ) -> int:
        """
        Simplified V3 swap calculation.

        Note: This is a simplified implementation. Real V3 swaps require
        tick crossing logic. For accurate testing, use actual UniswapV3Pool.
        """
        state = override_state if override_state is not None else self._state

        # Simplified: use liquidity and sqrt price for approximation
        # This is NOT production-accurate, just for testing structure
        sqrt_price = state.sqrt_price_x96 / (2**96)
        price = sqrt_price * sqrt_price

        if token_in == self.token0:
            # Selling token0 for token1
            amount_out = int(token_in_quantity * price * 0.997)  # Approximate with fee
        else:
            # Selling token1 for token0
            amount_out = int(token_in_quantity / price * 0.997)

        return max(0, amount_out)


def build_pools_from_fixture(
    fixture: ArbitrageCycleFixture,
) -> tuple[list[FakeV2Pool | FakeV3Pool], FakeErc20Token]:
    """
    Build minimal pool objects from a fixture.

    Parameters
    ----------
    fixture : ArbitrageCycleFixture
        The fixture containing pool states.

    Returns
    -------
    tuple[list[FakeV2Pool | FakeV3Pool], FakeErc20Token]
        List of pool objects and the input token.
    """
    pools: list[FakeV2Pool | FakeV3Pool] = []
    input_token: FakeErc20Token | None = None

    for address, state in fixture.pool_states.items():
        # Create fake tokens - use placeholder addresses
        token0 = FakeErc20Token(
            ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),  # USDC
            "USDC",
            6,
        )
        token1 = FakeErc20Token(
            ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),  # WETH
            "WETH",
            18,
        )

        if input_token is None:
            input_token = token0 if fixture.input_token_address == token0.address else token1

        if isinstance(state, UniswapV2PoolState):
            pools.append(FakeV2Pool(address, token0, token1, initial_state=state))
        elif isinstance(state, UniswapV3PoolState):
            pools.append(FakeV3Pool(address, token0, token1, initial_state=state))
        elif isinstance(state, UniswapV4PoolState):
            # V4 is similar to V3 for our purposes
            pools.append(FakeV3Pool(address, token0, token1, initial_state=None))
        else:
            msg = f"Unsupported pool state type: {type(state)}"
            raise TypeError(msg)

    if input_token is None:
        input_token = FakeErc20Token(fixture.input_token_address, "INPUT", 18)

    return pools, input_token


def run_solver_on_fixture(
    fixture: ArbitrageCycleFixture,
    max_input: int = 10**20,
) -> SolverResult:
    """
    Run a simplified solver calculation against a fixture.

    This is a baseline implementation that uses the fixture's pool states
    to estimate arbitrage profit. For production accuracy, use actual
    UniswapV2Pool/UniswapV3Pool instances.

    Parameters
    ----------
    fixture : ArbitrageCycleFixture
        The fixture to run against.
    max_input : int
        Maximum input amount to consider.

    Returns
    -------
    SolverResult
        The solver result with profit estimate.
    """
    start_time = time.perf_counter()

    try:
        pools, input_token = build_pools_from_fixture(fixture)

        if len(pools) < 2:
            return SolverResult(
                fixture_id=fixture.id,
                optimal_input=0,
                profit=0,
                calculation_time_ms=0,
                success=False,
                error_message="Need at least 2 pools for arbitrage",
            )

        # Simplified two-pool arbitrage estimation
        # For accurate results, use actual UniswapLpCycle
        pool_a, pool_b = pools[0], pools[1]

        # Binary search for optimal input
        best_profit = 0
        best_input = 0

        for input_amount in [
            10**15,
            10**16,
            10**17,
            10**18,
            10**19,
            10**20,
            10**21,
        ]:
            if input_amount > max_input:
                break

            # Simulate: buy in pool A, sell in pool B
            try:
                # Get state overrides from fixture
                state_a = fixture.pool_states.get(pool_a.address)
                state_b = fixture.pool_states.get(pool_b.address)

                # Calculate output from pool A
                out_a = pool_a.calculate_tokens_out_from_tokens_in(
                    token_in=input_token,
                    token_in_quantity=input_amount,
                    override_state=state_a if isinstance(state_a, UniswapV2PoolState) else None,
                )

                # Determine output token
                output_token = pool_b.token1 if input_token == pool_b.token0 else pool_b.token0

                # Calculate final output from pool B
                out_b = pool_b.calculate_tokens_out_from_tokens_in(
                    token_in=output_token,
                    token_in_quantity=out_a,
                    override_state=state_b if isinstance(state_b, UniswapV2PoolState) else None,
                )

                profit = out_b - input_amount
                if profit > best_profit:
                    best_profit = profit
                    best_input = input_amount

            except (ArbitrageError, ValueError, ZeroDivisionError):
                # Calculation errors (e.g., zero reserves, invalid amounts)
                continue

        elapsed_ms = (time.perf_counter() - start_time) * 1000

        return SolverResult(
            fixture_id=fixture.id,
            optimal_input=best_input,
            profit=best_profit,
            calculation_time_ms=elapsed_ms,
            success=True,
        )

    except (ArbitrageError, ValueError) as e:
        elapsed_ms = (time.perf_counter() - start_time) * 1000
        return SolverResult(
            fixture_id=fixture.id,
            optimal_input=0,
            profit=0,
            calculation_time_ms=elapsed_ms,
            success=False,
            error_message=str(e),
        )


def estimate_profit_for_v2_pair(
    pool_a_state: UniswapV2PoolState,
    pool_b_state: UniswapV2PoolState,
    input_amount: int,
) -> int:
    """
    Estimate profit for a V2-V2 arbitrage pair.

    Parameters
    ----------
    pool_a_state : UniswapV2PoolState
        State of first pool.
    pool_b_state : UniswapV2PoolState
        State of second pool.
    input_amount : int
        Amount to input.

    Returns
    -------
    int
        Estimated profit (may be negative if unprofitable).
    """
    # Standard V2 fee
    fee = 0.003

    # Pool A: token0 -> token1
    # Apply fee
    amount_in_with_fee = int(input_amount * (1 - fee))
    reserve_a_in = pool_a_state.reserves_token0
    reserve_a_out = pool_a_state.reserves_token1

    if reserve_a_in == 0:
        return 0

    # x*y=k: amount_out = (amount_in_with_fee * reserve_out) / (reserve_in + amount_in_with_fee)
    amount_out_a = (amount_in_with_fee * reserve_a_out) // (reserve_a_in + amount_in_with_fee)

    # Pool B: token1 -> token0 (reverse direction)
    amount_in_b_with_fee = int(amount_out_a * (1 - fee))
    reserve_b_in = pool_b_state.reserves_token1
    reserve_b_out = pool_b_state.reserves_token0

    if reserve_b_in == 0:
        return 0

    amount_out_b = (amount_in_b_with_fee * reserve_b_out) // (reserve_b_in + amount_in_b_with_fee)

    return amount_out_b - input_amount


def find_optimal_input_binary_search(
    pool_a_state: UniswapV2PoolState,
    pool_b_state: UniswapV2PoolState,
    max_input: int = 10**21,
    tolerance: int = 10**15,
) -> tuple[int, int]:
    """
    Find optimal input amount using binary search.

    Parameters
    ----------
    pool_a_state : UniswapV2PoolState
        State of first pool.
    pool_b_state : UniswapV2PoolState
        State of second pool.
    max_input : int
        Maximum input to consider.
    tolerance : int
        Stop when search range is below this.

    Returns
    -------
    tuple[int, int]
        (optimal_input, profit_at_optimum)
    """
    # First, find a range where profit is positive
    # Start with small inputs and expand
    test_inputs = [
        10**6, 10**7, 10**8, 10**9, 10**10, 10**11, 10**12, 10**13, 10**14, 10**15
    ]

    best_input = 0
    best_profit = 0

    # Find input with positive profit
    positive_range: list[tuple[int, int]] = []
    for inp in test_inputs:
        profit = estimate_profit_for_v2_pair(pool_a_state, pool_b_state, inp)
        if profit > best_profit:
            best_profit = profit
            best_input = inp
        if profit > 0:
            positive_range.append((inp, profit))

    if not positive_range:
        # No profitable arbitrage found
        return best_input, best_profit

    # Binary search in the positive range
    low_input = positive_range[0][0]
    high_input = positive_range[-1][0] if positive_range[-1][1] > 0 else max_input

    # Refine with binary search
    while high_input - low_input > tolerance:
        mid = (low_input + high_input) // 2

        profit_mid = estimate_profit_for_v2_pair(pool_a_state, pool_b_state, mid)
        profit_higher = estimate_profit_for_v2_pair(
            pool_a_state, pool_b_state, min(mid + tolerance, high_input)
        )

        if profit_higher > profit_mid:
            low_input = mid
            if profit_higher > best_profit:
                best_profit = profit_higher
                best_input = mid + tolerance
        else:
            high_input = mid
            if profit_mid > best_profit:
                best_profit = profit_mid
                best_input = mid

    return best_input, best_profit
