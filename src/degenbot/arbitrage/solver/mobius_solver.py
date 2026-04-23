import math
from collections.abc import Sequence
from dataclasses import dataclass
from fractions import Fraction
from typing import Any

from degenbot.arbitrage.optimizers.solver import (
    ArbSolver,
    BoundedProductHop,
    ConstantProductHop,
    SolveInput,
    V3TickRangeInfo,
)
from degenbot.arbitrage.solver.protocol import SolverProtocol
from degenbot.arbitrage.solver.types import (
    ConcentratedLiquidityHopState,
    HopState,
    MobiusHopState,
    MobiusSolveResult,
    SolverMethod,
)
from degenbot.exceptions import OptimizationError

try:
    from degenbot.degenbot_rs import mobius as _rs_mobius
except ImportError:
    _rs_mobius = None

# Constants for path constraints
MIN_HOPS_FOR_ARBITRAGE: int = 2
MAX_SEARCH_RADIUS: int = 5
SMALL_HOP_THRESHOLD: int = 2
Q96_CONSTANT: int = 2**96  # Uniswap V3 Q96 sqrt price scaling factor


@dataclass(frozen=True, slots=True)
class _MobiusCoefficients:
    k: float
    m: float
    n: float

    @property
    def is_profitable(self) -> bool:
        return self.k > self.m

    def optimal_input(self) -> float:
        if not self.is_profitable:
            return 0.0
        return (math.sqrt(self.k * self.m) - self.m) / self.n

    def path_output(self, x: float) -> float:
        denom = self.m + self.n * x
        if denom <= 0:
            return 0.0
        return self.k * x / denom

    def profit_at(self, x: float) -> float:
        return self.path_output(x) - x


def _hop_to_float_state(hop: MobiusHopState) -> tuple[float, float, float]:
    return float(hop.reserve_in), float(hop.reserve_out), hop.gamma


def _compute_mobius_coefficients(
    hops: Sequence[MobiusHopState],
) -> _MobiusCoefficients:
    if not hops:
        return _MobiusCoefficients(k=0.0, m=1.0, n=0.0)

    r0, s0, g0 = _hop_to_float_state(hops[0])
    k = g0 * s0
    m = r0
    n = g0

    for hop in hops[1:]:
        r_i, s_i, g_i = _hop_to_float_state(hop)
        old_k = k
        k = old_k * g_i * s_i
        m *= r_i
        n = n * r_i + old_k * g_i

    return _MobiusCoefficients(k=k, m=m, n=n)


def _simulate_path(x: float, hops: Sequence[MobiusHopState]) -> float:
    amount = x
    for hop in hops:
        if amount <= 0:
            return 0.0
        r_i, s_i, g_i = _hop_to_float_state(hop)
        denom = r_i + amount * g_i
        if denom <= 0:
            return 0.0
        amount = amount * g_i * s_i / denom
    return amount


def _integer_refinement(
    x_opt: float,
    hops: Sequence[MobiusHopState],
    max_input: int | None,
) -> tuple[int, int]:
    num_hops = len(hops)
    search_radius = 1 if num_hops <= SMALL_HOP_THRESHOLD else min(num_hops, MAX_SEARCH_RADIUS)

    x_floor = int(x_opt)
    best_input = x_floor
    best_profit = -1

    for candidate in range(
        max(1, x_floor - search_radius),
        x_floor + search_radius + 2,
    ):
        if max_input is not None and candidate > max_input:
            continue
        output = _simulate_path(float(candidate), hops)
        profit = int(output) - candidate
        if profit > best_profit:
            best_profit = profit
            best_input = candidate

    return best_input, best_profit


_SENTINEL = object()


