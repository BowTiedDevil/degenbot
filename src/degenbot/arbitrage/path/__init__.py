"""Swap-vector types for cross-pool arbitrage.

ACDWOC retire: ``ArbitragePath`` and ``build_swap_amount`` are gone — the
Rust ``UniswapArbEngine`` is the production solve surface (register pools in
``Bot`` → ``register_and_solve_path`` → ``latest_results()``); the Python
f64 path wrapper + the swap-amount planner that walked pools producing
typed ``UniswapV2PoolSwapAmounts`` payloads were deleted alongside. Callers
who want pool-typed swap payloads consume the engine's raw outputs
(``optimal_input`` / ``hop_outputs`` / ``consumed_inputs``) and call
``pool.build_swap_amount(zfo, amount_in, amount_out)`` directly.

The ``SwapVector`` dataclass and ``PathValidationError`` are kept — they
remain useful vocabulary for building arbitrary swap sequences outside the
deleted ``ArbitragePath`` wrapper.
"""

from degenbot.arbitrage.path.types import (
    PathValidationError,
    SwapVector,
)

__all__ = [
    "PathValidationError",
    "SwapVector",
]
