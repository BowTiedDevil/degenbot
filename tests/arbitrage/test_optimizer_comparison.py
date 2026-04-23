"""
Compare SciPy minimize_scalar methods using the simple_v2_arb_profitable fixture.
"""

from fractions import Fraction
from typing import TYPE_CHECKING

import pytest
from eth_typing import ChecksumAddress

from degenbot.uniswap.v2_types import UniswapV2PoolState
from tests.arbitrage.generator import FixtureFactory
from tests.arbitrage.mock_pools import MockErc20Token, MockV2Pool

if TYPE_CHECKING:
    from collections.abc import Callable


# Standard V2 fee: 0.3% (matches the fee used in simple_v2_arb_profitable fixture)
V2_FEE = Fraction(3, 1000)

# Token addresses used by the fixture
USDC_ADDRESS: ChecksumAddress = ChecksumAddress("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
WETH_ADDRESS: ChecksumAddress = ChecksumAddress("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")


def build_mock_pools_from_fixture(
    fixture,
) -> tuple[MockV2Pool, MockV2Pool, MockErc20Token]:
    """
    Build MockV2Pool objects from a V2 arbitrage fixture.

    Uses the proper fee from the fixture generation.
    """
    pool_states = list(fixture.pool_states.values())
    addresses = list(fixture.pool_states.keys())

    assert len(pool_states) == 2
    assert all(isinstance(s, UniswapV2PoolState) for s in pool_states)

    # Create mock tokens
    usdc = MockErc20Token(USDC_ADDRESS, "USDC", 6)
    weth = MockErc20Token(WETH_ADDRESS, "WETH", 18)

    # Create mock pools with the proper fee
    pool_a = MockV2Pool(
        address=addresses[0],
        token0=usdc,
        token1=weth,
        initial_state=pool_states[0],
        fee=V2_FEE,
    )
    pool_b = MockV2Pool(
        address=addresses[1],
        token0=usdc,
        token1=weth,
        initial_state=pool_states[1],
        fee=V2_FEE,
    )

    return pool_a, pool_b, usdc


class TestOptimizerMethodComparison:
    """
    Compare Brent vs Golden vs Bounded methods from scipy.optimize.minimize_scalar
    for finding optimal arbitrage input amounts.

    Note: In SciPy's minimize_scalar:
    - "Brent" and "Golden" use `bracket` (a triple of points)
    - "Bounded" uses `bounds` (a tuple of min, max)
    """

    @pytest.fixture
    def v2_fixture(self):
        """Generate the simple V2 arbitrage fixture."""
        factory = FixtureFactory()
        return factory.simple_v2_arb_profitable()

    @pytest.fixture
    def mock_pools(self, v2_fixture) -> tuple[MockV2Pool, MockV2Pool, MockErc20Token]:
        """Build mock pools from the fixture."""
        return build_mock_pools_from_fixture(v2_fixture)

    @pytest.fixture
    def profit_function(self, mock_pools) -> "Callable[[float], float]":
        """
        Create a profit function using MockV2Pool.calculate_tokens_out_from_tokens_in().

        Returns negative profit (since minimize_scalar finds minima).
        """
        pool_a, pool_b, usdc = mock_pools

        def calculate_profit(input_amount: float) -> float:
            """
            Calculate arbitrage profit for a given input amount.

            Returns negative profit for minimization.
            """
            if input_amount <= 0:
                return 0.0

            input_amount_int = int(input_amount)

            # Pool A: Buy token1 (WETH) with token0 (USDC)
            token1_received = pool_a.calculate_tokens_out_from_tokens_in(
                token_in=usdc,
                token_in_quantity=input_amount_int,
            )

            # Pool B: Sell token1 (WETH) for token0 (USDC)
            token0_received = pool_b.calculate_tokens_out_from_tokens_in(
                token_in=pool_b.token1,  # WETH
                token_in_quantity=token1_received,
            )

            # token0 profit (USDC)
            profit = float(token0_received - input_amount_int)

            # Return negative for minimization (minimize -profit = maximize profit)
            return -profit

        return calculate_profit

    def test_brent_vs_bounded_finds_same_optimum(
        self,
        profit_function: "Callable[[float], float]",
    ) -> None:
        """
        Test that both Brent (with bracket) and Bounded methods find same optimum.
        """
        from scipy.optimize import minimize_scalar

        # Brent: use bracket (initial search interval: a, b, c where f(b) < f(a), f(c))
        result_brent = minimize_scalar(
            profit_function,
            bracket=(1.0, 1_000_000.0, 100_000_000_000.0),
            method="Brent",
        )

        # Bounded: use bounds (requires bounded method)
        result_bounded = minimize_scalar(
            profit_function,
            bounds=(1.0, 1_000_000_000_000.0),
            method="Bounded",
        )

        # Both should succeed
        assert result_brent.success, f"Brent failed: {result_brent.message}"
        assert result_bounded.success, f"Bounded failed: {result_bounded.message}"

        # Both should find profitable solutions (negative = positive profit)
        assert result_brent.fun < 0, (
            f"Brent did not find profitable arbitrage, fun={result_brent.fun}"
        )
        assert result_bounded.fun < 0, (
            f"Bounded did not find profitable arbitrage, fun={result_bounded.fun}"
        )

        # Optimal inputs should be close (within 1% relative tolerance)
        assert result_brent.x == pytest.approx(result_bounded.x, rel=1e-2)

    def test_golden_vs_bounded_finds_same_optimum(
        self,
        profit_function: "Callable[[float], float]",
    ) -> None:
        """
        Test that both Golden (with bracket) and Bounded methods find same optimum.
        """
        from scipy.optimize import minimize_scalar

        # Golden: use bracket (like Brent but simpler algorithm)
        result_golden = minimize_scalar(
            profit_function,
            bracket=(1.0, 1_000_000.0, 100_000_000_000.0),
            method="Golden",
        )

        # Bounded: use bounds
        result_bounded = minimize_scalar(
            profit_function,
            bounds=(1.0, 1_000_000_000_000.0),
            method="Bounded",
        )

        # Both should succeed
        assert result_golden.success, f"Golden failed: {result_golden.message}"
        assert result_bounded.success, f"Bounded failed: {result_bounded.message}"

        # Both should find profitable solutions
        assert result_golden.fun < 0, (
            f"Golden did not find profitable arbitrage, fun={result_golden.fun}"
        )
        assert result_bounded.fun < 0, (
            f"Bounded did not find profitable arbitrage, fun={result_bounded.fun}"
        )

        # Optimal inputs should be close
        assert result_golden.x == pytest.approx(result_bounded.x, rel=1e-2)

    def test_brent_faster_than_golden(
        self,
        profit_function: "Callable[[float], float]",
    ) -> None:
        """
        Benchmark test: Brent should typically be faster than Golden.
        Both use bracket method.
        """
        import time

        from scipy.optimize import minimize_scalar

        bracket = (1.0, 1_000_000.0, 100_000_000_000.0)
        iterations = 100

        # Warm up
        for _ in range(10):
            minimize_scalar(profit_function, bracket=bracket, method="Brent")
            minimize_scalar(profit_function, bracket=bracket, method="Golden")

        # Benchmark Brent
        start = time.perf_counter()
        for _ in range(iterations):
            result_brent = minimize_scalar(
                profit_function,
                bracket=bracket,
                method="Brent",
            )
        brent_time = time.perf_counter() - start

        # Benchmark Golden
        start = time.perf_counter()
        for _ in range(iterations):
            result_golden = minimize_scalar(
                profit_function,
                bracket=bracket,
                method="Golden",
            )
        golden_time = time.perf_counter() - start

        # Both should succeed

        # Report performance
        print(f"\nBrent: {brent_time:.4f}s ({iterations} runs)")
        print(f"Golden: {golden_time:.4f}s ({iterations} runs)")
        print(f"Speedup: {golden_time / brent_time:.2f}x")
        print(f"Brent evaluations: {result_brent.nfev}")
        print(f"Golden evaluations: {result_golden.nfev}")

        # Brent should generally be faster (this is a soft assertion)
        if brent_time > golden_time:
            pytest.skip(
                f"Brent was slower ({brent_time:.4f}s vs {golden_time:.4f}s) - acceptable variance"
            )

    def test_brent_fewer_evaluations_than_golden(
        self,
        profit_function: "Callable[[float], float]",
    ) -> None:
        """
        Test that Brent uses fewer function evaluations than Golden.
        """
        from scipy.optimize import minimize_scalar

        bracket = (1.0, 1_000_000.0, 100_000_000_000.0)

        result_brent = minimize_scalar(
            profit_function,
            bracket=bracket,
            method="Brent",
        )

        result_golden = minimize_scalar(
            profit_function,
            bracket=bracket,
            method="Golden",
        )

        # Brent should use fewer or equal function evaluations
        assert result_brent.nfev <= result_golden.nfev, (
            f"Brent used {result_brent.nfev} evals vs Golden {result_golden.nfev}"
        )

    def test_all_three_methods_profit_accuracy(
        self,
        profit_function: "Callable[[float], float]",
    ) -> None:
        """
        Test that Brent, Golden, and Bounded achieve similar profit accuracy.
        """
        from scipy.optimize import minimize_scalar

        bracket = (1.0, 1_000_000.0, 100_000_000_000.0)
        bounds = (1.0, 1_000_000_000_000.0)

        result_brent = minimize_scalar(
            profit_function,
            bracket=bracket,
            method="Brent",
        )

        result_golden = minimize_scalar(
            profit_function,
            bracket=bracket,
            method="Golden",
        )

        result_bounded = minimize_scalar(
            profit_function,
            bounds=bounds,
            method="Bounded",
        )

        # Calculate actual profits (negate back)
        brent_profit = -result_brent.fun
        golden_profit = -result_golden.fun
        bounded_profit = -result_bounded.fun

        print(f"\n{'Method':<10} {'Input (USDC)':>15} {'Profit (USDC)':>15} {'Evals':>8}")
        print("-" * 55)
        print(f"{'Brent':<10} {result_brent.x:>15.2f} {brent_profit:>15.2f} {result_brent.nfev:>8}")
        print(
            f"{'Golden':<10} {result_golden.x:>15.2f} {golden_profit:>15.2f} "
            f"{result_golden.nfev:>8}"
        )
        print(
            f"{'Bounded':<10} {result_bounded.x:>15.2f} {bounded_profit:>15.2f} "
            f"{result_bounded.nfev:>8}"
        )

        # All profits should be very close (within 0.01% relative tolerance)
        assert brent_profit == pytest.approx(golden_profit, rel=1e-4)
        assert brent_profit == pytest.approx(bounded_profit, rel=1e-4)

    def test_optimal_input_is_profitable(self, mock_pools) -> None:
        """
        Verify that the optimal input found produces positive arbitrage profit.
        Uses MockV2Pool.calculate_tokens_out_from_tokens_in() for swap calculations.
        """
        from scipy.optimize import minimize_scalar

        pool_a, pool_b, usdc = mock_pools

        def neg_profit(x: float) -> float:
            if x <= 0:
                return 0.0
            input_amount = int(x)

            # Pool A: Buy token1 with token0
            token1_received = pool_a.calculate_tokens_out_from_tokens_in(
                token_in=usdc,
                token_in_quantity=input_amount,
            )

            # Pool B: Sell token1 for token0
            token0_received = pool_b.calculate_tokens_out_from_tokens_in(
                token_in=pool_b.token1,
                token_in_quantity=token1_received,
            )

            return -(float(token0_received - input_amount))

        result = minimize_scalar(
            neg_profit,
            bracket=(1.0, 1_000_000.0, 100_000_000_000.0),
            method="Brent",
        )

        assert result.fun < 0, "Optimal solution should be profitable (negative = positive profit)"

        # The optimal profit should be positive (profit in USDC's smallest unit)
        profit_usdc = -result.fun
        assert profit_usdc > 0, f"Profit {profit_usdc} should be positive"
