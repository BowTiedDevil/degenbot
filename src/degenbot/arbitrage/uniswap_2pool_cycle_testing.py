import time
import warnings
from collections.abc import Mapping, Sequence
from fractions import Fraction
from functools import partial
from typing import TYPE_CHECKING, Any, ClassVar

import cvxpy.settings
import eth_abi.abi
import eth_abi.packed
import numpy
import web3
import web3.contract
import web3.exceptions
import web3.main
import web3.middleware
import web3.types
from cvxpy import Maximize, Parameter, Problem, Variable
from cvxpy.atoms.affine.binary_operators import multiply
from cvxpy.atoms.affine.bmat import bmat
from cvxpy.atoms.affine.sum import sum as cvxpy_sum
from cvxpy.atoms.geo_mean import geo_mean
from cvxpy.error import SolverError
from cvxpy.settings import SOLUTION_PRESENT
from eth_typing import BlockNumber, ChecksumAddress, HexStr
from scipy.optimize import OptimizeResult, minimize_scalar

from degenbot import (
    AerodromeV2Pool,
    AerodromeV2PoolState,
    AerodromeV3Pool,
    ArbitrageCalculationResult,
    Erc20Token,
    UniswapLpCycle,
    UniswapV2Pool,
    UniswapV2PoolState,
    UniswapV3Pool,
    UniswapV3PoolState,
    UniswapV4Pool,
    UniswapV4PoolState,
)
from degenbot.arbitrage.types import (
    UniswapV2PoolSwapAmounts,
    UniswapV3PoolSwapAmounts,
    UniswapV4PoolSwapAmounts,
)
from degenbot.cache import get_checksum_address
from degenbot.constants import MAX_INT256, WRAPPED_NATIVE_TOKENS
from degenbot.exceptions import (
    ArbitrageError,
    DegenbotValueError,
    EVMRevertError,
    IncompleteSwap,
    LiquidityPoolError,
    PossibleInaccurateResult,
)
from degenbot.logging import logger
from degenbot.uniswap.v3_libraries.tick_math import MAX_SQRT_RATIO, MIN_SQRT_RATIO
from degenbot.uniswap.v4_liquidity_pool import NATIVE_CURRENCY_ADDRESS

SLOW_ARB_CALC_THRESHOLD = 0.25
SLOW_LOOP_TIME = 0.05
VERBOSE_CVXPY_SOLVE = False
XATOL = 1.0

DEBUG_VERIFY_CACHED_PROBLEM = False
DEBUG_SLOW_CALCS = False


class InvalidForwardAmount(ArbitrageError): ...


class Unprofitable(ArbitrageError): ...


class NoSolverSolution(ArbitrageError):
    def __init__(self, message: str = "Solver failed to converge on a solution.") -> None:
        self.message = message
        super().__init__(message=message)

    def __reduce__(self) -> tuple[Any, ...]:
        return self.__class__, (self.message,)


def _build_convex_problem(num_pools: int) -> Problem:
    """
    Construct a DPP-compliant cvxpy problem with parameterized values for pool reserves. This
    allows the problem to be defined once at the class level, and rapidly re-solved at the instance
    level by updating the parameters for the specific pools and tokens being evaluated.

    The initial reserve, fee, and token decimal values are typical for the expected problem.

    ref: https://www.cvxpy.org/tutorial/dpp/index.html
    """

    # Indices are arbitrary but must be consistent so token position matches across reserve arrays
    pool_hi_index, pool_lo_index = 0, 1

    num_tokens = num_pools

    token0_decimals = 18
    token1_decimals = 18

    profit_token_index = 0
    forward_token_index = 1

    # Identify the largest value to use as a common divisor for each token.
    compression_factor_token0 = max(
        Fraction(1, 10**token0_decimals),
        Fraction(1, 10**token0_decimals),
    )
    compression_factor_token1 = max(
        Fraction(1, 10**token1_decimals),
        Fraction(1, 10**token1_decimals),
    )

    # Compress all pool reserves into a 0.0 - 1.0 value range
    _compressed_starting_reserves_pool_hi = (
        Fraction(1, 10**token0_decimals) / compression_factor_token0,
        Fraction(1, 10**token1_decimals) / compression_factor_token1,
    )
    _compressed_starting_reserves_pool_lo = (
        Fraction(1, 10**token0_decimals) / compression_factor_token0,
        Fraction(1, 10**token1_decimals) / compression_factor_token1,
    )

    # Set up parameters
    fee_multiplier = cvxpy.Parameter(
        shape=(num_pools, num_tokens),
        name="fee_multiplier",
        value=numpy.array(
            (
                (Fraction(3, 1000), Fraction(3, 1000)),
                (Fraction(3, 1000), Fraction(3, 1000)),
            ),
            dtype=numpy.float64,
        ),
    )
    compressed_reserves_pre_swap = cvxpy.Parameter(
        name="compressed_reserves_pre_swap",
        shape=(num_pools, num_tokens),
        value=numpy.array(
            (
                _compressed_starting_reserves_pool_hi,
                _compressed_starting_reserves_pool_lo,
            ),
            dtype=numpy.float64,
        ),
    )
    pool_hi_k_pre_swap = Parameter(
        name="pool_hi_pre_swap_k",
        value=geo_mean(compressed_reserves_pre_swap[pool_hi_index]).value,
    )
    pool_lo_k_pre_swap = Parameter(
        name="pool_lo_pre_swap_k",
        value=geo_mean(compressed_reserves_pre_swap[pool_lo_index]).value,
    )

    # Set up variables
    forward_token_amount = Variable(name="forward_token_amount", nonneg=True)
    pool_lo_profit_token_in = Variable(name="pool_lo_profit_token_in", nonneg=True)
    pool_hi_profit_token_out = Variable(name="pool_hi_profit_token_out", nonneg=True)

    # Set up problem
    pool_hi_deposits = (
        (forward_token_amount, 0) if forward_token_index == 0 else (0, forward_token_amount)
    )
    pool_lo_deposits = (
        (0, pool_lo_profit_token_in) if forward_token_index == 0 else (pool_lo_profit_token_in, 0)
    )
    deposits = bmat(
        (
            pool_hi_deposits,
            pool_lo_deposits,
        )
    )

    pool_hi_withdrawals = (
        (0, pool_hi_profit_token_out) if forward_token_index == 0 else (pool_hi_profit_token_out, 0)
    )
    pool_lo_withdrawals = (
        (forward_token_amount, 0) if forward_token_index == 0 else (0, forward_token_amount)
    )
    withdrawals = bmat(
        (
            pool_hi_withdrawals,
            pool_lo_withdrawals,
        )
    )
    swap_fees = multiply(fee_multiplier, deposits)
    compressed_reserves_post_swap = (
        compressed_reserves_pre_swap + deposits - withdrawals - swap_fees
    )

    problem = Problem(
        objective=Maximize(cvxpy_sum((withdrawals - deposits)[:, profit_token_index])),
        constraints=[
            # Pool invariant (x*y=k)
            geo_mean(compressed_reserves_post_swap[pool_hi_index]) >= pool_hi_k_pre_swap,
            geo_mean(compressed_reserves_post_swap[pool_lo_index]) >= pool_lo_k_pre_swap,
            # Withdrawals can't exceed pool reserves
            pool_hi_profit_token_out
            <= compressed_reserves_pre_swap[pool_hi_index, profit_token_index],
            forward_token_amount
            <= compressed_reserves_pre_swap[pool_lo_index, forward_token_index],
        ],
    )
    problem.solve(solver="CLARABEL")
    assert problem.is_dcp(dpp=True)  # type: ignore[call-arg]
    return problem


type Pool = UniswapV2Pool | UniswapV3Pool | UniswapV4Pool | AerodromeV2Pool | AerodromeV3Pool
type PoolState = UniswapV2PoolState | UniswapV3PoolState | UniswapV4PoolState | AerodromeV2PoolState
type SwapAmount = UniswapV2PoolSwapAmounts | UniswapV3PoolSwapAmounts | UniswapV4PoolSwapAmounts
type PoolId = bytes | HexStr


