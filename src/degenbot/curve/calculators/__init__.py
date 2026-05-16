"""DyCalculator objects for Curve StableSwap pool swap computation.

Each calculator encapsulates the formula for one SwapStyle or MetapoolStyle variant.
The pool's get_dy() method delegates to the calculator injected via
PoolStrategies.dy_calculator (non-metapool) or
PoolStrategies.metapool_dy_calculator / metapool_underlying_dy_calculator (metapool).

Calculators resolve data from the pool (amp, balances, rates) in the
first few lines, then call pure invariant-solver functions from
calculations/stableswap.py for the math. The pool parameter is
read-only — calculators never mutate pool state.
"""
