"""
DyCalculator objects for Curve StableSwap pool swap computation.

Each calculator encapsulates the formula for one SwapStyle or MetapoolStyle variant.
The pool's get_dy() method delegates to the calculator injected via
PoolStrategies.dy_calculator (non-metapool) or
PoolStrategies.metapool_dy_calculator / metapool_underlying_dy_calculator (metapool).

Calculators receive a DyCalculationInputs object carrying all pre-resolved data
(balances, rates, xp, closures for invariant solving). All I/O and cache lookups
happen in the pool's get_dy() before the calculator is called — calculators are
pure math consumers of the inputs snapshot.
"""
