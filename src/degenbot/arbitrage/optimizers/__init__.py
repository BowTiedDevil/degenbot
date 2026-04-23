"""
Arbitrage optimizers for different pool types.

Optimizer Selection Guide:
==========================

Single Path:
- V2-V2: MöbiusSolver (~5.8μs, 38x faster than Brent)
- V2-V3: MöbiusSolver (single-range) or Brent (complex crossings)
- V3-V3: Brent (handles multiple tick crossings)
- Multi-pool V2: ChainRuleNewtonOptimizer (~50-100μs)

Batch Processing:
- V2-V2 (10+ paths): BatchNewtonOptimizer (~0.5μs/path)

Automatic Selection:
- ArbSolver (unified dispatcher: Mobius → Newton → Brent)

Performance Summary:
===================

| Method | Time | vs Brent | Use Case |
|--------|------|----------|----------|
| Möbius | 5.8μs | 38x faster | V2 all paths |
| Batch Newton | 0.5μs/path | 200x faster | V2-V2 batch 100+ |
| Chain Rule | 50-100μs | 2-4x faster | 3-6 pools |
| Brent | 223μs | baseline | V2-V3, V3-V3 |

Quick Start:
===========

>>> from degenbot.arbitrage.optimizers.solver import ArbSolver, Hop, SolveInput
>>> solver = ArbSolver()
>>> hops = (
...     Hop(reserve_in=r0_in, reserve_out=r0_out, fee=fee0),
...     Hop(reserve_in=r1_in, reserve_out=r1_out, fee=fee1),
... )
>>> result = solver.solve(SolveInput(hops=hops))
>>> print(f"Optimal: {result.optimal_input}, Profit: {result.profit}")
"""

from degenbot.arbitrage.optimizers.balancer_weighted import (
    BalancerMultiTokenState,
    BalancerWeightedPoolSolver,
    MultiTokenArbitrageResult,
    TradeSignature,
    balancer_pool_to_state,
    compute_optimal_trade,
    compute_profit_token_units,
    generate_trade_signatures,
)
from degenbot.arbitrage.optimizers.base import (
    ArbitrageOptimizer,
    OptimizerResult,
    OptimizerType,
)
from degenbot.arbitrage.optimizers.batch_mobius import (
    BatchMobiusOptimizer,
    BatchMobiusPathInput,
    SerialMobiusSolver,
    VectorizedMobiusResult,
    VectorizedMobiusSolver,
    generate_batch_paths,
)
from degenbot.arbitrage.optimizers.bounded_product import (
    BoundedProductCFMM,
    BoundedProductOptimizer,
    v3_tick_range_to_bounded_product,
)
from degenbot.arbitrage.optimizers.chain_rule import (
    ChainRuleNewtonOptimizer,
    multi_pool_newton_solve,
)
from degenbot.arbitrage.optimizers.chain_rule import (
    compute_path_gradient as chain_rule_gradient,
)
from degenbot.arbitrage.optimizers.mobius import (
    HopState,
    MobiusCoefficients,
    MobiusOptimizer,
    MobiusV2Optimizer,
    TickRangeCrossing,
    V3TickRangeHop,
    V3TickRangeSequence,
    compute_mobius_coefficients,
    estimate_v3_final_sqrt_price,
    mobius_solve,
    piecewise_v3_swap,
    simulate_path,
)
from degenbot.arbitrage.optimizers.newton import (
    NewtonV2Optimizer,
    v2_optimal_arbitrage_newton,
    v2_profit_gradient_and_hessian,
)
from degenbot.arbitrage.optimizers.solver import (
    ArbSolver,
    BalancerMultiTokenHop,
    BalancerMultiTokenSolver,
    BalancerWeightedHop,
    BoundedProductHop,
    ConstantProductHop,
    CurveStableswapHop,
    Hop,
    HopType,
    PiecewiseMobiusSolver,
    PoolInvariant,
    SolidlyStableHop,
    SolidlyStableSolver,
    SolveInput,
    SolveResult,
    SolverMethod,
    V3TickRangeInfo,
    pool_state_to_hop,
    pool_to_hop,
    pools_to_solve_input,
)
from degenbot.arbitrage.optimizers.solver import (
    BrentSolver as BrentSolverUnified,
)
from degenbot.arbitrage.optimizers.solver import (
    MobiusSolver as MobiusSolverUnified,
)
from degenbot.arbitrage.optimizers.solver import (
    NewtonSolver as NewtonSolverUnified,
)
from degenbot.arbitrage.optimizers.v2_v3_optimizer import (
    CandidateSolution,
    V2PoolState,
    V2V3OptimizationResult,
    V2V3Optimizer,
    compute_price_bounds,
    estimate_equilibrium_price,
    filter_tick_ranges_by_price_bounds,
    optimize_v2_v3_arbitrage,
    sort_ranges_by_equilibrium_distance,
)
from degenbot.arbitrage.optimizers.v3_tick_predictor import (
    BoundedProductCFMM as V3BoundedProductCFMM,
)
from degenbot.arbitrage.optimizers.v3_tick_predictor import (
    TickCrossingPrediction,
    TickRange,
    V3PoolState,
    estimate_price_impact,
    predict_tick_crossing,
    sqrt_price_to_tick,
    tick_range_to_bounded_product,
    tick_to_sqrt_price,
)
from degenbot.arbitrage.optimizers.vectorized_batch import (
    BatchNewtonOptimizer,
    VectorizedArbitrageResult,
    VectorizedNewtonSolver,
    VectorizedPathState,
    VectorizedPoolState,
)

