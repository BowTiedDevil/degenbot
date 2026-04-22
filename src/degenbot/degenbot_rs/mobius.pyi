"""
Type stubs for the degenbot_rs.mobius submodule.

Möbius transformation optimizer for constant-product and bounded-product
CFMM arbitrage path optimization. Provides both float (fast) and integer
(EVM-exact) solvers.
"""

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
        coeff_k: K coefficient (product of gammas and reserves)
        coeff_m: M coefficient (product of input reserves)
        coeff_n: N coefficient (cross-term coefficient)
        is_profitable: Whether K > M (profitable arbitrage exists)
    """

    @property
    def coeff_k(self) -> float: ...
    @property
    def coeff_m(self) -> float: ...
    @property
    def coeff_n(self) -> float: ...
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

class RustV3TickRangeSequence:
    """
    Sequence of adjacent V3 tick ranges for multi-range solving.

    Encapsulates multiple tick ranges and provides crossing calculations.
    """

    def __init__(self, ranges: list[RustV3TickRangeHop]) -> None: ...
    @property
    def ranges(self) -> list[RustV3TickRangeHop]: ...
    def to_hop_state(self, range_index: int | None = None) -> RustHopState:
        """Convert a tick range to an effective HopState."""
    def compute_crossing(self, end_idx: int) -> RustTickRangeCrossing:
        """Compute the crossing data for ending at a specific range."""

class RustTickRangeCrossing:
    """
    Tick range crossing data for piecewise Möbius calculation.

    Attributes:
        crossing_gross_input: Gross input required to cross ranges
        crossing_output: Output received from crossing ranges
        ending_range: The final tick range where the swap ends
    """

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

    Provides both pure Möbius and piecewise Möbius solving with
    V3 tick range support.
    """

    def __init__(self) -> None: ...
    def solve(
        self,
        hops: list[RustHopState],
        max_input: float | None = None,
    ) -> RustMobiusResult: ...
    def solve_piecewise(
        self,
        hops: list[RustHopState],
        v3_hop_index: int,
        crossings: list[RustTickRangeCrossing],
        max_input: float | None = None,
    ) -> RustMobiusResult: ...
    def solve_v3_sequence(
        self,
        hops: list[RustHopState],
        v3_hop_index: int,
        sequence: RustV3TickRangeSequence,
        max_candidates: int,
        max_input: float | None = None,
    ) -> RustMobiusResult: ...
    def solve_v3_v3(
        self,
        seq1: RustV3TickRangeSequence,
        seq2: RustV3TickRangeSequence,
        max_input: float | None = None,
        max_iterations: int = 10,
    ) -> RustMobiusResult: ...

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
        method: Integer method tag (0=MOBIUS, 1=PIECEWISE_MOBIUS)
    """

    @property
    def optimal_input(self) -> float: ...
    @property
    def profit(self) -> float: ...
    @property
    def optimal_input_int(self) -> float | None: ...
    @property
    def profit_int(self) -> float | None: ...
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

    Handles V2, V3 single-range, and V3 multi-range paths with automatic
    dispatch to the optimal algorithm.
    """

    def __init__(self) -> None: ...
    def solve(
        self,
        hops: list[RustHopState | RustIntHopState | tuple[float, float, float]],
        v3_sequences: list[tuple[int, RustV3TickRangeSequence]] | None = None,
        max_input: float | None = None,
        max_iterations: int = 10,
    ) -> RustArbResult: ...
    def solve_raw(
        self,
        hops_flat: list[int],
        max_input: float | None = None,
    ) -> RustArbResult:
        """
        Solve using flat integer array for minimal marshalling overhead.

        Args:
            hops_flat: Flat list of [r0, s0, gamma_numer0, fee_denom0, r1, s1, ...]
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

def compute_mobius_coefficients(
    hops: list[RustHopState],
) -> RustMobiusCoefficients:
    """Compute Möbius coefficients for a path."""

def mobius_solve(
    hops: list[RustHopState],
    max_input: float | None = None,
) -> RustMobiusResult:
    """Solve for optimal arbitrage input using float arithmetic."""

def simulate_path(x: float, hops: list[RustHopState]) -> float:
    """Simulate a swap through all hops."""

def estimate_v3_final_sqrt_price(
    amount_in: float,
    v3_hop: RustV3TickRangeHop,
) -> float:
    """Estimate the final sqrt price after a V3 swap."""

def int_mobius_solve(
    hops: list[RustIntHopState],
    max_input: float | None = None,
) -> RustIntMobiusResult:
    """Solve for optimal arbitrage input using integer arithmetic."""

def int_simulate_path(x: float, hops: list[RustIntHopState]) -> float:
    """Simulate a swap through all hops using integer arithmetic."""

def mobius_refine_int(
    hops: list[RustIntHopState],
    x_float: float,
) -> tuple[int, int]:
    """
    Refine a float solution to integer optimal input and profit.

    Returns:
        Tuple of (optimal_input, profit)
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
    "compute_mobius_coefficients",
    "estimate_v3_final_sqrt_price",
    "int_mobius_solve",
    "int_simulate_path",
    "mobius_refine_int",
    "mobius_solve",
    "simulate_path",
]
