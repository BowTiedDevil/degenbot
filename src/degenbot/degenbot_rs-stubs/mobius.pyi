"""
Type stubs for the degenbot_rs.mobius submodule.

Möbius transformation optimizer for constant-product and bounded-product
CFMM arbitrage path optimization. Provides both float (fast) and integer
(EVM-exact) solvers.
"""

from typing import Any

class RustHopState:
    """
    Pool hop state with reserves and fee for float-based Möbius solving.

    Attributes:
        reserve_in: Input reserve as float
        reserve_out: Output reserve as float
        fee: Fee rate as float (e.g., 0.003 for 0.3%)
    """

    def __init__(self, reserve_in: float, reserve_out: float, fee: float) -> None: ...
    @property
    def reserve_in(self) -> float: ...
    @property
    def reserve_out(self) -> float: ...
    @property
    def fee(self) -> float: ...

class RustMobiusCoefficients:
    """
    Möbius transformation coefficients for a path.

    The path output is computed as: l(x) = K * x / (M + N * x)

    Attributes:
        coeff_K: K coefficient (product of gammas and reserves)
        coeff_M: M coefficient (product of input reserves)
        coeff_N: N coefficient (cross-term coefficient)
        is_profitable: Whether K > M (profitable arbitrage exists)
    """

    @property
    def coeff_K(self) -> float: ...
    @property
    def coeff_M(self) -> float: ...
    @property
    def coeff_N(self) -> float: ...
    @property
    def is_profitable(self) -> bool: ...
    def path_output(self, x: float) -> float:
        """Compute path output for input x."""
    def optimal_input(self) -> float:
        """Compute the exact optimal input."""
    def profit_at(self, x: float) -> float:
        """Compute profit for input x."""

class RustV3TickRangeHop:
    """
    Uniswap V3 tick range state for piecewise Möbius solving.

    Attributes:
        liquidity: Liquidity in this tick range
        sqrt_price_current: Current sqrt price
        sqrt_price_lower: Lower bound sqrt price
        sqrt_price_upper: Upper bound sqrt price
        fee: Fee rate as float
        zero_for_one: True if swapping token0 for token1
    """

    def __init__(
        self,
        liquidity: float,
        sqrt_price_current: float,
        sqrt_price_lower: float,
        sqrt_price_upper: float,
        fee: float,
        zero_for_one: bool,
    ) -> None: ...
    @property
    def liquidity(self) -> float: ...
    @property
    def sqrt_price_current(self) -> float: ...
    @property
    def sqrt_price_lower(self) -> float: ...
    @property
    def sqrt_price_upper(self) -> float: ...
    @property
    def fee(self) -> float: ...
    @property
    def zero_for_one(self) -> bool: ...
    def alpha(self) -> float:
        """Lower bound on R0: L / √P_upper."""
    def beta(self) -> float:
        """Lower bound on R1: L · √P_lower."""
    def to_hop_state(self) -> RustHopState:
        """Convert to a RustHopState with effective reserves."""
    def contains_sqrt_price(self, sqrt_price: float) -> bool:
        """Check if a sqrt price is within this tick range."""
    def max_gross_input_in_range(self) -> float:
        """Maximum gross input (including fees) this range can absorb."""

class RustV3TickRangeSequence:
    """
    Sequence of adjacent V3 tick ranges for multi-range solving.

    Encapsulates multiple tick ranges and provides crossing calculations.
    """

    def __init__(self, ranges: list[RustV3TickRangeHop]) -> None: ...
    def compute_crossing(self, k: int) -> RustTickRangeCrossing:
        """Compute the crossing data for reaching range k."""

class RustTickRangeCrossing:
    """
    Tick range crossing data for piecewise Möbius calculation.

    Attributes:
        crossing_gross_input: Gross input required to cross ranges
        crossing_output: Output received from crossing ranges
        ending_range: The final tick range where the swap ends
    """

    def __init__(
        self,
        crossing_gross_input: float,
        crossing_output: float,
        ending_range: RustV3TickRangeHop,
    ) -> None: ...
    @property
    def crossing_gross_input(self) -> float: ...
    @property
    def crossing_output(self) -> float: ...
    @property
    def ending_range(self) -> RustV3TickRangeHop: ...

class RustMobiusResult:
    """
    Result from Möbius float solver.

    Attributes:
        optimal_input: Optimal input amount (float)
        profit: Expected profit (float)
        iterations: Number of iterations (0 for closed-form)
        success: Whether a profitable solution was found
    """

    @property
    def optimal_input(self) -> float: ...
    @property
    def profit(self) -> float: ...
    @property
    def iterations(self) -> int: ...
    @property
    def success(self) -> bool: ...