class _UniswapTwoPoolCycleTesting(UniswapLpCycle):
    convex_problem: ClassVar[Problem] = _build_convex_problem(num_pools=2)

    def _calculate(
        self,
        # TODO: add support for PoolId in overrides
        state_overrides: Mapping[ChecksumAddress | PoolId, PoolState] | None = None,
    ) -> ArbitrageCalculationResult:
        """
        Calculate the optimal arbitrage profit using the maximum input as an upper bound.
        """

        # TODO: check strategy comments for all arbs

        def _arb_profit_low_roe_v3_high_roe_v2(
            v3_pool: UniswapV3Pool,
            v2_pool: AerodromeV2Pool | UniswapV2Pool,
            forward_token: Erc20Token,
            forward_token_amount: float,
            v3_pool_state_override: UniswapV3PoolState | None = None,
            v2_pool_state_override: AerodromeV2PoolState | UniswapV2PoolState | None = None,
        ) -> float:
            """
            Transfer X token from V3 -> V2, profit is difference of WETH_out from V2 and WETH_in to
            V3
            """

            forward_token_quantity = int(forward_token_amount)  # round the input down

            calc_out_start = time.perf_counter()
            match v2_pool, v2_pool_state_override:
                case UniswapV2Pool(), UniswapV2PoolState() | None:
                    weth_out = v2_pool.calculate_tokens_out_from_tokens_in(
                        token_in=forward_token,
                        token_in_quantity=forward_token_quantity,
                        override_state=v2_pool_state_override,
                    )
                case AerodromeV2Pool(), AerodromeV2PoolState() | None:
                    weth_out = v2_pool.calculate_tokens_out_from_tokens_in(
                        token_in=forward_token,
                        token_in_quantity=forward_token_quantity,
                        override_state=v2_pool_state_override,
                    )
                case _:
                    raise TypeError
            calc_out_time = time.perf_counter() - calc_out_start

            calc_start = time.perf_counter()
            try:
                weth_in = v3_pool.calculate_tokens_in_from_tokens_out(
                    token_out=forward_token,
                    token_out_quantity=forward_token_quantity,
                    override_state=v3_pool_state_override,
                )
            except IncompleteSwap as exc:
                weth_in = exc.amount_in
            finally:
                calc_in_time = time.perf_counter() - calc_start

            profit = weth_out - weth_in
            logger.debug(
                f"V2: {forward_token_quantity} {forward_token} in, {weth_out} {self.input_token} out"  # noqa: E501
            )
            logger.debug(
                f"V3: {weth_in} {self.input_token} in, {forward_token_quantity} {forward_token} out"
            )

            if DEBUG_SLOW_CALCS and calc_out_time > SLOW_LOOP_TIME:
                logger.info(f"V2 calc_out time: {calc_out_time:.3f}s")
                logger.info(f"{v2_pool!r}")
            if DEBUG_SLOW_CALCS and calc_in_time > SLOW_LOOP_TIME:
                logger.info(f"V3 calc_in time : {calc_in_time:.3f}s")
                logger.info(f"{v3_pool!r}")

            # minimize_scalar requires the function to have a minimum value
            # for the solver to settle on an optimum input, so return the
            # negated profit
            return -float(profit)

        def _arb_profit_low_roe_v4_high_roe_v2(
            v4_pool: UniswapV4Pool,
            v2_pool: AerodromeV2Pool | UniswapV2Pool,
            forward_token: Erc20Token,
            forward_token_amount: float,
            v4_pool_state_override: UniswapV4PoolState | None = None,
            v2_pool_state_override: AerodromeV2PoolState | UniswapV2PoolState | None = None,
        ) -> float:
            """
            Calculate the expected profit for a V2 ROE > V4 ROE arbitrage that buys forward token X
            from the V4 pool and sells it to the V2 pool.
            """

            forward_token_quantity = int(forward_token_amount)  # round the input down

            calc_out_start = time.perf_counter()
            match v2_pool, v2_pool_state_override:
                case UniswapV2Pool(), UniswapV2PoolState() | None:
                    weth_out = v2_pool.calculate_tokens_out_from_tokens_in(
                        token_in=forward_token,
                        token_in_quantity=forward_token_quantity,
                        override_state=v2_pool_state_override,
                    )
                case AerodromeV2Pool(), AerodromeV2PoolState() | None:
                    weth_out = v2_pool.calculate_tokens_out_from_tokens_in(
                        token_in=forward_token,
                        token_in_quantity=forward_token_quantity,
                        override_state=v2_pool_state_override,
                    )
                case _:
                    raise TypeError
            calc_out_time = time.perf_counter() - calc_out_start

            calc_start = time.perf_counter()
            try:
                weth_in = v4_pool.calculate_tokens_in_from_tokens_out(
                    token_out=forward_token,
                    token_out_quantity=forward_token_quantity,
                    override_state=v4_pool_state_override,
                )
            except IncompleteSwap as exc:
                weth_in = exc.amount_in
            except PossibleInaccurateResult as exc:
                weth_in = exc.amount_in
            finally:
                calc_in_time = time.perf_counter() - calc_start

            profit = weth_out - weth_in
            logger.debug(
                f"V2: {forward_token_quantity} {forward_token} in, {weth_out} {self.input_token} out"  # noqa: E501
            )
            logger.debug(
                f"V4: {weth_in} {self.input_token} in, {forward_token_quantity} {forward_token} out"
            )

            if DEBUG_SLOW_CALCS and calc_out_time > SLOW_LOOP_TIME:
                logger.info(f"V2 calc_out time: {calc_out_time:.3f}s")
                logger.info(f"{v2_pool!r}")
            if DEBUG_SLOW_CALCS and calc_in_time > SLOW_LOOP_TIME:
                logger.info(f"V4 calc_in time : {calc_in_time:.3f}s")
                logger.info(f"{v4_pool!r}")

            # minimize_scalar requires the function to have a minimum value
            # for the solver to settle on an optimum input, so return the
            # negated profit
            return -float(profit)

        def _arb_profit_high_roe_v3_low_roe_v2(
            forward_token_amount: float,
            *,
            v3_pool: UniswapV3Pool,
            v2_pool: AerodromeV2Pool | UniswapV2Pool,
            v3_pool_state_override: UniswapV3PoolState | None = None,
            v2_pool_state_override: AerodromeV2PoolState | UniswapV2PoolState | None = None,
            forward_token: Erc20Token,
        ) -> float:
            """
            Transfer X token from V2 -> V3, profit is difference of WETH_out from V3 and WETH_in to
            V2
            """

            forward_token_quantity = int(forward_token_amount)  # round the input down

            calc_start = time.perf_counter()
            try:
                weth_out = v3_pool.calculate_tokens_out_from_tokens_in(
                    token_in=forward_token,
                    token_in_quantity=forward_token_quantity,
                    override_state=v3_pool_state_override,
                )
            except IncompleteSwap as exc:
                weth_out = exc.amount_out
            finally:
                calc_out_time = time.perf_counter() - calc_start

            calc_start = time.perf_counter()
            match v2_pool, v2_pool_state_override:
                case UniswapV2Pool(), UniswapV2PoolState() | None:
                    weth_in = v2_pool.calculate_tokens_in_from_tokens_out(
                        token_out=forward_token,
                        token_out_quantity=forward_token_quantity,
                        override_state=v2_pool_state_override,
                    )
                case AerodromeV2Pool(), AerodromeV2PoolState() | None:
                    weth_in = v2_pool.calculate_tokens_in_from_tokens_out(
                        token_out=forward_token,
                        token_out_quantity=forward_token_quantity,
                        override_state=v2_pool_state_override,
                    )
                case _:
                    raise TypeError
            calc_in_time = time.perf_counter() - calc_start

            profit = weth_out - weth_in
            logger.debug(
                f"V3: {forward_token_quantity} {forward_token} in, {weth_out} {self.input_token} out"  # noqa: E501
            )
            logger.debug(
                f"V2: {weth_in} {self.input_token} in, {forward_token_quantity} {forward_token} out"
            )

            if calc_out_time > SLOW_LOOP_TIME:
                logger.info(f"V3 calc_out time: {calc_out_time:.3f}s")
                logger.info(f"{v3_pool!r}")
            if calc_in_time > SLOW_LOOP_TIME:
                logger.info(f"V2 calc_in time : {calc_in_time:.3f}s")
                logger.info(f"{v2_pool!r}")

            # minimize_scalar requires the function to have a minimum value
            # for the solver to settle on an optimum input, so return the
            # negated profit
            return -float(profit)

        def _arb_profit_high_roe_v4_low_roe_v2(
            v4_pool: UniswapV4Pool,
            v2_pool: AerodromeV2Pool | UniswapV2Pool,
            forward_token: Erc20Token,
            forward_token_amount: float,
            v4_pool_state_override: UniswapV4PoolState | None = None,
            v2_pool_state_override: AerodromeV2PoolState | UniswapV2PoolState | None = None,
        ) -> float:
            """
            Calculate the expected profit for a V4 ROE > V2 ROE arbitrage that buys forward token X
            from the V2 pool and sells it to the V4 pool.
            """

            forward_token_quantity = int(forward_token_amount)  # round the input down

            calc_start = time.perf_counter()
            try:
                weth_out = v4_pool.calculate_tokens_out_from_tokens_in(
                    token_in=forward_token,
                    token_in_quantity=forward_token_quantity,
                    override_state=v4_pool_state_override,
                )
            except IncompleteSwap as exc:
                weth_out = exc.amount_out
            except PossibleInaccurateResult as exc:
                weth_out = exc.amount_out
            finally:
                calc_out_time = time.perf_counter() - calc_start

            calc_start = time.perf_counter()
            match v2_pool, v2_pool_state_override:
                case UniswapV2Pool(), UniswapV2PoolState() | None:
                    weth_in = v2_pool.calculate_tokens_in_from_tokens_out(
                        token_out=forward_token,
                        token_out_quantity=forward_token_quantity,
                        override_state=v2_pool_state_override,
                    )
                case AerodromeV2Pool(), AerodromeV2PoolState() | None:
                    weth_in = v2_pool.calculate_tokens_in_from_tokens_out(
                        token_out=forward_token,
                        token_out_quantity=forward_token_quantity,
                        override_state=v2_pool_state_override,
                    )
                case _:
                    raise TypeError

            calc_in_time = time.perf_counter() - calc_start

            profit = weth_out - weth_in
            logger.debug(
                f"V4: {forward_token_quantity} {forward_token} in, {weth_out} {self.input_token} out"  # noqa: E501
            )
            logger.debug(
                f"V2: {weth_in} {self.input_token} in, {forward_token_quantity} {forward_token} out"
            )

            if DEBUG_SLOW_CALCS and calc_out_time > SLOW_LOOP_TIME:
                logger.info(f"V4 calc_out time: {calc_out_time:.3f}s")
                logger.info(f"{v4_pool!r}")
            if DEBUG_SLOW_CALCS and calc_in_time > SLOW_LOOP_TIME:
                logger.info(f"V2 calc_in time : {calc_in_time:.3f}s")
                logger.info(f"{v2_pool!r}")

            # minimize_scalar requires the function to have a minimum value
            # for the solver to settle on an optimum input, so return the
            # negated profit
            return -float(profit)

        def _arb_profit_v3_v3(
            forward_token_amount: float,
            *,
            pool_hi: UniswapV3Pool,
            pool_lo: UniswapV3Pool,
            pool_hi_state_override: UniswapV3PoolState | None = None,
            pool_lo_state_override: UniswapV3PoolState | None = None,
            forward_token: Erc20Token,
        ) -> float:
            """
            Transfer X token from V3_low -> V3_high, profit is difference of WETH_out from V3_high
            and WETH_in to V3_low
            """

            forward_token_quantity = int(forward_token_amount)  # round the input down

            calc_start = time.perf_counter()
            try:
                weth_out = pool_hi.calculate_tokens_out_from_tokens_in(
                    token_in=forward_token,
                    token_in_quantity=forward_token_quantity,
                    override_state=pool_hi_state_override,
                )
            except IncompleteSwap as exc:
                weth_out = exc.amount_out
            finally:
                calc_out_time = time.perf_counter() - calc_start

            calc_start = time.perf_counter()
            try:
                weth_in = pool_lo.calculate_tokens_in_from_tokens_out(
                    token_out=forward_token,
                    token_out_quantity=forward_token_quantity,
                    override_state=pool_lo_state_override,
                )
            except IncompleteSwap as exc:
                weth_in = exc.amount_in
            finally:
                calc_in_time = time.perf_counter() - calc_start

            if DEBUG_SLOW_CALCS and calc_out_time > SLOW_LOOP_TIME:
                logger.info(f"V3 hi calc_out time: {calc_out_time:.3f}s")
                logger.info(f"{pool_hi!r}")
            if DEBUG_SLOW_CALCS and calc_in_time > SLOW_LOOP_TIME:
                logger.info(f"V3 lo calc_in time : {calc_in_time:.3f}s")
                logger.info(f"{pool_lo!r}")

            # minimize_scalar requires the function to have a minimum value
            # for the solver to settle on an optimum input, so return the
            # negated profit
            profit = weth_out - weth_in
            return -float(profit)

        def _v4_hi_v2_lo_calc(
            v4_pool: UniswapV4Pool,
            v2_pool: AerodromeV2Pool | UniswapV2Pool,
            forward_token: Erc20Token,
            v4_pool_state_override: UniswapV4PoolState | None = None,
            v2_pool_state_override: AerodromeV2PoolState | UniswapV2PoolState | None = None,
        ) -> ArbitrageCalculationResult:
            # STRATEGY:
            # - swap WETH_in -> X at V2
            # - transfer X from V2 -> V4
            # - swap X -> WETH_out at V4
            # - profit = WETH_out - WETH_in

            assert forward_token != NATIVE_CURRENCY_ADDRESS
            assert forward_token != WRAPPED_NATIVE_TOKENS[v4_pool.chain_id]

            start = time.perf_counter()

            amounts: list[UniswapV2PoolSwapAmounts | UniswapV4PoolSwapAmounts] = []

            try:
                v4_pool.calculate_tokens_out_from_tokens_in(
                    token_in=forward_token, token_in_quantity=MAX_INT256
                )
            except (IncompleteSwap, PossibleInaccurateResult) as exc:
                v4_pool_max_input = exc.amount_in
            except Exception:
                logger.exception("v4_hi_v2_lo_calc")
                raise

            v2_pool_max_output = (
                v2_pool.reserves_token0 - 1
                if v2_pool.token0 == forward_token
                else v2_pool.reserves_token1 - 1
            )

            assert v2_pool_max_output > 0, (
                f"{self._pool_viability}, {forward_token=}, {self._swap_vectors=}, {self.id=}"
            )
            assert v4_pool_max_input > 0

            # Bound the input to the Brent optimizer
            forward_token_bounds = (
                1.0,
                float(min(v4_pool_max_input, v2_pool_max_output)),
            )

            assert forward_token_bounds[0] <= forward_token_bounds[1], (
                f"{forward_token_bounds[0]=}, {forward_token_bounds[1]=}"
            )

            if forward_token_bounds[1] == 1.0:
                forward_token_amount = 1
            else:
                opt: OptimizeResult = minimize_scalar(
                    fun=lambda x: _arb_profit_high_roe_v4_low_roe_v2(
                        v4_pool=v4_pool,
                        v2_pool=v2_pool,
                        forward_token_amount=x,
                        v4_pool_state_override=v4_pool_state_override,
                        v2_pool_state_override=v2_pool_state_override,
                        forward_token=forward_token,
                    ),
                    method="bounded",
                    bounds=forward_token_bounds,
                    options={
                        "xatol": XATOL,
                    },
                )
                forward_token_amount = int(opt.x)

                if time.perf_counter() - start > SLOW_ARB_CALC_THRESHOLD:
                    logger.debug(
                        f"V4/V2 optimization (id={self.id}) took {time.perf_counter() - start:.2f}s with {opt.nit} iterations"  # noqa: E501
                    )

            # --------------------------------------------------------------------------------------
            # Encode the swap amounts to capture the arbitrage
            # --------------------------------------------------------------------------------------

            # Set the flag by checking the position of the token being sold
            v4_pool_zero_for_one = v4_pool.token0 == forward_token

            # Set the flag by checking the position of the token being purchased
            v2_pool_zero_for_one = v2_pool.token1 == forward_token

            try:
                weth_out = v4_pool.calculate_tokens_out_from_tokens_in(
                    token_in=forward_token,
                    token_in_quantity=forward_token_amount,
                    override_state=v4_pool_state_override,
                )
            except PossibleInaccurateResult as exc:
                weth_out = exc.amount_out

            amounts.append(
                UniswapV4PoolSwapAmounts(
                    pool=v4_pool.address,
                    amount_specified=-forward_token_amount,  # exact input swap
                    zero_for_one=v4_pool_zero_for_one,
                    sqrt_price_limit_x96=MIN_SQRT_RATIO + 1
                    if v4_pool_zero_for_one
                    else MAX_SQRT_RATIO - 1,
                )
            )

            if v4_pool.tokens == v2_pool.tokens:
                # Token position should be identical for both pools, 0->1 flags should be reversed
                assert v4_pool_zero_for_one != v2_pool_zero_for_one

            match v2_pool, v2_pool_state_override:
                case AerodromeV2Pool(), AerodromeV2PoolState() | None:
                    weth_in = v2_pool.calculate_tokens_in_from_tokens_out(
                        token_out=forward_token,
                        token_out_quantity=forward_token_amount,
                        override_state=v2_pool_state_override,
                    )
                case UniswapV2Pool(), UniswapV2PoolState() | None:
                    weth_in = v2_pool.calculate_tokens_in_from_tokens_out(
                        token_out=forward_token,
                        token_out_quantity=forward_token_amount,
                        override_state=v2_pool_state_override,
                    )
            amounts.append(
                UniswapV2PoolSwapAmounts(
                    pool=v2_pool.address,
                    amounts_in=((weth_in, 0) if v2_pool_zero_for_one else (0, weth_in)),
                    amounts_out=(
                        (0, forward_token_amount)
                        if v2_pool_zero_for_one
                        else (forward_token_amount, 0)
                    ),
                )
            )

            best_profit = weth_out - weth_in

            if forward_token_amount <= 0 or best_profit <= 0:
                raise ArbitrageError(message="No possible arbitrage")

            logger.debug(f"{best_profit=}, {weth_out=}, {weth_in=}")
            logger.debug(
                f"Profit result: cycle {forward_token_amount} {forward_token}, {best_profit} {self.input_token} profit"  # noqa: E501
            )
            logger.debug(f"{amounts=}")

            newest_state_block: BlockNumber | None = None
            for state in pool_states.values():
                if state.block is None:
                    newest_state_block = None
                    break
                newest_state_block = (
                    max(newest_state_block, state.block)
                    if newest_state_block is not None
                    else state.block
                )

            return ArbitrageCalculationResult(
                id=self.id,
                input_token=self.input_token,
                profit_token=self.input_token,
                input_amount=forward_token_amount,
                profit_amount=best_profit,
                swap_amounts=amounts,
                state_block=newest_state_block,
            )

        def _v3_v3_calc(
            v3_pool_hi: UniswapV3Pool,
            v3_pool_lo: UniswapV3Pool,
            v3_pool_hi_state_override: UniswapV3PoolState,
            v3_pool_lo_state_override: UniswapV3PoolState,
            forward_token: Erc20Token,
        ) -> ArbitrageCalculationResult:
            start = time.perf_counter()

            # Determine the amount of the forward token that can be deposited into the
            # hi pool by calculating a maximum input swap
            try:
                v3_pool_hi.calculate_tokens_out_from_tokens_in(
                    token_in=forward_token, token_in_quantity=MAX_INT256
                )
            except IncompleteSwap as exc:
                v3_pool_hi_max_input = exc.amount_in
            except Exception:
                logger.info(f"Failure for {v3_pool_hi!r}")
                logger.exception("v3_v3_calc")
                raise

            # Determine the amount of the forward token that can be withdrawn from the
            # lo pool by calculating a maximum output swap
            try:
                v3_pool_lo.calculate_tokens_in_from_tokens_out(
                    token_out=forward_token, token_out_quantity=MAX_INT256
                )
            except IncompleteSwap as exc:
                v3_pool_lo_max_output = exc.amount_out
            except Exception:
                logger.info(f"Failure for {v3_pool_lo!r}")
                logger.exception("v3_v3_calc")
                raise

            assert v3_pool_hi_max_input > 0
            assert v3_pool_lo_max_output > 0

            forward_token_bounds = (
                1.0,
                float(min(v3_pool_hi_max_input, v3_pool_lo_max_output)),
            )

            assert forward_token_bounds[0] <= forward_token_bounds[1], (
                f"{forward_token_bounds[0]=}, {forward_token_bounds[1]=}"
            )

            opt: OptimizeResult = minimize_scalar(
                fun=partial(
                    _arb_profit_v3_v3,
                    pool_hi=v3_pool_hi,
                    pool_lo=v3_pool_lo,
                    pool_hi_state_override=v3_pool_hi_state_override,
                    pool_lo_state_override=v3_pool_lo_state_override,
                    forward_token=forward_token,
                ),
                method="bounded",
                bounds=forward_token_bounds,
                options={
                    "xatol": XATOL,
                },
            )

            if time.perf_counter() - start > SLOW_ARB_CALC_THRESHOLD:
                logger.debug(
                    f"V3/V3 optimization (id={self.id}) took {time.perf_counter() - start:.2f}s with {opt.nit} iterations"  # noqa: E501
                )

            forward_token_amount = int(opt.x)
            logger.debug(f"{forward_token_amount=}")
            amounts: list[UniswapV3PoolSwapAmounts] = []

            try:
                # STRATEGY:
                # - swap WETH_in -> X at V3_lo
                # - transfer X from V3_lo -> V3_hi
                # - swap X -> WETH_out at V3_hi
                # - profit = WETH_out - WETH_in

                pool_hi_zero_for_one = v3_pool_hi.token1 == self.input_token
                weth_out = v3_pool_hi.calculate_tokens_out_from_tokens_in(
                    token_in=forward_token,
                    token_in_quantity=forward_token_amount,
                    override_state=v3_pool_hi_state_override,
                )
                amounts.append(
                    UniswapV3PoolSwapAmounts(
                        pool=v3_pool_hi.address,
                        amount_specified=forward_token_amount,  # input forward token
                        zero_for_one=pool_hi_zero_for_one,
                        sqrt_price_limit_x96=MIN_SQRT_RATIO + 1
                        if pool_hi_zero_for_one
                        else MAX_SQRT_RATIO - 1,
                    )
                )
                pool_lo_zero_for_one = v3_pool_lo.token1 == forward_token

                logger.debug(f"{pool_hi_zero_for_one=}")
                logger.debug(f"{pool_lo_zero_for_one=}")

                assert pool_hi_zero_for_one != pool_lo_zero_for_one

                weth_in = v3_pool_lo.calculate_tokens_in_from_tokens_out(
                    token_out=forward_token,
                    token_out_quantity=forward_token_amount,
                    override_state=v3_pool_lo_state_override,
                )
                amounts.append(
                    UniswapV3PoolSwapAmounts(
                        pool=v3_pool_lo.address,
                        amount_specified=-forward_token_amount,
                        zero_for_one=pool_lo_zero_for_one,
                        sqrt_price_limit_x96=MIN_SQRT_RATIO + 1
                        if pool_lo_zero_for_one
                        else MAX_SQRT_RATIO - 1,
                    )
                )

            except (EVMRevertError, LiquidityPoolError) as e:
                logger.error(f"V3/V3 calc error: {e}")
                raise ArbitrageError from e

            best_profit = weth_out - weth_in

            if forward_token_amount <= 0 or best_profit <= 0:
                raise ArbitrageError(message="No possible arbitrage")

            logger.debug(f"{best_profit=}, {weth_out=}, {weth_in=}")
            logger.debug(
                f"Profit result: cycle {forward_token_amount} {forward_token}, {best_profit} {self.input_token} profit"  # noqa: E501
            )
            logger.debug(f"{amounts=}")
            logger.debug(f"{self.swap_pools=}")

            newest_state_block: BlockNumber | None = None
            for state in pool_states.values():
                if state.block is None:
                    newest_state_block = None
                    break
                newest_state_block = (
                    max(newest_state_block, state.block)
                    if newest_state_block is not None
                    else state.block
                )

            return ArbitrageCalculationResult(
                id=self.id,
                input_token=self.input_token,
                profit_token=self.input_token,
                input_amount=forward_token_amount,
                profit_amount=best_profit,
                swap_amounts=amounts,
                state_block=newest_state_block,
            )

        def _v2_v2_calc(
            v2_pool_hi: AerodromeV2Pool | UniswapV2Pool,
            v2_pool_lo: AerodromeV2Pool | UniswapV2Pool,
            forward_token: Erc20Token,
            v2_pool_hi_state_override: UniswapV2PoolState | AerodromeV2PoolState | None = None,
            v2_pool_lo_state_override: UniswapV2PoolState | AerodromeV2PoolState | None = None,
        ) -> ArbitrageCalculationResult:
            # Reuse the pre-compiled problem
            problem = self.__class__.convex_problem

            # Indices are arbitrary but must be consistent so token position matches across
            # reserve arrays
            pool_hi_index, pool_lo_index = 0, 1

            token0_decimals = v2_pool_hi.token0.decimals
            token1_decimals = v2_pool_hi.token1.decimals

            profit_token = self.input_token
            forward_token = (
                v2_pool_hi.token1 if v2_pool_hi.token0 == profit_token else v2_pool_hi.token0
            )
            if v2_pool_hi.token0 == profit_token:
                profit_token_index = 0
                forward_token_index = 1
            else:
                profit_token_index = 1
                forward_token_index = 0
            assert forward_token_index != profit_token_index

            # Identify the largest value to use as a common divisor for each token.
            compression_factor_token0 = max(
                Fraction(v2_pool_hi.state.reserves_token0, 10**token0_decimals),
                Fraction(v2_pool_lo.state.reserves_token0, 10**token0_decimals),
            )
            compression_factor_token1 = max(
                Fraction(v2_pool_hi.state.reserves_token1, 10**token1_decimals),
                Fraction(v2_pool_lo.state.reserves_token1, 10**token1_decimals),
            )
            compression_factor_forward_token = (
                compression_factor_token0 if forward_token_index == 0 else compression_factor_token1
            )

            # Compress all pool reserves into a 0.0 - 1.0 value range
            _compressed_starting_reserves_pool_hi = (
                Fraction(v2_pool_hi.state.reserves_token0, 10**token0_decimals)
                / compression_factor_token0,
                Fraction(v2_pool_hi.state.reserves_token1, 10**token1_decimals)
                / compression_factor_token1,
            )
            _compressed_starting_reserves_pool_lo = (
                Fraction(v2_pool_lo.state.reserves_token0, 10**token0_decimals)
                / compression_factor_token0,
                Fraction(v2_pool_lo.state.reserves_token1, 10**token1_decimals)
                / compression_factor_token1,
            )

            # SET NEW PARAMETER VALUES
            fee_multiplier = problem.param_dict["fee_multiplier"]
            compressed_reserves_pre_swap = problem.param_dict["compressed_reserves_pre_swap"]
            pool_hi_k_pre_swap = problem.param_dict["pool_hi_pre_swap_k"]
            pool_lo_k_pre_swap = problem.param_dict["pool_lo_pre_swap_k"]

            if TYPE_CHECKING:
                assert isinstance(fee_multiplier, Parameter)
                assert isinstance(pool_hi_k_pre_swap, Parameter)
                assert isinstance(pool_lo_k_pre_swap, Parameter)
                assert isinstance(compressed_reserves_pre_swap, Parameter)

            fee_multiplier.save_value(
                numpy.array(
                    (
                        (v2_pool_hi.fee_token0, v2_pool_hi.fee_token1),
                        (v2_pool_lo.fee_token0, v2_pool_lo.fee_token1),
                    ),
                    dtype=numpy.float64,
                ),
            )
            compressed_reserves_pre_swap.save_value(
                numpy.array(
                    (
                        _compressed_starting_reserves_pool_hi,
                        _compressed_starting_reserves_pool_lo,
                    ),
                    dtype=numpy.float64,
                )
            )
            pool_hi_k_pre_swap.save_value(
                geo_mean(compressed_reserves_pre_swap[pool_hi_index]).value
            )
            pool_lo_k_pre_swap.save_value(
                geo_mean(compressed_reserves_pre_swap[pool_lo_index]).value
            )

            # SET UP VARIABLES
            forward_token_amount = problem.var_dict["forward_token_amount"]
            pool_lo_profit_token_in = problem.var_dict["pool_lo_profit_token_in"]
            pool_hi_profit_token_out = problem.var_dict["pool_hi_profit_token_out"]

            # SOLVE PROBLEM
            try:
                with warnings.catch_warnings():
                    warnings.simplefilter("ignore")
                    problem.solve(solver="CLARABEL")
            except SolverError as exc:
                logger.exception("Cvxpy solver error")
                raise ArbitrageError(message="Solver error") from exc

            if problem.status not in SOLUTION_PRESENT:
                raise NoSolverSolution

            if problem.value <= 0:
                raise Unprofitable

            if DEBUG_VERIFY_CACHED_PROBLEM:
                try:
                    new_problem = Problem(
                        objective=problem.objective,
                        constraints=problem.constraints,
                    )
                    with warnings.catch_warnings():
                        warnings.simplefilter("ignore")
                        new_problem.solve(solver="CLARABEL")
                except SolverError as exc:
                    raise ArbitrageError(message="Solver error") from exc
                else:
                    if new_problem.value != problem.value:
                        result_percent_difference = (
                            100
                            * abs(new_problem.value - problem.value)
                            / ((new_problem.value + problem.value) / 2)
                        )
                        logger.error(
                            f"Cached problem result ({problem.value}) within {result_percent_difference:.2f}% of fresh result ({new_problem.value})"  # noqa: E501
                        )
                        raise DegenbotValueError(message="CVXPY calculation result mismatch")

            uncompressed_forward_token_amount = min(
                int(
                    forward_token_amount.value
                    * 10**forward_token.decimals
                    * compression_factor_forward_token
                )
                + 1,
                (
                    v2_pool_lo.state.reserves_token0
                    if forward_token_index == 0
                    else v2_pool_lo.state.reserves_token1
                )
                - 1,
            )

            if VERBOSE_CVXPY_SOLVE:
                pool_hi_withdrawals = (
                    (0, pool_hi_profit_token_out)
                    if forward_token_index == 0
                    else (pool_hi_profit_token_out, 0)
                )
                pool_lo_withdrawals = (
                    (forward_token_amount, 0)
                    if forward_token_index == 0
                    else (0, forward_token_amount)
                )
                withdrawals = bmat(
                    (
                        pool_hi_withdrawals,
                        pool_lo_withdrawals,
                    )
                )
                pool_hi_deposits = (
                    (forward_token_amount, 0)
                    if forward_token_index == 0
                    else (0, forward_token_amount)
                )
                pool_lo_deposits = (
                    (0, pool_lo_profit_token_in)
                    if forward_token_index == 0
                    else (pool_lo_profit_token_in, 0)
                )
                deposits = bmat(
                    (
                        pool_hi_deposits,
                        pool_lo_deposits,
                    )
                )
                swap_fees = multiply(fee_multiplier, deposits)
                compressed_reserves_post_swap = (
                    compressed_reserves_pre_swap + deposits - withdrawals - swap_fees
                )
                logger.info("Solved")
                logger.info(
                    f"fee_multiplier                        = {[(float(fee[0]), float(fee[1])) for fee in fee_multiplier.value]}"  # noqa: E501
                )
                logger.info(
                    f"forward_token_amount                  = {uncompressed_forward_token_amount}"
                )
                logger.info(
                    f"withdrawals (pool_hi)                 = {withdrawals[pool_hi_index].value}"
                )
                logger.info(
                    f"withdrawals (pool_lo)                 = {withdrawals[pool_lo_index].value}"
                )
                logger.info(
                    f"deposits (pool_hi)                    = {deposits[pool_hi_index].value}"
                )
                logger.info(
                    f"deposits (pool_lo)                    = {deposits[pool_lo_index].value}"
                )
                logger.info(
                    f"reserves_starting (pool_hi)           = {list(compressed_reserves_pre_swap[pool_hi_index].value)}"  # noqa: E501
                )
                logger.info(
                    f"reserves_ending   (pool_hi)           = {list(compressed_reserves_post_swap[pool_hi_index].value)}"  # noqa: E501
                )
                logger.info(
                    f"reserves_starting (pool_lo)           = {list(compressed_reserves_pre_swap[pool_lo_index].value)}"  # noqa: E501
                )
                logger.info(
                    f"reserves_ending   (pool_lo)           = {list(compressed_reserves_post_swap[pool_lo_index].value)}"  # noqa: E501
                )
                logger.info(
                    f"reserves_final    (pool_hi)           = {list(compressed_reserves_post_swap[pool_hi_index].value)}"  # noqa: E501
                )
                logger.info(
                    f"reserves_final    (pool_lo)           = {list(compressed_reserves_post_swap[pool_lo_index].value)}"  # noqa: E501
                )

            if uncompressed_forward_token_amount <= 0:
                raise InvalidForwardAmount

            amounts: list[UniswapV2PoolSwapAmounts] = []
            try:
                # STRATEGY:
                # - swap WETH_in -> X at pool_lo
                # - transfer X from pool_lo -> pool_hi
                # - swap X -> WETH_out at pool_hi
                # - profit = WETH_out - WETH_in

                match v2_pool_hi, v2_pool_hi_state_override:
                    case UniswapV2Pool(), UniswapV2PoolState() | None:
                        weth_out = v2_pool_hi.calculate_tokens_out_from_tokens_in(
                            token_in=forward_token,
                            token_in_quantity=uncompressed_forward_token_amount,
                            override_state=v2_pool_hi_state_override,
                        )
                    case AerodromeV2Pool(), AerodromeV2PoolState() | None:
                        weth_out = v2_pool_hi.calculate_tokens_out_from_tokens_in(
                            token_in=forward_token,
                            token_in_quantity=uncompressed_forward_token_amount,
                            override_state=v2_pool_hi_state_override,
                        )
                    case _:
                        raise TypeError

                pool_hi_zero_for_one = v2_pool_hi.token1 == self.input_token

                amounts.append(
                    UniswapV2PoolSwapAmounts(
                        pool=v2_pool_hi.address,
                        amounts_in=(uncompressed_forward_token_amount, 0)
                        if pool_hi_zero_for_one
                        else (0, uncompressed_forward_token_amount),
                        amounts_out=(0, weth_out) if pool_hi_zero_for_one else (weth_out, 0),
                    )
                )

                pool_lo_zero_for_one = v2_pool_lo.token1 == forward_token
                assert pool_hi_zero_for_one != pool_lo_zero_for_one

                match v2_pool_lo, v2_pool_lo_state_override:
                    case UniswapV2Pool(), UniswapV2PoolState() | None:
                        weth_in = v2_pool_lo.calculate_tokens_in_from_tokens_out(
                            token_out=forward_token,
                            token_out_quantity=uncompressed_forward_token_amount,
                            override_state=v2_pool_lo_state_override,
                        )
                    case AerodromeV2Pool(), AerodromeV2PoolState() | None:
                        weth_in = v2_pool_lo.calculate_tokens_in_from_tokens_out(
                            token_out=forward_token,
                            token_out_quantity=uncompressed_forward_token_amount,
                            override_state=v2_pool_lo_state_override,
                        )
                    case _:
                        raise TypeError

                amounts.append(
                    UniswapV2PoolSwapAmounts(
                        pool=v2_pool_lo.address,
                        amounts_in=(weth_in, 0) if pool_lo_zero_for_one else (0, weth_in),
                        amounts_out=(0, uncompressed_forward_token_amount)
                        if pool_lo_zero_for_one
                        else (uncompressed_forward_token_amount, 0),
                    )
                )
            except (EVMRevertError, LiquidityPoolError) as e:
                logger.error(f"v2/v2 calc error: {e}")
                logger.error(f"{uncompressed_forward_token_amount=}")
                logger.error(f"{v2_pool_hi.state=}")
                logger.error(f"{v2_pool_lo.state=}")
                raise ArbitrageError from e

            if (best_profit := weth_out - weth_in) <= 0:
                raise Unprofitable

            newest_state_block: BlockNumber | None = None
            for state in pool_states.values():
                if state.block is None:
                    newest_state_block = None
                    break
                newest_state_block = (
                    max(newest_state_block, state.block)
                    if newest_state_block is not None
                    else state.block
                )

            return ArbitrageCalculationResult(
                id=self.id,
                input_token=self.input_token,
                profit_token=self.input_token,
                input_amount=uncompressed_forward_token_amount,
                profit_amount=best_profit,
                swap_amounts=amounts,
                state_block=newest_state_block,
            )

        def _v3_hi_v2_lo_calc(
            v3_pool: UniswapV3Pool,
            v2_pool: AerodromeV2Pool | UniswapV2Pool,
            forward_token: Erc20Token,
            v3_pool_override: UniswapV3PoolState | None = None,
            v2_pool_override: AerodromeV2PoolState | UniswapV2PoolState | None = None,
        ) -> ArbitrageCalculationResult:
            # STRATEGY:
            # - swap X -> WETH_out at V3
            # - transfer X from V2 -> V3
            # - swap WETH_in -> X at V2
            # - profit = WETH_out - WETH_in

            start = time.perf_counter()

            amounts: list[UniswapV2PoolSwapAmounts | UniswapV3PoolSwapAmounts] = []

            try:
                # Establish the upper forward token swap limit
                v3_pool.calculate_tokens_out_from_tokens_in(
                    token_in=forward_token, token_in_quantity=MAX_INT256
                )
            except IncompleteSwap as exc:
                v3_pool_max_forward_token_deposit = exc.amount_in
            except Exception:
                logger.info(f"Failure for {v3_pool!r}")
                logger.exception("v3_hi_v2_lo_calc")
                raise

            v2_pool_max_forward_token_withdrawal = (
                v2_pool.reserves_token0 - 1
                if forward_token == v2_pool.token0
                else v2_pool.reserves_token1 - 1
            )

            # Bound the input to the Brent optimizer
            forward_token_bounds = (
                1.0,
                float(
                    min(
                        v2_pool_max_forward_token_withdrawal,
                        v3_pool_max_forward_token_deposit,
                    )
                ),
            )

            assert forward_token_bounds[0] <= forward_token_bounds[1], (
                f"{forward_token_bounds[0]=}, {forward_token_bounds[1]=}"
            )

            if forward_token_bounds[1] == 1.0:
                forward_token_amount = 1
            else:
                opt: OptimizeResult = minimize_scalar(
                    fun=lambda x: _arb_profit_high_roe_v3_low_roe_v2(
                        v3_pool=v3_pool,
                        v2_pool=v2_pool,
                        forward_token=forward_token,
                        forward_token_amount=x,
                        v3_pool_state_override=v3_pool_override,
                        v2_pool_state_override=v2_pool_override,
                    ),
                    method="bounded",
                    bounds=forward_token_bounds,
                    options={
                        "xatol": XATOL,
                    },
                )
                forward_token_amount = int(opt.x)

                if time.perf_counter() - start > SLOW_ARB_CALC_THRESHOLD:
                    logger.debug(
                        f"V3/V2 optimization (id={self.id}) took {time.perf_counter() - start:.2f}s with {opt.nit} iterations"  # noqa: E501
                    )

            try:
                # STRATEGY:
                # - swap X -> WETH_out at V3
                # - transfer X from V2 -> V3
                # - swap WETH_in -> X at V2
                # - profit = WETH_out - WETH_in

                pool_hi_zero_for_one = v3_pool.token1 == self.input_token
                weth_out_v3 = v3_pool.calculate_tokens_out_from_tokens_in(
                    token_in=forward_token,
                    token_in_quantity=forward_token_amount,
                    override_state=v3_pool_override,
                )
                amounts.append(
                    UniswapV3PoolSwapAmounts(
                        pool=v3_pool.address,
                        amount_specified=forward_token_amount,
                        zero_for_one=pool_hi_zero_for_one,
                        sqrt_price_limit_x96=MIN_SQRT_RATIO + 1
                        if pool_hi_zero_for_one
                        else MAX_SQRT_RATIO - 1,
                    )
                )

                pool_lo_zero_for_one = v3_pool.token1 == forward_token

                assert pool_hi_zero_for_one != pool_lo_zero_for_one

                match v2_pool, v2_pool_override:
                    case UniswapV2Pool(), UniswapV2PoolState() | None:
                        weth_in_v2 = v2_pool.calculate_tokens_in_from_tokens_out(
                            token_out=forward_token,
                            token_out_quantity=forward_token_amount,
                            override_state=v2_pool_override,
                        )
                    case AerodromeV2Pool(), AerodromeV2PoolState() | None:
                        weth_in_v2 = v2_pool.calculate_tokens_in_from_tokens_out(
                            token_out=forward_token,
                            token_out_quantity=forward_token_amount,
                            override_state=v2_pool_override,
                        )
                    case _:
                        raise TypeError

                amounts.append(
                    UniswapV2PoolSwapAmounts(
                        pool=v2_pool.address,
                        amounts_out=(
                            (0, forward_token_amount)
                            if pool_lo_zero_for_one
                            else (forward_token_amount, 0)
                        ),
                        amounts_in=((weth_in_v2, 0) if pool_lo_zero_for_one else (0, weth_in_v2)),
                    )
                )
            except (EVMRevertError, LiquidityPoolError) as e:
                raise ArbitrageError from e

            best_profit = weth_out_v3 - weth_in_v2

            if forward_token_amount <= 0 or best_profit <= 0:
                raise ArbitrageError(message="No possible arbitrage")

            logger.debug(f"{best_profit=}, {weth_out_v3=}, {weth_in_v2=}")
            logger.debug(
                f"Profit result: cycle {forward_token_amount} {forward_token}, {best_profit} {self.input_token} profit"  # noqa: E501
            )
            logger.debug(f"{amounts=}")
            logger.debug(f"{self.swap_pools=}")

            newest_state_block: BlockNumber | None = None
            for state in pool_states.values():
                if state.block is None:
                    newest_state_block = None
                    break
                newest_state_block = (
                    max(newest_state_block, state.block)
                    if newest_state_block is not None
                    else state.block
                )

            return ArbitrageCalculationResult(
                id=self.id,
                input_token=self.input_token,
                profit_token=self.input_token,
                input_amount=forward_token_amount,
                profit_amount=best_profit,
                swap_amounts=amounts,
                state_block=newest_state_block,
            )

        def _v2_hi_v4_lo_calc(
            v2_pool: AerodromeV2Pool | UniswapV2Pool,
            v4_pool: UniswapV4Pool,
            forward_token: Erc20Token,
            v2_pool_override: AerodromeV2PoolState | UniswapV2PoolState | None = None,
            v4_pool_override: UniswapV4PoolState | None = None,
        ) -> ArbitrageCalculationResult:
            # STRATEGY:
            # - swap WETH_in -> X at V4
            # - transfer X from V4 -> V2
            # - swap X -> WETH_out at V2
            # - profit = WETH_out - WETH_in

            assert forward_token != NATIVE_CURRENCY_ADDRESS
            assert forward_token != WRAPPED_NATIVE_TOKENS[v4_pool.chain_id]

            start = time.perf_counter()

            amounts: list[UniswapV2PoolSwapAmounts | UniswapV4PoolSwapAmounts] = []

            try:
                v4_pool.calculate_tokens_in_from_tokens_out(
                    token_out=forward_token, token_out_quantity=MAX_INT256
                )
            except (IncompleteSwap, PossibleInaccurateResult) as exc:
                v4_pool_max_output = exc.amount_out
            except Exception:
                logger.exception("v2_hi_v4_lo_calc")
                raise

            # TODO: check more thoroughly for this condition.
            # this is probably valid, but ignoring a zero amount might mask actual issues
            if v4_pool_max_output == 0:
                raise ArbitrageError(message="Insufficient liquidity")

            # Bound the input to the Brent optimizer
            # NOTE: the V2 pool input does not need to be considered, an infinite amount can be
            # swapped in
            forward_token_bounds = (
                1.0,
                float(v4_pool_max_output),
            )

            assert forward_token_bounds[0] <= forward_token_bounds[1], (
                f"{forward_token_bounds[0]=}, {forward_token_bounds[1]=}"
            )

            opt: OptimizeResult = minimize_scalar(
                fun=lambda x: _arb_profit_low_roe_v4_high_roe_v2(
                    v4_pool=v4_pool,
                    v2_pool=v2_pool,
                    forward_token_amount=x,
                    v4_pool_state_override=v4_pool_override,
                    v2_pool_state_override=v2_pool_override,
                    forward_token=forward_token,
                ),
                method="bounded",
                bounds=forward_token_bounds,
                options={
                    "xatol": XATOL,
                },
            )

            if time.perf_counter() - start > SLOW_ARB_CALC_THRESHOLD:
                logger.debug(
                    f"V4/V2 optimization (id={self.id}) took {time.perf_counter() - start:.2f}s with {opt.nit} iterations"  # noqa: E501
                )

            forward_token_amount = int(opt.x)
            logger.debug(f"{forward_token_amount=}")

            amounts = []

            # --------------------------------------------------------------------------------------
            # Encode the swap amounts to capture the arbitrage
            # --------------------------------------------------------------------------------------

            # Set the flag by checking the position of the token being sold
            v2_pool_zero_for_one = v2_pool.token0 == forward_token

            # Set the flag by checking the position of the token being purchased
            v4_pool_zero_for_one = v4_pool.token1 == forward_token

            try:
                assert forward_token_amount >= 0
                weth_in = v4_pool.calculate_tokens_in_from_tokens_out(
                    token_out=forward_token,
                    token_out_quantity=forward_token_amount,
                    override_state=v4_pool_override,
                )
            except PossibleInaccurateResult as exc:
                weth_in = exc.amount_in

            amounts.append(
                UniswapV4PoolSwapAmounts(
                    pool=v4_pool.address,
                    amount_specified=forward_token_amount,  # exact output swap
                    zero_for_one=v4_pool_zero_for_one,
                    sqrt_price_limit_x96=MIN_SQRT_RATIO + 1
                    if v4_pool_zero_for_one
                    else MAX_SQRT_RATIO - 1,
                )
            )

            if v4_pool.tokens == v2_pool.tokens:
                # Token position should be identical for both pools, 0->1 flags should be reversed
                assert v4_pool_zero_for_one != v2_pool_zero_for_one

            match v2_pool, v2_pool_override:
                case AerodromeV2Pool(), AerodromeV2PoolState() | None:
                    weth_out = v2_pool.calculate_tokens_out_from_tokens_in(
                        token_in=forward_token,
                        token_in_quantity=forward_token_amount,
                        override_state=v2_pool_override,
                    )
                case UniswapV2Pool(), UniswapV2PoolState() | None:
                    weth_out = v2_pool.calculate_tokens_out_from_tokens_in(
                        token_in=forward_token,
                        token_in_quantity=forward_token_amount,
                        override_state=v2_pool_override,
                    )
                case _:
                    raise TypeError

            amounts.append(
                UniswapV2PoolSwapAmounts(
                    pool=v2_pool.address,
                    amounts_in=(
                        (forward_token_amount, 0)
                        if v2_pool_zero_for_one
                        else (0, forward_token_amount)
                    ),
                    amounts_out=(0, weth_out) if v2_pool_zero_for_one else (weth_out, 0),
                )
            )

            best_profit = weth_out - weth_in

            if forward_token_amount <= 0 or best_profit <= 0:
                raise ArbitrageError(message="No possible arbitrage")

            logger.debug(f"{best_profit=}, {weth_out=}, {weth_in=}")
            logger.debug(
                f"Profit result: cycle {forward_token_amount} {forward_token}, {best_profit:.4f} {self.input_token} profit"  # noqa: E501
            )
            logger.debug(f"{amounts=}")

            newest_state_block: BlockNumber | None = None
            for state in pool_states.values():
                if state.block is None:
                    newest_state_block = None
                    break
                newest_state_block = (
                    max(newest_state_block, state.block)
                    if newest_state_block is not None
                    else state.block
                )

            return ArbitrageCalculationResult(
                id=self.id,
                input_token=self.input_token,
                profit_token=self.input_token,
                input_amount=forward_token_amount,
                profit_amount=best_profit,
                swap_amounts=amounts,
                state_block=newest_state_block,
            )

        def _v2_hi_v3_lo_calc(
            v2_pool: AerodromeV2Pool | UniswapV2Pool,
            v3_pool: UniswapV3Pool,
            forward_token: Erc20Token,
            v2_pool_override: AerodromeV2PoolState | UniswapV2PoolState | None = None,
            v3_pool_override: UniswapV3PoolState | None = None,
        ) -> ArbitrageCalculationResult:
            # STRATEGY:
            # - swap WETH_in -> X at V3
            # - transfer X from V2 -> V3
            # - swap X -> WETH_out at V2
            # - profit = WETH_out - WETH_in

            start = time.perf_counter()

            amounts: list[UniswapV2PoolSwapAmounts | UniswapV3PoolSwapAmounts] = []

            try:
                v3_pool.calculate_tokens_in_from_tokens_out(
                    token_out=forward_token, token_out_quantity=MAX_INT256
                )
            except IncompleteSwap as exc:
                v3_pool_max_output = exc.amount_out
            except Exception:
                logger.info(f"Failure for {v3_pool!r}")
                logger.exception("v2_hi_v3_lo_calc")
                raise

            # TODO: check more thoroughly for this condition.
            # this is probably valid, but ignoring a zero amount might mask actual issues
            if v3_pool_max_output == 0:
                raise ArbitrageError(message="Insufficient liquidity")

            # Bound the input to the Brent optimizer
            # NOTE: the V2 pool input does not need to be considered, an infinite amount can be
            # swapped in
            forward_token_bounds = (
                1.0,
                float(v3_pool_max_output),
            )

            assert forward_token_bounds[0] <= forward_token_bounds[1], (
                f"{forward_token_bounds[0]=}, {forward_token_bounds[1]=}"
            )

            opt: OptimizeResult = minimize_scalar(
                fun=lambda x: _arb_profit_low_roe_v3_high_roe_v2(
                    v3_pool=v3_pool,
                    v2_pool=v2_pool,
                    forward_token=forward_token,
                    forward_token_amount=x,
                    v3_pool_state_override=v3_pool_override,
                    v2_pool_state_override=v2_pool_override,
                ),
                method="bounded",
                bounds=forward_token_bounds,
                options={
                    "xatol": XATOL,
                },
            )

            if time.perf_counter() - start > SLOW_ARB_CALC_THRESHOLD:
                logger.debug(
                    f"V3/V2 optimization (id={self.id}) took {time.perf_counter() - start:.2f}s with {opt.nit} iterations"  # noqa: E501
                )

            forward_token_amount = int(opt.x)
            logger.debug(f"{forward_token_amount=}")

            amounts = []

            try:
                # Transfer X token from V3 -> V2, profit is difference of WETH_out from V2 and
                # WETH_in to V3
                pool_hi_zero_for_one = v3_pool.token1 == forward_token
                weth_in_v3 = v3_pool.calculate_tokens_in_from_tokens_out(
                    token_out=forward_token,
                    token_out_quantity=forward_token_amount,
                    override_state=v3_pool_override,
                )
                amounts.append(
                    UniswapV3PoolSwapAmounts(
                        pool=v3_pool.address,
                        amount_specified=-forward_token_amount,
                        zero_for_one=pool_hi_zero_for_one,
                        sqrt_price_limit_x96=MIN_SQRT_RATIO + 1
                        if pool_hi_zero_for_one
                        else MAX_SQRT_RATIO - 1,
                    )
                )
                pool_lo_zero_for_one = v3_pool.token1 == self.input_token

                assert pool_hi_zero_for_one != pool_lo_zero_for_one

                match v2_pool, v2_pool_override:
                    case UniswapV2Pool(), UniswapV2PoolState() | None:
                        weth_out_v2 = v2_pool.calculate_tokens_out_from_tokens_in(
                            token_in=forward_token,
                            token_in_quantity=forward_token_amount,
                            override_state=v2_pool_override,
                        )
                    case AerodromeV2Pool(), AerodromeV2PoolState() | None:
                        weth_out_v2 = v2_pool.calculate_tokens_out_from_tokens_in(
                            token_in=forward_token,
                            token_in_quantity=forward_token_amount,
                            override_state=v2_pool_override,
                        )
                    case _:
                        raise TypeError

                amounts.append(
                    UniswapV2PoolSwapAmounts(
                        pool=v2_pool.address,
                        amounts_out=(
                            (0, weth_out_v2) if pool_lo_zero_for_one else (weth_out_v2, 0)
                        ),
                        amounts_in=(
                            (forward_token_amount, 0)
                            if pool_lo_zero_for_one
                            else (0, forward_token_amount)
                        ),
                    )
                )
            except (EVMRevertError, LiquidityPoolError) as e:
                raise ArbitrageError from e

            best_profit = weth_out_v2 - weth_in_v3

            if forward_token_amount <= 0 or best_profit <= 0:
                raise ArbitrageError(message="No possible arbitrage")

            logger.debug(f"{best_profit=}, {weth_out_v2=}, {weth_in_v3=}")
            logger.debug(
                f"Profit result: cycle {forward_token_amount} {forward_token}, {best_profit:.4f} {self.input_token} profit"  # noqa: E501
            )
            logger.debug(f"{amounts=}")
            logger.debug(f"{self.swap_pools=}")

            newest_state_block: BlockNumber | None = None
            for state in pool_states.values():
                if state.block is None:
                    newest_state_block = None
                    break
                newest_state_block = (
                    max(newest_state_block, state.block)
                    if newest_state_block is not None
                    else state.block
                )

            return ArbitrageCalculationResult(
                id=self.id,
                input_token=self.input_token,
                profit_token=self.input_token,
                input_amount=forward_token_amount,
                profit_amount=best_profit,
                swap_amounts=amounts,
                state_block=newest_state_block,
            )

        pool_states: Mapping[ChecksumAddress | PoolId, PoolState]
        pool_states = {pool.address: pool.state for pool in self.swap_pools}
        if state_overrides is not None:
            pool_states |= state_overrides

        match self.swap_pools:
            case (
                UniswapV3Pool() as v3_pool_a,
                UniswapV3Pool() as v3_pool_b,
            ):
                v3_pool_a_state = pool_states.get(v3_pool_a.address)
                v3_pool_b_state = pool_states.get(v3_pool_b.address)

                if TYPE_CHECKING:
                    assert isinstance(v3_pool_a_state, UniswapV3PoolState)
                    assert isinstance(v3_pool_b_state, UniswapV3PoolState)

                rate_of_exchange_a = v3_pool_a.get_absolute_exchange_rate(
                    token=self.input_token,
                    override_state=v3_pool_a_state,
                )
                rate_of_exchange_b = v3_pool_b.get_absolute_exchange_rate(
                    token=self.input_token,
                    override_state=v3_pool_b_state,
                )

                if rate_of_exchange_a > rate_of_exchange_b:
                    pool_hi = v3_pool_a
                    pool_hi_state = v3_pool_a_state
                    pool_lo = v3_pool_b
                    pool_lo_state = v3_pool_b_state
                else:
                    pool_hi = v3_pool_b
                    pool_hi_state = v3_pool_b_state
                    pool_lo = v3_pool_a
                    pool_lo_state = v3_pool_a_state

                forward_token = (
                    v3_pool_a.token1 if self.input_token == v3_pool_a.token0 else v3_pool_a.token0
                )

                return _v3_v3_calc(
                    v3_pool_hi=pool_hi,
                    v3_pool_lo=pool_lo,
                    v3_pool_hi_state_override=pool_hi_state,
                    v3_pool_lo_state_override=pool_lo_state,
                    forward_token=forward_token,
                )

            case (
                UniswapV4Pool() as v4_pool,
                (AerodromeV2Pool() | UniswapV2Pool()) as v2_pool,
            ) | (
                (AerodromeV2Pool() | UniswapV2Pool()) as v2_pool,
                UniswapV4Pool() as v4_pool,
            ):
                assert self.input_token in (
                    NATIVE_CURRENCY_ADDRESS,
                    WRAPPED_NATIVE_TOKENS[v2_pool.chain_id],
                )

                v2_pool_state = pool_states.get(v2_pool.address)
                v4_pool_state = pool_states.get(v4_pool.address)

                if TYPE_CHECKING:
                    assert isinstance(
                        v2_pool_state, AerodromeV2PoolState | UniswapV2PoolState | None
                    )
                    assert isinstance(v4_pool_state, UniswapV4PoolState | None)

                wrapped_currency_address = WRAPPED_NATIVE_TOKENS[v2_pool.chain_id]

                if self.input_token == NATIVE_CURRENCY_ADDRESS:
                    v2_input_token = (
                        v2_pool.token0
                        if v2_pool.token0 == wrapped_currency_address
                        else v2_pool.token1
                    )
                    v4_input_token = self.input_token
                    forward_token = (
                        v4_pool.token1 if v4_input_token is v4_pool.token0 else v4_pool.token0
                    )
                elif self.input_token == wrapped_currency_address:
                    v4_input_token = (
                        v4_pool.token0
                        if v4_pool.token0 in (wrapped_currency_address, NATIVE_CURRENCY_ADDRESS)
                        else v4_pool.token1
                    )
                    v2_input_token = self.input_token
                    forward_token = (
                        v2_pool.token1 if v2_input_token is v2_pool.token0 else v2_pool.token0
                    )
                else:
                    raise DegenbotValueError(message="Cannot identify input and forward tokens")

                match v2_pool, v2_pool_state:
                    case AerodromeV2Pool(), AerodromeV2PoolState() | None:
                        rate_of_exchange_v2 = v2_pool.get_absolute_exchange_rate(
                            token=v2_input_token,
                            override_state=v2_pool_state,
                        )
                    case UniswapV2Pool(), UniswapV2PoolState() | None:
                        rate_of_exchange_v2 = v2_pool.get_absolute_exchange_rate(
                            token=v2_input_token,
                            override_state=v2_pool_state,
                        )
                    case _:
                        raise TypeError

                rate_of_exchange_v4 = v4_pool.get_absolute_exchange_rate(
                    token=v4_input_token,
                    override_state=v4_pool_state,
                )

                assert forward_token not in (wrapped_currency_address, NATIVE_CURRENCY_ADDRESS)

                # Arb helper vectors are built based on assumed swap direction, so verify that the
                # pool states are profitable in this direction
                if (
                    isinstance(self.swap_pools[-1], UniswapV4Pool)
                    and rate_of_exchange_v4 > rate_of_exchange_v2
                ):
                    return _v4_hi_v2_lo_calc(
                        v4_pool=v4_pool,
                        v2_pool=v2_pool,
                        v4_pool_state_override=v4_pool_state,
                        v2_pool_state_override=v2_pool_state,
                        forward_token=forward_token,
                    )

                if (
                    isinstance(self.swap_pools[-1], AerodromeV2Pool | UniswapV2Pool)
                    and rate_of_exchange_v2 > rate_of_exchange_v4
                ):
                    return _v2_hi_v4_lo_calc(
                        v2_pool=v2_pool,
                        v4_pool=v4_pool,
                        v2_pool_override=v2_pool_state,
                        v4_pool_override=v4_pool_state,
                        forward_token=forward_token,
                    )

                raise ArbitrageError(message="No arbitrage possible.")

            case (
                (UniswapV2Pool() | AerodromeV2Pool()) as v2_pool,
                UniswapV3Pool() as v3_pool,
            ) | (
                UniswapV3Pool() as v3_pool,
                (UniswapV2Pool() | AerodromeV2Pool()) as v2_pool,
            ):
                v2_pool_state = pool_states.get(v2_pool.address)
                v3_pool_state = pool_states.get(v3_pool.address)

                if TYPE_CHECKING:
                    assert isinstance(
                        v2_pool_state, AerodromeV2PoolState | UniswapV2PoolState | None
                    )
                    assert isinstance(v3_pool_state, UniswapV3PoolState | None)

                match v2_pool, v2_pool_state:
                    case UniswapV2Pool(), UniswapV2PoolState() | None:
                        rate_of_exchange_v2 = v2_pool.get_absolute_exchange_rate(
                            token=self.input_token,
                            override_state=v2_pool_state,
                        )
                    case AerodromeV2Pool(), AerodromeV2PoolState() | None:
                        rate_of_exchange_v2 = v2_pool.get_absolute_exchange_rate(
                            token=self.input_token,
                            override_state=v2_pool_state,
                        )
                    case _:
                        raise TypeError

                rate_of_exchange_v3 = v3_pool.get_absolute_exchange_rate(
                    token=self.input_token,
                    override_state=v3_pool_state,
                )

                forward_token = (
                    v3_pool.token1 if self.input_token == v3_pool.token0 else v3_pool.token0
                )

                if rate_of_exchange_v3 > rate_of_exchange_v2:
                    return _v3_hi_v2_lo_calc(
                        v3_pool=v3_pool,
                        v2_pool=v2_pool,
                        v3_pool_override=v3_pool_state,
                        v2_pool_override=v2_pool_state,
                        forward_token=forward_token,
                    )
                return _v2_hi_v3_lo_calc(
                    v2_pool=v2_pool,
                    v3_pool=v3_pool,
                    v2_pool_override=v2_pool_state,
                    v3_pool_override=v3_pool_state,
                    forward_token=forward_token,
                )

            case (
                (UniswapV2Pool() | AerodromeV2Pool()) as v2_pool_a,
                (UniswapV2Pool() | AerodromeV2Pool()) as v2_pool_b,
            ):
                v2_pool_a_state = pool_states.get(v2_pool_a.address)
                v2_pool_b_state = pool_states.get(v2_pool_b.address)

                if TYPE_CHECKING:
                    assert isinstance(v2_pool_a_state, UniswapV2PoolState | AerodromeV2PoolState)
                    assert isinstance(v2_pool_b_state, UniswapV2PoolState | AerodromeV2PoolState)

                match v2_pool_a, v2_pool_a_state:
                    case UniswapV2Pool(), UniswapV2PoolState() | None:
                        rate_of_exchange_a = v2_pool_a.get_absolute_exchange_rate(
                            token=self.input_token,
                            override_state=v2_pool_a_state,
                        )
                    case AerodromeV2Pool(), AerodromeV2PoolState() | None:
                        rate_of_exchange_a = v2_pool_a.get_absolute_exchange_rate(
                            token=self.input_token,
                            override_state=v2_pool_a_state,
                        )
                    case _:
                        raise TypeError

                match v2_pool_b, v2_pool_b_state:
                    case UniswapV2Pool(), UniswapV2PoolState() | None:
                        rate_of_exchange_b = v2_pool_b.get_absolute_exchange_rate(
                            token=self.input_token,
                            override_state=v2_pool_b_state,
                        )
                    case AerodromeV2Pool(), AerodromeV2PoolState() | None:
                        rate_of_exchange_b = v2_pool_b.get_absolute_exchange_rate(
                            token=self.input_token,
                            override_state=v2_pool_b_state,
                        )
                    case _:
                        raise TypeError

                if rate_of_exchange_a > rate_of_exchange_b:
                    v2_pool_hi = v2_pool_a
                    v2_pool_hi_state = v2_pool_a_state
                    v2_pool_lo = v2_pool_b
                    v2_pool_lo_state = v2_pool_b_state
                else:
                    v2_pool_hi = v2_pool_b
                    v2_pool_hi_state = v2_pool_b_state
                    v2_pool_lo = v2_pool_a
                    v2_pool_lo_state = v2_pool_a_state

                forward_token = (
                    v2_pool_a.token1 if self.input_token == v2_pool_a.token0 else v2_pool_a.token0
                )

                return _v2_v2_calc(
                    v2_pool_hi=v2_pool_hi,
                    v2_pool_lo=v2_pool_lo,
                    v2_pool_hi_state_override=v2_pool_hi_state,
                    v2_pool_lo_state_override=v2_pool_lo_state,
                    forward_token=forward_token,
                )

            case _:
                err_msg = f"Cannot identify pools {self.swap_pools}"
                raise TypeError(err_msg)

    def generate_payloads(  # type: ignore[override]
        self,
        from_address: ChecksumAddress | str,
        forward_token_amount: int,
        pool_swap_amounts: Sequence[
            UniswapV2PoolSwapAmounts | UniswapV3PoolSwapAmounts | UniswapV4PoolSwapAmounts
        ],
    ) -> list[tuple[ChecksumAddress, bytes, bool]]:
        logger.debug(f"Generating payloads for {forward_token_amount} forward token amount")
        from_address = get_checksum_address(from_address)

        assert len(self.swap_pools) == 2  # noqa: PLR2004

        """
        PAYLOAD DEFINITION FROM CONTRACT

        struct Payload:
            target: address
            calldata: Bytes[MAX_PAYLOAD_BYTES]
            will_callback: bool
        """

        def _generate_v4_v2_payloads() -> Sequence[tuple[Any, ...]]:
            # TODO: rewrite for delivery by tstore executor

            # Identify the V4 pool, which always initializes the swap
            if isinstance(self.swap_pools[0], UniswapV4Pool):
                v4_pool, v2_pool = self.swap_pools
            elif isinstance(self.swap_pools[0], UniswapV2Pool | AerodromeV2Pool):
                v2_pool, v4_pool = self.swap_pools
            else:
                err_msg = f"Could not identify pool types {self.swap_pools}"
                raise TypeError(err_msg)

            v2_swap_amounts = next(
                amount
                for amount in pool_swap_amounts
                if isinstance(amount, UniswapV2PoolSwapAmounts)
            )
            v4_swap_amounts = next(
                amount
                for amount in pool_swap_amounts
                if isinstance(amount, UniswapV4PoolSwapAmounts)
            )

            assert isinstance(v2_pool, AerodromeV2Pool | UniswapV2Pool)
            assert isinstance(v4_pool, UniswapV4Pool)

            wrapped_token_address = WRAPPED_NATIVE_TOKENS[v2_pool.chain_id]

            v2_pool_rate = v2_pool.get_absolute_exchange_rate(
                v2_pool.token0 if v2_pool.token0 == wrapped_token_address else v2_pool.token1
            )

            v4_pool_rate = v4_pool.get_absolute_exchange_rate(
                v4_pool.token0
                if v4_pool.token0 in (wrapped_token_address, NATIVE_CURRENCY_ADDRESS)
                else v4_pool.token1
            )

            """
            PAYLOAD DEFINITION FROM CONTRACT

            struct V4Payload:
                currency0: address
                currency1: address
                fee: uint24
                tick_spacing: int24
                hooks: address
                amount_specified: int256
                zero_for_one: bool

            struct V2Payload:
                pool_address: address
                zero_for_one: bool
                amount_in: uint256
                amount_out: uint256
            """

            if v4_pool_rate > v2_pool_rate:
                assert v4_swap_amounts.amount_specified < 0  # exact input swap at V4
            else:
                assert v4_swap_amounts.amount_specified > 0  # exact output swap at V4

            return [
                # V4 payload
                (
                    v4_pool.token0.address,
                    v4_pool.token1.address,
                    v4_pool.fee,
                    v4_pool.tick_spacing,
                    v4_pool.hook_address,
                    v4_swap_amounts.amount_specified,
                    v4_swap_amounts.zero_for_one,
                ),
                # V2 payload
                (
                    v2_pool.address,
                    (
                        # if amounts_out for token0 is 0, swap is 0->1
                        v2_swap_amounts.amounts_out[0] == 0
                    ),
                    max(v2_swap_amounts.amounts_in),  # deposited amount
                    max(v2_swap_amounts.amounts_out),  # withdrawn amount
                ),
            ]

        def _generate_v3_v3_payloads() -> Sequence[tuple[ChecksumAddress, bytes, bool]]:
            pool_hi_swap_amount, pool_lo_swap_amount = pool_swap_amounts

            if TYPE_CHECKING:
                assert isinstance(pool_hi_swap_amount, UniswapV3PoolSwapAmounts)
                assert isinstance(pool_lo_swap_amount, UniswapV3PoolSwapAmounts)

            return [
                (
                    # PAYLOAD 0: Initial swap at the high ROE pool, WETH out to contract
                    pool_hi_swap_amount.pool,
                    web3.Web3.keccak(text="swap(address,bool,int256,uint160,bytes)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "address",  # recipient
                            "bool",  # zero_for_one
                            "int256",  # amount_specified
                            "uint160",  # sqrt_price
                            "bytes",  # data
                        ),
                        args=(
                            from_address,
                            pool_hi_swap_amount.zero_for_one,
                            pool_hi_swap_amount.amount_specified,
                            pool_hi_swap_amount.sqrt_price_limit_x96,
                            b"",
                        ),
                    ),
                    True,  # V3 swaps always execute a callback
                ),
                (
                    # PAYLOAD 1: Call for a swap at the low ROE pool, with forward token amount
                    # to the initial pool
                    pool_lo_swap_amount.pool,
                    web3.Web3.keccak(text="swap(address,bool,int256,uint160,bytes)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "address",  # recipient
                            "bool",  # zero_for_one
                            "int256",  # amount_specified
                            "uint160",  # sqrt_price
                            "bytes",  # data
                        ),
                        args=(
                            pool_hi_swap_amount.pool,
                            pool_lo_swap_amount.zero_for_one,
                            pool_lo_swap_amount.amount_specified,
                            pool_lo_swap_amount.sqrt_price_limit_x96,
                            b"",
                        ),
                    ),
                    True,  # V3 swaps always execute a callback
                ),
            ]

        def _generate_v3_v2_payloads() -> Sequence[tuple[ChecksumAddress, bytes, bool]]:
            # Identify the V3 pool, which always initializes the swap
            if isinstance(self.swap_pools[0], UniswapV3Pool):
                v3_pool, v2_pool = self.swap_pools
            elif isinstance(self.swap_pools[0], UniswapV2Pool | AerodromeV2Pool):
                v2_pool, v3_pool = self.swap_pools
            else:
                err_msg = f"Could not identify pool types {self.swap_pools}"
                raise TypeError(err_msg)

            # TODO: generalize the ordering of swap amounts
            v3_swap_amounts, v2_swap_amounts = pool_swap_amounts
            if TYPE_CHECKING:
                assert isinstance(v2_swap_amounts, UniswapV2PoolSwapAmounts)
                assert isinstance(v3_swap_amounts, UniswapV3PoolSwapAmounts)

            v2_pool_rate = v2_pool.get_absolute_exchange_rate(self.input_token)
            v3_pool_rate = v3_pool.get_absolute_exchange_rate(self.input_token)

            if v3_pool_rate > v2_pool_rate:
                return [
                    (
                        # PAYLOAD 0: Call for a swap at the V3 pool, transferring cycled token
                        # to self
                        v3_pool.address,
                        web3.Web3.keccak(text="swap(address,bool,int256,uint160,bytes)")[:4]
                        + eth_abi.abi.encode(
                            types=(
                                "address",  # recipient
                                "bool",  # zero_for_one
                                "int256",  # amount_specified
                                "uint160",  # sqrt_price
                                "bytes",  # data
                            ),
                            args=(
                                from_address,
                                v3_swap_amounts.zero_for_one,
                                v3_swap_amounts.amount_specified,
                                v3_swap_amounts.sqrt_price_limit_x96,
                                b"",
                            ),
                        ),
                        True,  # V3 swaps always execute a callback
                    ),
                    (
                        # PAYLOAD 1: Transfer `cycle_token` to V2 pool
                        self.input_token.address,
                        web3.Web3.keccak(text="transfer(address,uint256)")[:4]
                        + eth_abi.abi.encode(
                            types=(
                                "address",
                                "uint256",
                            ),
                            args=(
                                v2_pool.address,
                                max(v2_swap_amounts.amounts_in),
                            ),
                        ),
                        True,
                    ),
                    (
                        # PAYLOAD 2: Call for a swap at the V2 pool, forward token out,
                        # V3 pool as destination
                        v2_pool.address,
                        web3.Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
                        + eth_abi.abi.encode(
                            types=(
                                "uint256",
                                "uint256",
                                "address",
                                "bytes",
                            ),
                            args=(
                                *v2_swap_amounts.amounts_out,
                                v3_pool.address,
                                b"",
                            ),
                        ),
                        False,
                    ),
                ]
            return [
                (
                    # PAYLOAD 0: V3 swap, forward token out, V2 recipient
                    v3_pool.address,
                    web3.Web3.keccak(text="swap(address,bool,int256,uint160,bytes)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "address",  # recipient
                            "bool",  # zero_for_one
                            "int256",  # amount_specified
                            "uint160",  # sqrt_price
                            "bytes",  # data
                        ),
                        args=(
                            v2_pool.address,
                            v3_swap_amounts.zero_for_one,
                            v3_swap_amounts.amount_specified,
                            v3_swap_amounts.sqrt_price_limit_x96,
                            b"",
                        ),
                    ),
                    True,  # V3 swaps always execute a callback
                ),
                (
                    # PAYLOAD 1: Call for a swap at the V2 pool, transferring cycled token
                    # to self. V3 repayment handled by executor in callback.
                    v2_pool.address,
                    web3.Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "uint256",
                            "uint256",
                            "address",
                            "bytes",
                        ),
                        args=(
                            *v2_swap_amounts.amounts_out,
                            from_address,
                            b"",
                        ),
                    ),
                    False,
                ),
            ]

        def _generate_v2_v2_payloads() -> Sequence[tuple[ChecksumAddress, bytes, bool]]:
            pool_hi_swap_amount, pool_lo_swap_amount = pool_swap_amounts
            if TYPE_CHECKING:
                assert isinstance(pool_hi_swap_amount, UniswapV2PoolSwapAmounts)
                assert isinstance(pool_lo_swap_amount, UniswapV2PoolSwapAmounts)

            if pool_hi_swap_amount.pool == self.swap_pools[0]:
                pool_hi = self.swap_pools[0]
            if pool_hi_swap_amount.pool == self.swap_pools[1]:
                pool_hi = self.swap_pools[1]

            if pool_lo_swap_amount.pool == self.swap_pools[0]:
                pool_lo = self.swap_pools[0]
            if pool_lo_swap_amount.pool == self.swap_pools[1]:
                pool_lo = self.swap_pools[1]

            if TYPE_CHECKING:
                assert isinstance(pool_hi, UniswapV2Pool)
                assert isinstance(pool_lo, UniswapV2Pool)

            return [
                (
                    # PAYLOAD 0: Initial swap at the high ROE pool, WETH transfer to self with
                    # callback
                    pool_hi.address,
                    web3.Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "uint256",
                            "uint256",
                            "address",
                            "bytes",
                        ),
                        args=(
                            *pool_hi_swap_amount.amounts_out,
                            from_address,
                            b"x",  # <--- trigger callback
                        ),
                    ),
                    True,
                ),
                (
                    # PAYLOAD 1: Transfer WETH to low ROE pool
                    self.input_token.address,
                    web3.Web3.keccak(text="transfer(address,uint256)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "address",
                            "uint256",
                        ),
                        args=(
                            pool_lo.address,
                            max(pool_lo_swap_amount.amounts_in),
                        ),
                    ),
                    False,
                ),
                (
                    # PAYLOAD 2: Swap at low ROE pool, sending forward token to high ROE pool
                    pool_lo.address,
                    web3.Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "uint256",
                            "uint256",
                            "address",
                            "bytes",
                        ),
                        args=(
                            *pool_lo_swap_amount.amounts_out,
                            pool_hi.address,
                            b"",
                        ),
                    ),
                    False,
                ),
            ]

        match self.swap_pools:
            case UniswapV3Pool(), UniswapV3Pool():
                return _generate_v3_v3_payloads()
            case (
                UniswapV2Pool() | AerodromeV2Pool(),
                UniswapV3Pool(),
            ) | (
                UniswapV3Pool(),
                UniswapV2Pool() | AerodromeV2Pool(),
            ):
                return _generate_v3_v2_payloads()
            case (
                AerodromeV2Pool() | UniswapV2Pool(),
                AerodromeV2Pool() | UniswapV2Pool(),
            ):
                return _generate_v2_v2_payloads()
            case (
                UniswapV2Pool() | AerodromeV2Pool(),
                UniswapV4Pool(),
            ) | (
                UniswapV4Pool(),
                UniswapV2Pool() | AerodromeV2Pool(),
            ):
                return _generate_v4_v2_payloads()
            case _:
                err_msg = f"Could not identify pool types {self.swap_pools}"
                raise TypeError(err_msg)