__all__ = [
    "ArbSolver",
    "ArbitrageOptimizer",
    "BalancerMultiTokenHop",
    "BalancerMultiTokenSolver",
    "BalancerMultiTokenState",
    "BalancerWeightedHop",
    "BalancerWeightedPoolSolver",
    "BatchMobiusOptimizer",
    "BatchMobiusPathInput",
    "BatchNewtonOptimizer",
    "BoundedProductCFMM",
    "BoundedProductHop",
    "BoundedProductOptimizer",
    "BrentSolverUnified",
    "CandidateSolution",
    "ChainRuleNewtonOptimizer",
    "ConstantProductHop",
    "CurveStableswapHop",
    "Hop",
    "HopState",
    "HopType",
    "MobiusCoefficients",
    "MobiusOptimizer",
    "MobiusSolverUnified",
    "MobiusV2Optimizer",
    "MultiTokenArbitrageResult",
    "NewtonSolverUnified",
    "NewtonV2Optimizer",
    "OptimizerResult",
    "OptimizerType",
    "PiecewiseMobiusSolver",
    "PoolInvariant",
    "SerialMobiusSolver",
    "SolidlyStableHop",
    "SolidlyStableSolver",
    "SolveInput",
    "SolveResult",
    "SolverMethod",
    "TickCrossingPrediction",
    "TickRange",
    "TickRangeCrossing",
    "TradeSignature",
    "V2PoolState",
    "V2V3OptimizationResult",
    "V2V3Optimizer",
    "V3BoundedProductCFMM",
    "V3PoolState",
    "V3TickRangeHop",
    "V3TickRangeInfo",
    "V3TickRangeSequence",
    "VectorizedArbitrageResult",
    "VectorizedMobiusResult",
    "VectorizedMobiusSolver",
    "VectorizedNewtonSolver",
    "VectorizedPathState",
    "VectorizedPoolState",
    "balancer_pool_to_state",
    "chain_rule_gradient",
    "compute_mobius_coefficients",
    "compute_optimal_trade",
    "compute_price_bounds",
    "compute_profit_token_units",
    "estimate_equilibrium_price",
    "estimate_price_impact",
    "estimate_v3_final_sqrt_price",
    "filter_tick_ranges_by_price_bounds",
    "generate_batch_paths",
    "generate_trade_signatures",
    "mobius_solve",
    "multi_pool_newton_solve",
    "optimize_v2_v3_arbitrage",
    "piecewise_v3_swap",
    "pool_state_to_hop",
    "pool_to_hop",
    "pools_to_solve_input",
    "predict_tick_crossing",
    "simulate_path",
    "sort_ranges_by_equilibrium_distance",
    "sqrt_price_to_tick",
    "tick_range_to_bounded_product",
    "tick_to_sqrt_price",
    "v2_optimal_arbitrage_newton",
    "v2_profit_gradient_and_hessian",
    "v3_tick_range_to_bounded_product",
]