class RustMobiusOptimizer:
    """
    High-level Möbius optimizer for multi-hop paths.

    Every constant product swap y = (γ·s·x)/(r + γ·x) is a Möbius
    transformation. An n-hop path composes into l(x) = K·x / (M + N·x),
    with closed-form optimal input x_opt = (√(K·M) - M) / N.

    Zero iterations, exact solution, O(n) forward pass.
    """

    def __init__(self) -> None: ...
    def compute_coefficients(
        self,
        hops: list[RustHopState],
    ) -> RustMobiusCoefficients:
        """Compute Möbius coefficients K, M, N for an n-hop path."""
    def simulate_path(self, x: float, hops: list[RustHopState]) -> float:
        """Simulate a swap through all hops."""
    def solve(
        self,
        hops: list[RustHopState],
        max_input: float | None = None,
    ) -> RustMobiusResult:
        """Solve for optimal arbitrage input (closed-form, zero iterations)."""
    def solve_v3_candidates(
        self,
        base_hops: list[RustHopState],
        v3_hop_index: int,
        v3_candidates: list[RustV3TickRangeHop],
        max_input: float | None = None,
    ) -> RustMobiusResult:
        """Solve with multiple candidate V3 tick ranges."""
    def estimate_v3_final_sqrt_price(
        self,
        amount_in: float,
        v3_hop: RustV3TickRangeHop,
    ) -> float:
        """Estimate final sqrt price after a V3 swap."""
    def solve_piecewise(
        self,
        hops: list[RustHopState],
        v3_hop_index: int,
        crossings: list[RustTickRangeCrossing],
        max_input: float | None = None,
    ) -> RustMobiusResult:
        """Solve arbitrage with piecewise-Möbius for V3 tick crossings."""
    def solve_v3_sequence(
        self,
        hops: list[RustHopState],
        v3_hop_index: int,
        sequence: RustV3TickRangeSequence,
        max_candidates: int,
        max_input: float | None = None,
    ) -> RustMobiusResult:
        """Solve arbitrage with full V3 tick range sequence handling."""
    def solve_v3_v3(
        self,
        sequence1: RustV3TickRangeSequence,
        sequence2: RustV3TickRangeSequence,
        max_input: float | None = None,
        max_candidates: int = 10,
    ) -> RustMobiusResult:
        """Solve V3-V3 arbitrage (two V3 hops, both potentially crossing ticks)."""
    def solve_batch(
        self,
        hops_array: list[float],
        num_hops: int,
        max_inputs: list[float],
    ) -> dict[str, Any]:
        """
        Solve a batch of paths with the same hop count.

        Returns:
            dict with 'optimal_input', 'profit', 'is_profitable' lists.
        """
    def solve_batch_vectorized(
        self,
        reserves_in: list[float],
        reserves_out: list[float],
        fees: list[float],
        num_hops: int,
        max_inputs: list[float],
    ) -> dict[str, Any]:
        """
        Solve a batch using vectorized coefficient computation.

        Returns:
            dict with 'optimal_input', 'profit', 'is_profitable' lists.
        """

class RustArbResult:
    """
    Result from unified arbitrage solver (RustArbSolver).

    Contains both float and integer results for maximum flexibility.

    Attributes:
        optimal_input: Optimal input (float, always present)
        profit: Expected profit (float)
        optimal_input_int: Optimal input as integer (if available)
        profit_int: Profit as integer (if available)
        iterations: Number of iterations
        success: Whether optimization succeeded
        supported: Whether the path type is supported
        method: Integer method tag (0=MOBIUS, 1=PIECEWISE_MOBIUS, 2=V3V3)
    """

    @property
    def optimal_input(self) -> float: ...
    @property
    def profit(self) -> float: ...
    @property
    def optimal_input_int(self) -> int | None: ...
    @property
    def profit_int(self) -> int | None: ...
    @property
    def iterations(self) -> int: ...
    @property
    def success(self) -> bool: ...
    @property
    def supported(self) -> bool: ...
    @property
    def method(self) -> int: ...

class RustArbSolver:
    """
    Unified arbitrage solver with automatic method selection.

    Accepts mixed hop types and automatically selects the best solver.
    Returns supported=False for hop types not handled by Rust
    (Solidly, Balancer, Curve), so Python can fall back.
    """

    def __init__(self) -> None: ...
    def solve(
        self,
        hops: list[RustHopState | RustIntHopState | tuple[float, float, float]],
        v3_sequences: list[tuple[int, RustV3TickRangeSequence]] | None = None,
        max_input: float | None = None,
        max_candidates: int = 10,
    ) -> RustArbResult:
        """
        Unified solve entry point with automatic method selection.

        When all hops are RustIntHopState, does merged integer refinement
        and returns EVM-exact integer results.
        """
    def solve_raw(
        self,
        int_hops_flat: list[int],
        max_input: float | None = None,
    ) -> RustArbResult:
        """
        Solve using flat integer array for minimal marshalling overhead.

        Args:
            int_hops_flat: Flat list of [reserve_in, reserve_out, gamma_numer, fee_denom] per hop
            max_input: Optional maximum input constraint
        """