class MobiusSolver(SolverProtocol):
    """
    Generalized Mobius solver for constant-product and bounded-product CFMM paths.

    Handles:
    - Pure Mobius (closed-form): V2 and single-range V3/V4 paths
    - Piecewise Mobius: multi-range V3/V4 with tick crossings

    Delegates to the Rust optimizer when available for EVM-exact results.
    """

    def __init__(self) -> None:
        self._rust_solver: Any = None
        self._pool_cache: Any = None
        self._piecewise_solver: ArbSolver | object = _SENTINEL
        if _rs_mobius is not None:
            self._rust_solver = _rs_mobius.RustArbSolver()
            self._pool_cache = _rs_mobius.RustPoolCache()

    def supports(self, hops: Sequence[HopState]) -> bool:  # noqa: PLR6301
        return len(hops) >= MIN_HOPS_FOR_ARBITRAGE

    def solve(
        self,
        hops: Sequence[HopState],
        max_input: int | None = None,
    ) -> MobiusSolveResult:
        if not self.supports(hops):
            raise OptimizationError(
                "Unsupported hop types",
                iterations=0,
                method=SolverMethod.MOBIUS.value,
            )

        has_multi_range = any(
            isinstance(h, ConcentratedLiquidityHopState) and h.has_multi_range for h in hops
        )

        if has_multi_range:
            return self._solve_piecewise(hops, max_input)

        return self._solve_mobius(hops, max_input)

    def _solve_mobius(
        self,
        hops: Sequence[HopState],
        max_input: int | None = None,
    ) -> MobiusSolveResult:
        if self._rust_solver is not None:
            try:
                return self._try_rust_solve(hops, max_input)
            except OptimizationError:
                pass

        return MobiusSolver._solve_mobius_python(hops, max_input)

    @staticmethod
    def _solve_mobius_python(
        hops: Sequence[HopState],
        max_input: int | None = None,
    ) -> MobiusSolveResult:
        mobius_hops = [hop for hop in hops if isinstance(hop, MobiusHopState)]

        coeffs = _compute_mobius_coefficients(mobius_hops)
        if not coeffs.is_profitable:
            raise OptimizationError(
                "k/m <= 1",
                iterations=0,
                method=SolverMethod.MOBIUS.value,
            )

        x_opt = coeffs.optimal_input()
        if x_opt <= 0:
            raise OptimizationError(
                "Optimal input <= 0",
                iterations=0,
                method=SolverMethod.MOBIUS.value,
            )

        if max_input is not None and x_opt > float(max_input):
            x_opt = float(max_input)

        best_input, best_profit = _integer_refinement(x_opt, mobius_hops, max_input)

        if best_profit <= 0:
            raise OptimizationError(
                "Not profitable (integer verification failed)",
                iterations=0,
                method=SolverMethod.MOBIUS.value,
            )

        return MobiusSolveResult(
            optimal_input=best_input,
            profit=best_profit,
            method=SolverMethod.MOBIUS,
        )

    def _try_rust_solve(
        self,
        hops: Sequence[HopState],
        max_input: int | None = None,
    ) -> MobiusSolveResult:

        max_input_float = float(max_input) if max_input is not None else None

        all_simple = all(isinstance(h, MobiusHopState) for h in hops)
        if not all_simple:
            raise OptimizationError(
                "Rust solver requires all MobiusHopState hops",
                iterations=0,
                method=SolverMethod.MOBIUS.value,
            )

        return self._try_rust_solve_raw(hops, max_input_float)

    def _try_rust_solve_raw(
        self,
        hops: Sequence[HopState],
        max_input_float: float | None,
    ) -> MobiusSolveResult:
        if self._rust_solver is None:
            raise OptimizationError(
                "Rust solver not available",
                iterations=0,
                method=SolverMethod.MOBIUS.value,
            )

        int_hops_flat: list[int] = []
        for hop in hops:
            assert isinstance(hop, MobiusHopState)
            fee_denom = hop.fee.denominator
            gamma_numer = fee_denom - hop.fee.numerator
            int_hops_flat.extend([hop.reserve_in, hop.reserve_out, gamma_numer, fee_denom])

        try:
            result = self._rust_solver.solve_raw(int_hops_flat, max_input_float)
        except (ValueError, TypeError) as e:
            raise OptimizationError(
                f"Rust solve failed: {e}",
                iterations=0,
                method=SolverMethod.MOBIUS.value,
            ) from e

        if not result.supported:
            raise OptimizationError(
                "Not supported by Rust solver",
                iterations=0,
                method=SolverMethod.MOBIUS.value,
            )

        if not result.success:
            raise OptimizationError(
                "Not profitable (Rust)",
                iterations=result.iterations if hasattr(result, "iterations") else 0,
                method=SolverMethod.MOBIUS.value,
            )

        if result.optimal_input_int is not None and result.profit_int is not None:
            optimal_input = int(result.optimal_input_int)
            profit = int(result.profit_int)
            if profit > 0:
                return MobiusSolveResult(
                    optimal_input=optimal_input,
                    profit=profit,
                    method=SolverMethod.MOBIUS,
                )
            raise OptimizationError(
                "Not profitable (integer verification failed)",
                iterations=result.iterations if hasattr(result, "iterations") else 0,
                method=SolverMethod.MOBIUS.value,
            )

        raise OptimizationError(
            "Rust solver returned no integer results",
            iterations=result.iterations if hasattr(result, "iterations") else 0,
            method=SolverMethod.MOBIUS.value,
        )

    def _solve_piecewise(
        self,
        hops: Sequence[HopState],
        max_input: int | None = None,
    ) -> MobiusSolveResult:
        if self._rust_solver is not None:
            try:
                return self._try_rust_piecewise(hops, max_input)
            except OptimizationError:
                pass

        return self._solve_piecewise_python(hops, max_input)

    def _try_rust_piecewise(
        self,
        hops: Sequence[HopState],
        max_input: int | None = None,
    ) -> MobiusSolveResult:
        if self._rust_solver is None:
            raise OptimizationError(
                "Rust solver not available",
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS.value,
            )

        max_input_float = float(max_input) if max_input is not None else None
        rust_hops: list[Any] = []
        v3_sequences: list[tuple[int, Any]] = []

        for i, hop in enumerate(hops):
            if isinstance(hop, ConcentratedLiquidityHopState):
                if hop.has_multi_range:
                    seq = MobiusSolver._build_rust_v3_sequence(hop)
                    if seq is None:
                        raise OptimizationError(
                            "Cannot build V3 sequence",
                            iterations=0,
                            method=SolverMethod.PIECEWISE_MOBIUS.value,
                        )
                    v3_sequences.append((i, seq))
                    rust_hops.append((
                        float(hop.reserve_in),
                        float(hop.reserve_out),
                        float(hop.fee),
                    ))
                else:
                    fee_denom = hop.fee.denominator
                    gamma_numer = fee_denom - hop.fee.numerator
                    rust_hops.append(
                        _rs_mobius.RustIntHopState(
                            hop.reserve_in, hop.reserve_out, gamma_numer, fee_denom
                        )
                    )
            elif isinstance(hop, MobiusHopState):
                fee_denom = hop.fee.denominator
                gamma_numer = fee_denom - hop.fee.numerator
                rust_hops.append(
                    _rs_mobius.RustIntHopState(hop.reserve_in, hop.reserve_out, gamma_numer, fee_denom)
                )
            else:
                raise OptimizationError(
                    f"Unsupported hop type: {type(hop).__name__}",
                    iterations=0,
                    method=SolverMethod.PIECEWISE_MOBIUS.value,
                )

        result = self._rust_solver.solve(
            rust_hops,
            v3_sequences or None,
            max_input_float,
            10,
        )

        if not result.supported:
            raise OptimizationError(
                "Not supported by Rust solver",
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS.value,
            )

        if not result.success:
            raise OptimizationError(
                "Not profitable (Rust piecewise)",
                iterations=result.iterations if hasattr(result, "iterations") else 0,
                method=SolverMethod.PIECEWISE_MOBIUS.value,
            )

        if result.optimal_input_int is not None and result.profit_int is not None:
            optimal_input = int(result.optimal_input_int)
            profit = int(result.profit_int)
            if profit > 0:
                return MobiusSolveResult(
                    optimal_input=optimal_input,
                    profit=profit,
                    method=SolverMethod.PIECEWISE_MOBIUS,
                    iterations=result.iterations,
                )
        else:
            optimal_input = int(result.optimal_input)
            profit = int(result.profit)
            if profit > 0:
                return MobiusSolveResult(
                    optimal_input=optimal_input,
                    profit=profit,
                    method=SolverMethod.PIECEWISE_MOBIUS,
                    iterations=result.iterations,
                )

        raise OptimizationError(
            "Not profitable",
            iterations=result.iterations if hasattr(result, "iterations") else 0,
            method=SolverMethod.PIECEWISE_MOBIUS.value,
        )

    @staticmethod
    def _build_rust_v3_sequence(
        v3_hop: ConcentratedLiquidityHopState,
    ) -> Any:
        assert v3_hop.tick_ranges is not None
        q96 = Q96_CONSTANT
        zero_for_one = v3_hop.reserve_in > v3_hop.reserve_out

        try:
            rust_ranges = []
            for i, range_info in enumerate(v3_hop.tick_ranges):
                if i == v3_hop.current_range_index:
                    sqrt_p_current = float(v3_hop.sqrt_price) / q96
                elif i < v3_hop.current_range_index:
                    sqrt_p_current = float(range_info.sqrt_price_upper) / q96
                else:
                    sqrt_p_current = float(range_info.sqrt_price_lower) / q96

                rust_ranges.append(
                    _rs_mobius.RustV3TickRangeHop(
                        liquidity=float(range_info.liquidity),
                        sqrt_price_current=sqrt_p_current,
                        sqrt_price_lower=float(range_info.sqrt_price_lower) / q96,
                        sqrt_price_upper=float(range_info.sqrt_price_upper) / q96,
                        fee=float(v3_hop.fee),
                        zero_for_one=zero_for_one,
                    )
                )

            return _rs_mobius.RustV3TickRangeSequence(rust_ranges)
        except (ValueError, TypeError, AttributeError):
            return None

    def _get_piecewise_solver(self) -> ArbSolver:
        if self._piecewise_solver is _SENTINEL:
            self._piecewise_solver = ArbSolver()
        return self._piecewise_solver  # type: ignore[return-value]

    def _solve_piecewise_python(
        self,
        hops: Sequence[HopState],
        max_input: int | None = None,
    ) -> MobiusSolveResult:

        old_hops: list[Any] = []
        for hop in hops:
            if isinstance(hop, ConcentratedLiquidityHopState) and hop.has_multi_range:
                old_ranges = None
                if hop.tick_ranges is not None:
                    old_ranges = tuple(
                        V3TickRangeInfo(
                            tick_lower=tr.tick_lower,
                            tick_upper=tr.tick_upper,
                            liquidity=tr.liquidity,
                            sqrt_price_lower=tr.sqrt_price_lower,
                            sqrt_price_upper=tr.sqrt_price_upper,
                        )
                        for tr in hop.tick_ranges
                    )
                old_hops.append(
                    BoundedProductHop(
                        reserve_in=hop.reserve_in,
                        reserve_out=hop.reserve_out,
                        fee=hop.fee,
                        liquidity=hop.liquidity,
                        sqrt_price=hop.sqrt_price,
                        tick_lower=hop.tick_lower,
                        tick_upper=hop.tick_upper,
                        tick_ranges=old_ranges,
                        current_range_index=hop.current_range_index,
                    )
                )
            elif isinstance(hop, MobiusHopState):
                old_hops.append(
                    ConstantProductHop(
                        reserve_in=hop.reserve_in,
                        reserve_out=hop.reserve_out,
                        fee=hop.fee,
                    )
                )
            else:
                raise OptimizationError(
                    f"Unsupported hop type: {type(hop).__name__}",
                    iterations=0,
                    method=SolverMethod.PIECEWISE_MOBIUS.value,
                )

        old_result = self._get_piecewise_solver().solve(
            SolveInput(hops=tuple(old_hops), max_input=max_input)
        )

        return MobiusSolveResult(
            optimal_input=old_result.optimal_input,
            profit=old_result.profit,
            method=SolverMethod.PIECEWISE_MOBIUS,
            iterations=old_result.iterations,
        )

    def register_pool(
        self,
        reserve_in: int,
        reserve_out: int,
        fee: Fraction,
        *,
        pool_id: int | None = None,
    ) -> int:
        cache = self.get_pool_cache()
        if pool_id is None:
            pool_id = id(cache) + 1
        fee_denom = fee.denominator
        gamma_numer = fee_denom - fee.numerator
        cache.insert(pool_id, reserve_in, reserve_out, gamma_numer, fee_denom)
        return pool_id

    def update_pool(
        self,
        pool_id: int,
        reserve_in: int,
        reserve_out: int,
        fee: Fraction,
    ) -> None:
        cache = self.get_pool_cache()
        fee_denom = fee.denominator
        gamma_numer = fee_denom - fee.numerator
        cache.insert(pool_id, reserve_in, reserve_out, gamma_numer, fee_denom)

    def remove_pool(self, pool_id: int) -> bool:
        cache = self.get_pool_cache()
        return bool(cache.remove(pool_id))

    def solve_cached(
        self,
        path: list[int],
        *,
        max_input: int | None = None,
    ) -> MobiusSolveResult:
        cache = self.get_pool_cache()
        max_input_float = float(max_input) if max_input is not None else None

        try:
            result = cache.solve(path, max_input_float)
        except (ValueError, TypeError) as e:
            raise OptimizationError(
                f"Pool cache solve failed: {e}",
                iterations=0,
                method=SolverMethod.MOBIUS.value,
            ) from e

        if not result.supported:
            raise OptimizationError(
                "Not supported by cache",
                iterations=0,
                method=SolverMethod.MOBIUS.value,
            )

        if not result.success:
            raise OptimizationError(
                "Not profitable",
                iterations=result.iterations if hasattr(result, "iterations") else 0,
                method=SolverMethod.MOBIUS.value,
            )

        if result.optimal_input_int is not None and result.profit_int is not None:
            optimal_input = int(result.optimal_input_int)
            profit = int(result.profit_int)
            if profit > 0:
                return MobiusSolveResult(
                    optimal_input=optimal_input,
                    profit=profit,
                    method=SolverMethod.MOBIUS,
                )

        raise OptimizationError(
            "Not profitable",
            iterations=result.iterations if hasattr(result, "iterations") else 0,
            method=SolverMethod.MOBIUS.value,
        )

    def get_pool_cache(self):
        if self._pool_cache is None:
            msg = "Pool cache requires the Rust extension (degenbot_rs)"
            raise RuntimeError(msg)
        return self._pool_cache
