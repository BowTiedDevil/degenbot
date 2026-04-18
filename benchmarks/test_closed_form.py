"""
Final verification: closed-form Balancer solver correctness + performance.
"""

import time
from fractions import Fraction

import numpy as np

from degenbot.arbitrage.optimizers.balancer_weighted import (
    BalancerMultiTokenState,
    BalancerWeightedPoolSolver,
    _compute_d,
    compute_optimal_trade,
    compute_profit_token_units,
    validate_trade,
)

# ---------------------------------------------------------------------------
# Pool Setup
# ---------------------------------------------------------------------------

# True equilibrium pool: 1000 WETH @ $2000, 1M USDC @ $1, 1M DAI @ $1
# V = $4M, w_weth = 50%, w_usdc = 25%, w_dai = 25%
POOL = BalancerMultiTokenState(
    reserves=(1_000_000_000_000_000_000_000, 1_000_000_000_000, 1_000_000_000_000),
    weights=(500_000_000_000_000_000, 250_000_000_000_000_000, 250_000_000_000_000_000),
    fee=Fraction(3, 1000),
    decimals=(18, 6, 6),
)

solver = BalancerWeightedPoolSolver()

# ---------------------------------------------------------------------------
# Correctness Tests
# ---------------------------------------------------------------------------

print("=" * 60)
print("CORRECTNESS VERIFICATION")
print("=" * 60)

# 1. Zero-fee equilibrium → exactly zero trade
pool_no_fee = BalancerMultiTokenState(
    reserves=POOL.reserves,
    weights=POOL.weights,
    fee=Fraction(0, 1),
    decimals=POOL.decimals,
)
for sig in [(1, -1, -1), (-1, 1, 1), (1, 1, -1), (1, -1, 1), (-1, -1, 1), (-1, 1, -1)]:
    trades = compute_optimal_trade(pool_no_fee, (2000.0, 1.0, 1.0), sig)
    max_trade = max(abs(t) for t in trades) / 1e18
    assert max_trade < 1e-6, f"Non-zero trade at equilibrium with zero fees: {max_trade}"
print("✓ Zero-fee equilibrium: all 6 active signatures give ~zero trades")

# 2. Direction correctness
result = solver.solve(POOL, (1900.0, 1.0, 1.0))  # ETH cheap → deposit ETH
assert result.success, "ETH cheap should find profitable trade"
assert result.signature[0] == 1, "Token 0 (ETH) should be deposited (+1)"
assert result.signature[1] == -1, "Token 1 (USDC) should be withdrawn (-1)"
assert result.signature[2] == -1, "Token 2 (DAI) should be withdrawn (-1)"
print(f"✓ ETH=$1900: deposit ETH, withdraw stables. Profit=${result.profit:.2f}")

result = solver.solve(POOL, (2100.0, 1.0, 1.0))  # ETH expensive → withdraw ETH
assert result.success, "ETH expensive should find profitable trade"
assert result.signature[0] == -1, "Token 0 (ETH) should be withdrawn (-1)"
assert result.signature[1] == 1, "Token 1 (USDC) should be deposited (+1)"
assert result.signature[2] == 1, "Token 2 (DAI) should be deposited (+1)"
print(f"✓ ETH=$2100: withdraw ETH, deposit stables. Profit=${result.profit:.2f}")

# 3. Equilibrium → no profit
result = solver.solve(POOL, (2000.0, 1.0, 1.0))
assert not result.success or result.profit < 1.0, "Equilibrium should have no profit"
print(f"✓ Equilibrium: no profitable trade (success={result.success})")

# 4. Invariant preservation
for prices in [(1900.0, 1.0, 1.0), (2100.0, 1.0, 1.0), (2100.0, 0.95, 0.90)]:
    for sig in [(1, -1, -1), (-1, 1, 1)]:
        trades = compute_optimal_trade(POOL, prices, sig)
        if validate_trade(trades, sig, POOL):
            profit = compute_profit_token_units(trades, prices)
            assert profit > 0, f"Valid trade should be profitable: sig={sig}, prices={prices}"
print("✓ All valid trades preserve invariant and are profitable")

# 5. d_i indicator
assert _compute_d((1, -1, 0)) == [1, 0, 0], "d_i should be 1 for deposit, 0 otherwise"
assert _compute_d((-1, 1, 1)) == [0, 1, 1], "d_i indicator correct"
print("✓ d_i indicator function correct")

# 6. Multi-token mispricing
result = solver.solve(POOL, (2100.0, 0.95, 0.90))
assert result.success, "Multi-token mispricing should find profitable trade"
assert result.profit > 1000, "Multi-token mispricing should be very profitable"
print(f"✓ Multi-token mispricing: profit=${result.profit:.2f}")

# ---------------------------------------------------------------------------
# Performance Benchmark
# ---------------------------------------------------------------------------

print()
print("=" * 60)
print("PERFORMANCE BENCHMARK")
print("=" * 60)

rng = np.random.default_rng(42)
n_iter = 2000

# Single Equation 9 evaluation
sig = (1, -1, -1)
single_times = []
for _ in range(n_iter):
    t0 = time.perf_counter_ns()
    compute_optimal_trade(POOL, (1900.0, 1.0, 1.0), sig)
    single_times.append(time.perf_counter_ns() - t0)
print(f"Single Eq.9 eval:  {np.median(single_times) / 1e3:.1f} μs (median)")

# Full solver
prices_list = [
    (2000.0 + rng.uniform(-200, 200), 1.0 + rng.uniform(-0.1, 0.1), 1.0 + rng.uniform(-0.1, 0.1))
    for _ in range(n_iter)
]
solver_times = []
for prices in prices_list:
    t0 = time.perf_counter_ns()
    solver.solve(POOL, prices)
    solver_times.append(time.perf_counter_ns() - t0)
print(f"Full solver (N=3): {np.median(solver_times) / 1e3:.1f} μs (median)")
print(f"Full solver (N=3): {np.percentile(solver_times, 99) / 1e3:.1f} μs (P99)")

# Success rate
successes = sum(1 for p in prices_list if solver.solve(POOL, p).success)
print(f"Success rate: {successes / n_iter * 100:.1f}%")

print()
print("All checks passed! ✓")