class RustPoolCache:
    """
    Cached pool state storage for fast solve-by-ID operations.

    Pool states are registered once, then solved by referencing pool IDs.
    This eliminates Python object construction overhead on the solve path.
    """

    def __init__(self) -> None: ...
    def insert(
        self,
        pool_id: int,
        reserve_in: int,
        reserve_out: int,
        gamma_numer: int,
        fee_denom: int,
    ) -> None:
        """Insert or update a pool's state in the cache."""
    def remove(self, pool_id: int) -> bool:
        """Remove a pool from the cache. Returns True if found."""
    def solve(
        self,
        path: list[int],
        max_input: float | None = None,
    ) -> RustArbResult:
        """Solve an arbitrage path using cached pool states by ID."""
    def contains(self, pool_id: int) -> bool:
        """Check if a pool ID is in the cache."""
    def __len__(self) -> int: ...
    def __bool__(self) -> bool: ...

class RustIntHopState:
    """
    Integer-based hop state for EVM-exact Möbius solving.

    Uses U256 internally for exact EVM arithmetic without float precision loss.

    Attributes:
        reserve_in: Input reserve as U256-compatible int
        reserve_out: Output reserve as U256-compatible int
        gamma_numer: Gamma numerator (fee_denom - fee_numer)
        fee_denom: Fee denominator
    """

    def __init__(
        self,
        reserve_in: int,
        reserve_out: int,
        gamma_numer: int,
        fee_denom: int,
    ) -> None: ...
    @property
    def reserve_in(self) -> int: ...
    @property
    def reserve_out(self) -> int: ...
    @property
    def gamma_numer(self) -> int: ...
    @property
    def fee_numer(self) -> int: ...
    @property
    def fee_denom(self) -> int: ...

class RustIntMobiusResult:
    """
    Result from integer Möbius solver.

    Attributes:
        optimal_input: Optimal input as integer
        profit: Expected profit as integer
        iterations: Number of iterations (0 for closed-form)
        success: Whether a profitable solution was found
    """

    @property
    def optimal_input(self) -> int: ...
    @property
    def profit(self) -> int: ...
    @property
    def iterations(self) -> int: ...
    @property
    def success(self) -> bool: ...

def py_compute_mobius_coefficients(
    hops: list[RustHopState],
) -> RustMobiusCoefficients:
    """Compute Möbius coefficients for a path."""

def py_mobius_solve(
    hops: list[RustHopState],
    max_input: float | None = None,
) -> RustMobiusResult:
    """Solve for optimal arbitrage input using float arithmetic."""

def py_simulate_path(x: float, hops: list[RustHopState]) -> float:
    """Simulate a swap through all hops."""

def py_estimate_v3_final_sqrt_price(
    amount_in: float,
    v3_hop: RustV3TickRangeHop,
) -> float:
    """Estimate the final sqrt price after a V3 swap."""

def py_int_mobius_solve(
    hops: list[RustIntHopState],
) -> RustIntMobiusResult:
    """Solve for optimal arbitrage input using integer arithmetic."""

def py_int_simulate_path(x: int, hops: list[RustIntHopState]) -> int:
    """Simulate a swap through all hops using integer arithmetic."""

def py_mobius_refine_int(
    x_approx: float,
    hops: list[RustIntHopState],
    max_input: float | None = None,
) -> RustIntMobiusResult:
    """
    Integer refinement around a float optimum using EVM-exact U256 arithmetic.

    Args:
        x_approx: Approximate optimal input from the float Möbius solver
        hops: List of integer hop states
        max_input: Optional maximum input constraint

    Returns:
        RustIntMobiusResult with optimal_input, profit, success, and iterations
    """

__all__ = [
    "RustArbResult",
    "RustArbSolver",
    "RustHopState",
    "RustIntHopState",
    "RustIntMobiusResult",
    "RustMobiusCoefficients",
    "RustMobiusOptimizer",
    "RustMobiusResult",
    "RustPoolCache",
    "RustTickRangeCrossing",
    "RustV3TickRangeHop",
    "RustV3TickRangeSequence",
    "py_compute_mobius_coefficients",
    "py_estimate_v3_final_sqrt_price",
    "py_int_mobius_solve",
    "py_int_simulate_path",
    "py_mobius_refine_int",
    "py_mobius_solve",
    "py_simulate_path",
]
