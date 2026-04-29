"""
Closed-form solver for N-token Balancer weighted pool arbitrage.

Based on "Closed-form solutions for generic N-token AMM arbitrage"
by Willetts & Harrington (QuantAMM.fi, Feb 2024).

Key implementation detail: the paper defines d_i = I_{s_i=1}, an indicator
that is 1 when depositing and 0 when withdrawing — NOT -1/+1.
"""

from __future__ import annotations

import itertools
from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from fractions import Fraction

    from degenbot.balancer.pools import BalancerV2Pool

# ---------------------------------------------------------------------------
# Types
# ---------------------------------------------------------------------------

TradeSignature = tuple[int, ...]


@dataclass(frozen=True, slots=True)
class BalancerMultiTokenState:
    """
    State for an N-token Balancer weighted pool.

    Attributes
    ----------
    reserves : tuple[int, ...]
        Token reserves in wei.
    weights : tuple[int, ...]
        Normalized weights as 18-decimal fixed point (sum = 1e18).
    fee : Fraction
        Swap fee as exact fraction.
    decimals : tuple[int, ...]
        Decimal places for each token (e.g. 18 for ETH, 6 for USDC).
        All reserves are internally upscaled to 18-decimal before applying
        the closed-form formula, matching Balancer Vault behavior.
    """

    reserves: tuple[int, ...]
    weights: tuple[int, ...]
    fee: Fraction
    decimals: tuple[int, ...] = ()

    @property
    def n_tokens(self) -> int:
        return len(self.reserves)

    def _scaling_factors(self) -> list[int]:
        """Compute scaling factors to upscale reserves to 18-decimal."""
        if not self.decimals:
            return [1] * self.n_tokens
        return [10 ** (18 - d) for d in self.decimals]

    def upscaled_reserves(self) -> tuple[float, ...]:
        """Reserves upscaled to 18-decimal (Balancer Vault convention)."""
        factors = self._scaling_factors()
        return tuple(float(r * f) for r, f in zip(self.reserves, factors, strict=True))

    def descale_trade(self, trade: float, token_index: int) -> int:
        """Descale a trade from 18-decimal units back to native token units."""
        if not self.decimals:
            return int(round(trade))  # noqa: RUF046
        factor = 10 ** (18 - self.decimals[token_index])
        return int(round(trade / factor))  # noqa: RUF046


@dataclass(frozen=True, slots=True)
class MultiTokenArbitrageResult:
    """
    Result of multi-token arbitrage optimization.

    Attributes
    ----------
    trades : tuple[float, ...]
        Optimal trade amounts (positive = deposit, negative = withdraw).
    profit : float
        Expected profit in numéraire units.
    success : bool
        Whether a profitable trade was found.
    signature : TradeSignature
        Trade signature that produced this result.
    iterations : int
        Number of signatures evaluated.
    """

    trades: tuple[float, ...]
    profit: float
    success: bool
    signature: TradeSignature
    iterations: int


# ---------------------------------------------------------------------------
# Signature Generation
# ---------------------------------------------------------------------------

MIN_ACTIVE_TOKENS = 2
INVARIANT_TOLERANCE = 1e-6


def generate_trade_signatures(n_tokens: int) -> list[TradeSignature]:
    """
    Generate all valid trade signatures for an N-token pool.

    N=3: 12 signatures, N=4: 50, N=5: 180.
    """
    return [s for s in itertools.product((-1, 0, 1), repeat=n_tokens) if 1 in s and -1 in s]


# ---------------------------------------------------------------------------
# Closed-form Solution (Equation 9) — Corrected
# ---------------------------------------------------------------------------


def _compute_d(signature: TradeSignature) -> list[int]:
    """
    Compute d_i = I_{s_i=1} per the paper's definition.

    d_i = 1 if depositing (signature[i] == 1)
    d_i = 0 if withdrawing (signature[i] == -1)
    d_i = 0 if not traded (signature[i] == 0)
    """
    return [1 if s == 1 else 0 for s in signature]


