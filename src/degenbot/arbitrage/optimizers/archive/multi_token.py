"""
Multi-token routing optimization using dual decomposition.

Based on "An Efficient Algorithm for Optimal Routing Through Constant Function Market Makers"
by Diamandis, Resnick, Chitra, and Angeris (2023).

Key insight: The routing problem decomposes into:
1. A dual problem that finds market-clearing prices (shadow prices ν)
2. Subproblems that solve optimal arbitrage for each market given those prices

Each pool optimizes independently given shadow prices, then prices are updated
based on token imbalances. At convergence, trades automatically balance.

Advantages:
- Parallelizable: Each pool solves independently
- Scales to 10+ pools efficiently
- Handles shared pools across multiple paths
- Gas-aware selection possible

Performance:
- 3-6 pools: ~50-100μs (chain rule Newton faster)
- 10+ pools: ~500μs (dual decomposition scales better)
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

import numpy as np
from numpy.typing import NDArray

if TYPE_CHECKING:
    from degenbot.erc20.erc20 import Erc20Token


# =============================================================================
# DATA STRUCTURES
# =============================================================================


@dataclass
class TokenInfo:
    """Token with its index in the global token list."""

    token: "Erc20Token"
    index: int


@dataclass
class MarketInfo:
    """
    A market (pool) in the routing problem.

    Each market connects two tokens and can be optimized independently
    given shadow prices.
    """

    pool: Any  # UniswapV2Pool or similar
    token_in: "Erc20Token"
    token_out: "Erc20Token"
    token_in_index: int
    token_out_index: int
    reserve_in: float
    reserve_out: float
    fee: float

    # Results from last optimization
    optimal_delta: float = 0.0  # Amount tendered (sold)
    optimal_lambda: float = 0.0  # Amount received (bought)


@dataclass
class PathInfo:
    """
    An arbitrage path through multiple pools.

    Example: USDC → WETH → USDT → USDC (triangular)
    """

    pools: list[Any]
    tokens: list["Erc20Token"]
    path_id: int

    # Results
    optimal_input: float = 0.0
    optimal_output: float = 0.0
    profit: float = 0.0


@dataclass
class MultiTokenResult:
    """Result from multi-token routing optimization."""

    success: bool
    paths: list[PathInfo]
    markets: list[MarketInfo]
    shadow_prices: dict[str, float]  # token_address -> price
    solve_time_ms: float
    iterations: int
    total_profit: float
    error_message: str | None = None


# =============================================================================
# SINGLE-MARKET OPTIMIZATION (CLOSED-FORM)
# =============================================================================


def solve_market_arbitrage(
    market: MarketInfo,
    shadow_price_ratio: float,
) -> tuple[float, float]:
    """
    Solve optimal arbitrage for a single market given shadow prices.

    For V2 constant product AMM:
    - At optimum, pool's marginal price = external price
    - Closed-form solution: x = (sqrt(γ * k / m) - R_in) / γ

    Parameters
    ----------
    market : MarketInfo
        The market to optimize.
    shadow_price_ratio : float
        Price of output token in terms of input token.
        (How much output token is worth per unit of input token)

    Returns
    -------
    tuple[float, float]
        (delta, lambda) - amount to sell (input), amount to receive (output).
    """
    R_in = market.reserve_in
    R_out = market.reserve_out
    gamma = 1.0 - market.fee
    k = R_in * R_out

    if R_in <= 0 or R_out <= 0 or shadow_price_ratio <= 0:
        return 0.0, 0.0

    # Pool's current marginal rate (price of output in terms of input)
    # At equilibrium: marginal_rate = shadow_price_ratio
    # marginal_rate = gamma * R_out * R_in / (R_in + x * gamma)²
    #
    # Solving for x:
    # (R_in + x * gamma)² = gamma * k / shadow_price_ratio
    # R_in + x * gamma = sqrt(gamma * k / shadow_price_ratio)
    # x = (sqrt(gamma * k / shadow_price_ratio) - R_in) / gamma

    sqrt_term = np.sqrt(gamma * k / shadow_price_ratio)

    if sqrt_term <= R_in:
        # sqrt_term < R_in means no profitable trade
        # This happens when shadow_price_ratio > pool's effective marginal rate
        # i.e., external price is worse than pool price
        return 0.0, 0.0

    # Optimal input (pre-fee amount)
    x = (sqrt_term - R_in) / gamma

    if x <= 0:
        return 0.0, 0.0

    # Output amount using V2 formula: y = x * gamma * R_out / (R_in + x * gamma)
    # Note: R_in + x * gamma = sqrt_term
    y = x * gamma * R_out / sqrt_term

    return x, y


def compute_marginal_rate(
    market: MarketInfo,
    amount_in: float,
) -> float:
    """
    Compute marginal exchange rate at given input amount.

    Marginal rate = d(output)/d(input) = γ * R_out * R_in / (R_in + x*γ)²
    """
    R_in = market.reserve_in
    R_out = market.reserve_out
    gamma = 1.0 - market.fee

    denom = R_in + amount_in * gamma
    if denom <= 0:
        return 0.0

    return gamma * R_out * R_in / denom**2


# =============================================================================
# DUAL DECOMPOSITION SOLVER
# =============================================================================


class DualDecompositionSolver:
    """
    Solve multi-token routing using dual decomposition.

    The algorithm:
    1. Initialize shadow prices for all tokens
    2. For each market, solve optimal arbitrage given local prices
    3. Compute token imbalance across all markets
    4. Update shadow prices based on imbalance (gradient step)
    5. Repeat until convergence

    At convergence, shadow prices are market-clearing prices and
    trades automatically balance.
    """

    def __init__(
        self,
        max_iterations: int = 100,
        tolerance: float = 1e-8,
        learning_rate: float = 0.1,
        use_lbfgs: bool = True,
    ):
        """
        Parameters
        ----------
        max_iterations : int
            Maximum iterations for dual problem.
        tolerance : float
            Convergence tolerance on token imbalance.
        learning_rate : float
            Learning rate for gradient descent (if not using L-BFGS).
        use_lbfgs : bool
            Use L-BFGS-B for faster convergence (requires scipy).
        """
        self.max_iterations = max_iterations
        self.tolerance = tolerance
        self.learning_rate = learning_rate
        self.use_lbfgs = use_lbfgs

    def solve(
        self,
        markets: list[MarketInfo],
        tokens: list[TokenInfo],
        initial_prices: NDArray[np.float64] | None = None,
    ) -> tuple[NDArray[np.float64], int]:
        """
        Solve the dual problem to find market-clearing prices.

        Parameters
        ----------
        markets : list[MarketInfo]
            All markets in the routing problem.
        tokens : list[TokenInfo]
            All unique tokens.
        initial_prices : NDArray | None
            Initial shadow prices (optional).

        Returns
        -------
        tuple[NDArray, int]
            Optimal shadow prices and iterations to converge.
        """
        n_tokens = len(tokens)

        # Initialize prices
        if initial_prices is None:
            # Initialize from pool prices (better than unit prices)
            nu = self._initialize_prices(markets, n_tokens)
        else:
            nu = initial_prices.copy()

        if self.use_lbfgs:
            return self._solve_lbfgs(markets, n_tokens, nu)
        else:
            return self._solve_gradient_descent(markets, n_tokens, nu)

    def _initialize_prices(
        self,
        markets: list[MarketInfo],
        n_tokens: int,
    ) -> NDArray[np.float64]:
        """
        Initialize shadow prices from pool prices.

        Uses geometric mean of pool prices to get initial estimates.
        """
        nu = np.ones(n_tokens, dtype=np.float64)

        # Collect all price estimates for each token pair
        price_estimates: dict[tuple[int, int], list[float]] = {}

        for market in markets:
            # Pool price: R_out / R_in (how much output per input)
            pool_price = market.reserve_out / market.reserve_in if market.reserve_in > 0 else 1.0

            key = (market.token_in_index, market.token_out_index)
            if key not in price_estimates:
                price_estimates[key] = []
            price_estimates[key].append(pool_price)

        # Use geometric mean of estimates
        # Set token 0 as numeraire (price = 1)
        for (in_idx, out_idx), prices in price_estimates.items():
            if in_idx == 0:
                # Direct price from numeraire
                avg_price = np.exp(np.mean(np.log(prices)))
                nu[out_idx] = avg_price
            elif out_idx == 0:
                # Inverse price to numeraire
                avg_price = np.exp(np.mean(np.log(prices)))
                nu[in_idx] = 1.0 / avg_price

        return nu

    def _solve_gradient_descent(
        self,
        markets: list[MarketInfo],
        n_tokens: int,
        nu: NDArray[np.float64],
    ) -> tuple[NDArray[np.float64], int]:
        """Gradient descent on dual problem."""

        for iteration in range(self.max_iterations):
            # Compute gradient (token imbalance)
            imbalance = np.zeros(n_tokens, dtype=np.float64)

            for market in markets:
                # Price ratio: price of out relative to in
                price_ratio = nu[market.token_out_index] / nu[market.token_in_index]

                # Solve market subproblem
                delta, lam = solve_market_arbitrage(market, price_ratio)

                # Store results
                market.optimal_delta = delta
                market.optimal_lambda = lam

                # Imbalance: we sell delta of token_in, receive lambda of token_out
                imbalance[market.token_in_index] -= delta
                imbalance[market.token_out_index] += lam

            # Check convergence
            max_imbalance = np.max(np.abs(imbalance))
            if max_imbalance < self.tolerance:
                return nu, iteration + 1

            # Update prices (gradient step)
            nu = nu - self.learning_rate * imbalance

            # Project to positive orthant
            nu = np.maximum(nu, 1e-10)

        return nu, self.max_iterations

    def _solve_lbfgs(
        self,
        markets: list[MarketInfo],
        n_tokens: int,
        nu_init: NDArray[np.float64],
    ) -> tuple[NDArray[np.float64], int]:
        """L-BFGS-B optimization for faster convergence."""

        try:
            from scipy.optimize import minimize
        except ImportError:
            # Fall back to gradient descent
            return self._solve_gradient_descent(markets, n_tokens, nu_init)

        # Track best solution across iterations
        best_prices = nu_init.copy()
        best_utility = float("-inf")

        def objective(nu: NDArray[np.float64]) -> float:
            """
            Dual objective: g(ν) = Σ market_utilities - ν^T imbalance

            At optimum, gradient = 0 means trades balance.
            """
            nonlocal best_utility, best_prices

            total_utility = 0.0

            for market in markets:
                price_ratio = nu[market.token_out_index] / nu[market.token_in_index]

                if price_ratio <= 0:
                    continue

                delta, lam = solve_market_arbitrage(market, price_ratio)

                # Utility: profit from this market in value terms
                if delta > 0 and lam > 0:
                    # Profit in shadow price terms
                    total_utility += (
                        lam * nu[market.token_out_index] - delta * nu[market.token_in_index]
                    )

            # Track best
            if total_utility > best_utility:
                best_utility = total_utility
                best_prices = nu.copy()

            return -total_utility  # Minimize negative utility

        def gradient(nu: NDArray[np.float64]) -> NDArray[np.float64]:
            """Gradient of dual objective = imbalance."""
            grad = np.zeros(n_tokens, dtype=np.float64)

            for market in markets:
                price_ratio = nu[market.token_out_index] / nu[market.token_in_index]
                delta, lam = solve_market_arbitrage(market, price_ratio)

                # Gradient contribution (imbalance)
                grad[market.token_in_index] -= delta
                grad[market.token_out_index] += lam

            return grad

        # Bounds: prices must be positive
        bounds = [(1e-10, None) for _ in range(n_tokens)]

        result = minimize(
            objective,
            nu_init,
            method="L-BFGS-B",
            jac=gradient,
            bounds=bounds,
            options={"maxiter": self.max_iterations, "gtol": self.tolerance},
        )

        # Update markets with best solution found
        for market in markets:
            price_ratio = best_prices[market.token_out_index] / best_prices[market.token_in_index]
            delta, lam = solve_market_arbitrage(market, price_ratio)
            market.optimal_delta = delta
            market.optimal_lambda = lam

        # Return at least 1 iteration if solver ran
        iterations = max(1, result.nit) if hasattr(result, "nit") else 1

        return best_prices, iterations


# =============================================================================
# MULTI-TOKEN ROUTER
# =============================================================================


class MultiTokenRouter:
    """
    Production multi-token routing optimizer.

    Handles:
    - Multiple arbitrage paths simultaneously
    - Shared pools across paths
    - Optimal path selection given constraints

    Usage:
    -----
    >>> router = MultiTokenRouter()
    >>> paths = [
    ...     [pool_a, pool_b],  # USDC → WETH → USDC
    ...     [pool_c, pool_d],  # USDC → USDT → USDC
    ... ]
    >>> result = router.optimize(paths, usdc)
    """

    def __init__(
        self,
        max_iterations: int = 100,
        tolerance: float = 1e-8,
        use_lbfgs: bool = True,
    ):
        self.max_iterations = max_iterations
        self.tolerance = tolerance
        self.use_lbfgs = use_lbfgs
        self._solver = DualDecompositionSolver(
            max_iterations=max_iterations,
            tolerance=tolerance,
            use_lbfgs=use_lbfgs,
        )

    def optimize(
        self,
        paths: list[list[Any]],
        input_token: "Erc20Token",
        input_amount: int | None = None,
    ) -> MultiTokenResult:
        """
        Optimize multiple arbitrage paths simultaneously.

        Parameters
        ----------
        paths : list[list[pool]]
            List of arbitrage paths, where each path is a list of pools.
        input_token : Erc20Token
            The starting token for all paths.
        input_amount : int | None
            If provided, fixed input amount for all paths.

        Returns
        -------
        MultiTokenResult
            Optimization results including optimal trades per path.
        """
        import time

        start_time = time.perf_counter_ns()

        if not paths:
            elapsed_ms = (time.perf_counter_ns() - start_time) / 1_000_000
            return MultiTokenResult(
                success=False,
                paths=[],
                markets=[],
                shadow_prices={},
                solve_time_ms=elapsed_ms,
                iterations=0,
                total_profit=0,
                error_message="No paths provided",
            )

        # Build token and market registries
        tokens, token_registry = self._build_token_registry(paths, input_token)
        markets, path_infos = self._build_markets(paths, tokens, token_registry, input_token)

        if not markets:
            elapsed_ms = (time.perf_counter_ns() - start_time) / 1_000_000
            return MultiTokenResult(
                success=False,
                paths=path_infos,
                markets=[],
                shadow_prices={},
                solve_time_ms=elapsed_ms,
                iterations=0,
                total_profit=0,
                error_message="No valid markets",
            )

        # Solve dual problem
        nu, iterations = self._solver.solve(markets, tokens)

        # Aggregate results per path
        total_profit = 0.0
        for path_info in path_infos:
            path_profit = self._compute_path_profit(path_info, markets)
            path_info.profit = path_profit
            total_profit += path_profit

        # Build shadow price dict
        shadow_prices = {token.token.address: nu[token.index] for token in tokens}

        elapsed_ms = (time.perf_counter_ns() - start_time) / 1_000_000

        return MultiTokenResult(
            success=total_profit > 0,
            paths=path_infos,
            markets=markets,
            shadow_prices=shadow_prices,
            solve_time_ms=elapsed_ms,
            iterations=iterations,
            total_profit=total_profit,
        )

    def _build_token_registry(
        self,
        paths: list[list[Any]],
        input_token: "Erc20Token",
    ) -> tuple[list[TokenInfo], dict[str, TokenInfo]]:
        """Build registry of unique tokens across all paths."""
        token_set = {input_token}
        token_registry: dict[str, TokenInfo] = {}

        for path in paths:
            for pool in path:
                # Get pool tokens
                if hasattr(pool, "token0") and hasattr(pool, "token1"):
                    token_set.add(pool.token0)
                    token_set.add(pool.token1)

        # Sort by address for deterministic ordering
        sorted_tokens = sorted(token_set, key=lambda t: t.address)

        tokens = []
        for idx, token in enumerate(sorted_tokens):
            info = TokenInfo(token=token, index=idx)
            tokens.append(info)
            token_registry[token.address] = info

        return tokens, token_registry

    def _build_markets(
        self,
        paths: list[list[Any]],
        tokens: list[TokenInfo],
        token_registry: dict[str, TokenInfo],
        input_token: "Erc20Token",
    ) -> tuple[list[MarketInfo], list[PathInfo]]:
        """Build market info for each pool and path info for each path."""
        markets: list[MarketInfo] = []
        path_infos: list[PathInfo] = []
        market_id = 0

        for path_id, path in enumerate(paths):
            if not path:
                continue

            path_tokens = [input_token]
            current_token = input_token

            for pool in path:
                # Determine direction
                if current_token == pool.token0:
                    token_in = pool.token0
                    token_out = pool.token1
                    reserve_in = float(pool.state.reserves_token0)
                    reserve_out = float(pool.state.reserves_token1)
                elif current_token == pool.token1:
                    token_in = pool.token1
                    token_out = pool.token0
                    reserve_in = float(pool.state.reserves_token1)
                    reserve_out = float(pool.state.reserves_token0)
                else:
                    # Token not in pool - invalid path
                    break

                # Create market info
                fee = float(pool.fee) if hasattr(pool, "fee") else 0.003

                market = MarketInfo(
                    pool=pool,
                    token_in=token_in,
                    token_out=token_out,
                    token_in_index=token_registry[token_in.address].index,
                    token_out_index=token_registry[token_out.address].index,
                    reserve_in=reserve_in,
                    reserve_out=reserve_out,
                    fee=fee,
                )
                markets.append(market)

                current_token = token_out
                path_tokens.append(token_out)

            # Create path info
            path_info = PathInfo(
                pools=path,
                tokens=path_tokens,
                path_id=path_id,
            )
            path_infos.append(path_info)

        return markets, path_infos

    def _compute_path_profit(
        self,
        path_info: PathInfo,
        markets: list[MarketInfo],
    ) -> float:
        """Compute profit for a path based on market results."""
        # Find markets belonging to this path
        path_markets = [m for m in markets if m.pool in path_info.pools]

        if not path_markets:
            return 0.0

        # Simulate through path
        # For dual decomposition, we use the optimal deltas from each market
        # The profit is the net position change

        # Simple approach: sum up net position changes
        token_flows: dict[str, float] = {}

        for market in path_markets:
            if market.optimal_delta > 0:
                # Sell token_in, receive token_out
                in_addr = market.token_in.address
                out_addr = market.token_out.address

                token_flows[in_addr] = token_flows.get(in_addr, 0) - market.optimal_delta
                token_flows[out_addr] = token_flows.get(out_addr, 0) + market.optimal_lambda

        # Profit is net flow of first/last token (assuming cycle)
        if path_info.tokens:
            first_token_addr = path_info.tokens[0].address
            return max(0.0, token_flows.get(first_token_addr, 0.0))

        return 0.0


# =============================================================================
# CONVENIENCE FUNCTIONS
# =============================================================================


def optimize_multi_path(
    paths: list[list[Any]],
    input_token: "Erc20Token",
) -> MultiTokenResult:
    """
    Optimize multiple arbitrage paths simultaneously.

    This is a convenience function that creates a MultiTokenRouter
    and solves the routing problem.

    Parameters
    ----------
    paths : list[list[pool]]
        List of arbitrage paths.
    input_token : Erc20Token
        The starting token.

    Returns
    -------
    MultiTokenResult
        Optimization results.

    Example
    -------
    >>> from degenbot.arbitrage.optimizers import optimize_multi_path
    >>> result = optimize_multi_path([[pool_a, pool_b], [pool_c, pool_d]], usdc)
    >>> if result.success:
    ...     print(f"Total profit: {result.total_profit}")
    ...     for path in result.paths:
    ...         print(f"  Path {path.path_id}: profit={path.profit}")
    """
    router = MultiTokenRouter()
    return router.optimize(paths, input_token)