def compute_optimal_trade(
    pool: BalancerMultiTokenState,
    market_prices: tuple[float, ...],
    signature: TradeSignature,
) -> tuple[float, ...]:
    """
    Compute optimal trade amounts for a given signature using Equation 9.

    All reserves are internally upscaled to 18-decimal before applying
    the formula. The resulting trades are in the upscaled 18-decimal space
    and must be descaled to native token units for on-chain use.
    """
    n = pool.n_tokens
    gamma = 1.0 - float(pool.fee)

    # d_i = I_{s_i=1}: 1 for deposit, 0 for withdraw
    d = _compute_d(signature)

    # Active token indices
    active_indices = [i for i in range(n) if signature[i] != 0]

    if len(active_indices) < MIN_ACTIVE_TOKENS:
        return tuple(0.0 for _ in range(n))

    # Use upscaled reserves (all in 18-decimal) for the formula
    reserves_f = pool.upscaled_reserves()

    # Active-token normalized weights: sum to 1.0
    active_weight_sum = sum(pool.weights[i] for i in active_indices)
    w_tilde = [pool.weights[i] / active_weight_sum if signature[i] != 0 else 0.0 for i in range(n)]

    # k_tilde = prod(R_i^w_tilde_i) for active tokens (upscaled)
    k_tilde = 1.0
    for i in active_indices:
        k_tilde *= reserves_f[i] ** w_tilde[i]

    # Compute Phi_i for each active token (in upscaled 18-decimal units)
    trades: list[float] = [0.0] * n

    for i in active_indices:
        gamma_pow_di = gamma ** d[i]
        term_i = (w_tilde[i] * gamma_pow_di / market_prices[i]) ** (1 - w_tilde[i])

        # product_j = prod_{j!=i, j in A} (m_p,j / (w_tilde_j * gamma^(d_j))) ^ w_tilde_j
        product_j = 1.0
        for j in active_indices:
            if j == i:
                continue
            gamma_pow_dj = gamma ** d[j]
            denom_j = w_tilde[j] * gamma_pow_dj
            product_j *= (market_prices[j] / denom_j) ** w_tilde[j]

        bracket = k_tilde * term_i * product_j - reserves_f[i]

        # Phi_i = gamma^(-d_i) * bracket (in upscaled 18-decimal units)
        trades[i] = (gamma ** (-d[i])) * bracket

    return tuple(trades)


def validate_trade(
    trades: tuple[float, ...],
    signature: TradeSignature,
    pool: BalancerMultiTokenState,
) -> bool:
    """
    Validate that the trade:
    1. Respects the signature (direction matches)
    2. Doesn't withdraw more than available
    3. Maintains the invariant with fees

    All calculations use upscaled 18-decimal reserves.
    """
    gamma = 1.0 - float(pool.fee)
    reserves_f = pool.upscaled_reserves()

    for i_, (trade, sig) in enumerate(zip(trades, signature, strict=True)):
        if sig == 1 and trade < 0:
            return False
        if sig == -1 and trade > 0:
            return False
        if trade < 0 and abs(trade) >= reserves_f[i_]:
            return False

    # Invariant check using upscaled reserves
    active_indices = [i for i in range(len(trades)) if signature[i] != 0]
    active_weight_sum = sum(pool.weights[i] for i in active_indices)

    product = 1.0
    for i in active_indices:
        w_tilde = pool.weights[i] / active_weight_sum
        effective = gamma * trades[i] if signature[i] == 1 else trades[i]
        new_reserve = reserves_f[i] + effective
        if new_reserve <= 0:
            return False
        product *= new_reserve**w_tilde

    k_tilde = 1.0
    for i in active_indices:
        w_tilde = pool.weights[i] / active_weight_sum
        k_tilde *= reserves_f[i] ** w_tilde

    relative_error = abs(product - k_tilde) / k_tilde
    return relative_error < INVARIANT_TOLERANCE


def compute_profit_token_units(
    trades: tuple[float, ...],
    market_prices: tuple[float, ...],
) -> float:
    """
    Compute profit from upscaled 18-decimal trades at market prices.

    Converts upscaled 18-decimal trades to token units before
    multiplying by market prices, ensuring dimensional consistency.

    Profit = -sum(market_price_i * Phi_i_in_tokens)
    """
    total = 0.0
    for i in range(len(trades)):
        token_amount = trades[i] / 1e18
        total += market_prices[i] * token_amount
    return -total


# ---------------------------------------------------------------------------
# Integer Refinement
# ---------------------------------------------------------------------------


def refine_to_integer(
    trades: tuple[float, ...],
    signature: TradeSignature,
    pool: BalancerMultiTokenState,
    market_prices: tuple[float, ...],
    search_radius: int = 3,
) -> tuple[int, ...]:
    """
    Refine float trades to integer amounts in native token units.

    Trades are in upscaled 18-decimal units. We descale to native
    units (accounting for each token's decimals), round, and search.
    """
    n = pool.n_tokens
    active_indices = [i for i in range(n) if signature[i] != 0]

    if len(active_indices) < MIN_ACTIVE_TOKENS:
        return tuple(0 for _ in range(n))

    # Descale trades from upscaled 18-decimal to native token units
    int_trades = [pool.descale_trade(trades[i], i) for i in range(n)]

    for i in active_indices:
        if signature[i] == 1 and int_trades[i] < 0:
            int_trades[i] = 0
        if signature[i] == -1 and int_trades[i] > 0:
            int_trades[i] = 0

    best_trades = int_trades.copy()
    best_profit = -float("inf")

    search_ranges = []
    for i in active_indices:
        base = int_trades[i]
        search_ranges.append(range(base - search_radius, base + search_radius + 1))

    max_combinations = 1000
    combination_count = 1
    for sr in search_ranges:
        combination_count *= len(sr)

    if combination_count <= max_combinations:
        for combo in itertools.product(*search_ranges):
            candidate = int_trades.copy()
            for idx, val in zip(active_indices, combo, strict=True):
                candidate[idx] = val

            valid = True
            for i in active_indices:
                if signature[i] == 1 and candidate[i] < 0:
                    valid = False
                    break
                if signature[i] == -1 and candidate[i] > 0:
                    valid = False
                    break
                if candidate[i] < 0 and abs(candidate[i]) >= pool.reserves[i]:
                    valid = False
                    break

            if not valid:
                continue

            # Profit: convert native int trades to token units
            profit = -sum(
                market_prices[i] * (candidate[i] / 10 ** pool.decimals[i])
                if pool.decimals
                else market_prices[i] * candidate[i]
                for i in active_indices
            )
            if profit > best_profit:
                best_profit = profit
                best_trades = candidate.copy()
    else:
        profit = -sum(
            market_prices[i] * (int_trades[i] / 10 ** pool.decimals[i])
            if pool.decimals
            else market_prices[i] * int_trades[i]
            for i in active_indices
        )
        best_profit = profit

    return tuple(best_trades)


# ---------------------------------------------------------------------------
# Main Solver
# ---------------------------------------------------------------------------


class BalancerWeightedPoolSolver:
    """
    Closed-form solver for N-token Balancer weighted pool arbitrage.

    Uses Equation 9 from Willetts & Harrington (2024) with correct
    d_i = I_{s_i=1} indicator (1 for deposit, 0 for withdraw).
    """

    def __init__(
        self,
        *,
        use_heuristic_pruning: bool = False,
        max_signatures: int = 500,
    ) -> None:
        self.use_heuristic_pruning = use_heuristic_pruning
        self.max_signatures = max_signatures

    def solve(  # noqa: PLR6301
        self,
        pool: BalancerMultiTokenState,
        market_prices: tuple[float, ...],
        max_input: float | None = None,
    ) -> MultiTokenArbitrageResult:
        """Find optimal multi-token arbitrage trade."""
        n = pool.n_tokens

        if n != len(market_prices):
            return MultiTokenArbitrageResult(
                trades=tuple(0.0 for _ in range(n)),
                profit=0.0,
                success=False,
                signature=tuple(0 for _ in range(n)),
                iterations=0,
            )

        signatures = generate_trade_signatures(n)

        best_result: MultiTokenArbitrageResult | None = None
        best_profit = 0.0

        for signature in signatures:
            trades = compute_optimal_trade(pool, market_prices, signature)

            if not validate_trade(trades, signature, pool):
                continue

            profit = compute_profit_token_units(trades, market_prices)

            if max_input is not None:
                total_input = sum(market_prices[i] * max(0.0, trades[i] / 1e18) for i in range(n))
                if total_input > max_input:
                    continue

            if profit > best_profit:
                best_profit = profit
                int_trades = refine_to_integer(trades, signature, pool, market_prices)
                # Profit in numéraire: convert native int trades to token units
                int_profit = -sum(
                    market_prices[i] * (int_trades[i] / 10 ** pool.decimals[i])
                    if pool.decimals
                    else market_prices[i] * int_trades[i]
                    for i in range(n)
                )

                if int_profit > 0:
                    best_result = MultiTokenArbitrageResult(
                        trades=tuple(float(t) for t in int_trades),
                        profit=int_profit,
                        success=True,
                        signature=signature,
                        iterations=len(signatures),
                    )

        if best_result is None:
            return MultiTokenArbitrageResult(
                trades=tuple(0.0 for _ in range(n)),
                profit=0.0,
                success=False,
                signature=tuple(0 for _ in range(n)),
                iterations=len(signatures),
            )

        return best_result


def balancer_pool_to_state(pool: BalancerV2Pool) -> BalancerMultiTokenState:
    """Convert a BalancerV2Pool object to BalancerMultiTokenState."""
    decimals = tuple(token.decimals for token in pool.tokens) if hasattr(pool, "tokens") else ()
    return BalancerMultiTokenState(
        reserves=pool.balances,
        weights=pool.weights,
        fee=pool.fee,
        decimals=decimals,
    )
