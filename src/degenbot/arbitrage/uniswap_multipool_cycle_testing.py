import warnings
from collections.abc import Mapping, Sequence
from fractions import Fraction
from typing import TYPE_CHECKING, ClassVar, Literal

import cvxpy.settings
import eth_abi.abi
import numpy as np
import web3
from cvxpy import Maximize, Parameter, Problem, Variable
from cvxpy.atoms.affine.binary_operators import multiply
from cvxpy.atoms.affine.bmat import bmat
from cvxpy.atoms.affine.hstack import hstack
from cvxpy.atoms.geo_mean import geo_mean
from cvxpy.error import SolverError
from eth_typing import ChecksumAddress

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.aerodrome.types import AerodromeV2PoolState
from degenbot.arbitrage.types import ArbitrageCalculationResult, UniswapV2PoolSwapAmounts
from degenbot.arbitrage.uniswap_lp_cycle import UniswapLpCycle
from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20.erc20 import Erc20Token
from degenbot.exceptions.arbitrage import ArbitrageError, NoSolverSolution, Unprofitable
from degenbot.exceptions.evm import EVMRevertError
from degenbot.exceptions.liquidity_pool import LiquidityPoolError
from degenbot.logging import logger
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolState

DEBUG_VERIFY_CACHED_PROBLEM = False


def _build_convex_problem(num_pools: int) -> Problem:
    """
    Construct a DPP-compliant cvxpy problem with parameterized values for pool reserves. This
    allows the problem to be defined once at the class level, and rapidly re-solved at the instance
    level by updating the parameters for the specific pools and tokens being evaluated.

    The initial reserve, fee, and token decimal values are typical for the expected problem.

    ref: https://www.cvxpy.org/tutorial/dpp/index.html
    """

    class FakePool:
        token0: "FakeToken"
        token1: "FakeToken"
        tokens: tuple["FakeToken", "FakeToken"]
        fee_token0 = Fraction(3, 1000)
        fee_token1 = Fraction(3, 1000)

    class FakeToken: ...

    num_tokens = num_pools

    # Create unique fake pools & tokens to replicate the sorting and ordering mechanism used by
    # the instance method
    ordered_pools = [FakePool() for _ in range(num_pools)]
    ordered_tokens = [FakeToken() for _ in range(num_tokens)]

    for i, pool in enumerate(ordered_pools):
        if i == num_pools - 1:
            pool.token0 = ordered_tokens[-1]
            pool.token1 = ordered_tokens[0]
        else:
            pool.token0 = ordered_tokens[i]
            pool.token1 = ordered_tokens[i + 1]

        pool.tokens = pool.token0, pool.token1

    global_pool_index = {pool: i for i, pool in enumerate(ordered_pools)}
    global_token_index = {token: i for i, token in enumerate(ordered_tokens)}

    # The first pool holds token_1, token_2
    # The second holds token_2, token_3
    # ...
    # The last pool holds token_n, token_1 where token_n is equal to the number of tokens

    uncompressed_reserves = np.zeros(
        shape=(num_pools, num_tokens),
        dtype=np.float64,
    )
    for pool_index in range(num_pools):
        if pool_index == num_pools - 1:
            uncompressed_reserves[pool_index, pool_index] = 1 * 10**18
            uncompressed_reserves[pool_index, 0] = 1 * 10**18
        else:
            uncompressed_reserves[pool_index, pool_index] = 1 * 10**18
            uncompressed_reserves[pool_index, pool_index + 1] = 1 * 10**18

    # Identify the largest value to use as a common divisor for each token.
    token_compression_factors = [
        np.max(uncompressed_reserves[:, global_token_index[token]]) for token in ordered_tokens
    ]

    # SET UP PARAMETERS
    # Compress all pool reserves into a 0.0 - 1.0 value range by dividing by the compression
    # factor (via elementwise multiplication of the inverse)
    compressed_reserves_pre_swap = Parameter(
        shape=(num_pools, num_tokens),
        name="compressed_reserves_pre_swap",
        value=np.multiply(uncompressed_reserves, np.reciprocal(token_compression_factors)),
    )
    swap_fees = Parameter(
        shape=(num_pools, num_tokens),
        name="swap_fees",
        value=np.array(
            [
                [
                    (
                        pool.fee_token0
                        if token == pool.token0
                        else pool.fee_token1
                        if token in pool.tokens
                        else 0
                    )
                    for token in ordered_tokens
                ]
                for pool in ordered_pools
            ],
            dtype=np.float64,
        ),
    )
    pool_ks_pre_swap = Parameter(
        shape=len(ordered_pools),
        name="pool_ks_pre_swap",
        value=[
            geo_mean(
                hstack(
                    [
                        compressed_reserves_pre_swap[
                            global_pool_index[pool], global_token_index[token]
                        ]
                        for token in pool.tokens
                    ]
                )
            ).value
            for pool in ordered_pools
        ],
    )

    # SET UP VARIABLES
    initial_pool_deposit = Variable(name="initial_pool_deposit", nonneg=True)
    final_pool_withdrawal = Variable(name="final_pool_withdrawal", nonneg=True)
    # TODO: make profit token amounts forward token variables?
    forward_token_amount_variables = {
        token: Variable(name=f"forward_token_{global_token_index[token]}_amount", nonneg=True)
        for token in ordered_tokens[1:]  # skip the profit token
    }

    # SET UP PROBLEM
    _deposits: list[list[Literal[0] | Variable]]
    _deposits = [[0 for _ in ordered_tokens] for _ in ordered_pools]

    _withdrawals: list[list[Literal[0] | Variable]]
    _withdrawals = [[0 for _ in ordered_tokens] for _ in ordered_pools]

    for token, token_index in global_token_index.items():
        if token_index == 0:
            # Special case for profit token, which is deposited into first pool and
            # withdrawn from last pool
            deposit_pool_index = 0
            withdrawal_pool_index = -1
            deposit_variable = initial_pool_deposit
            withdrawal_variable = final_pool_withdrawal
        else:
            # Each forward token is withdrawn from the pool at the previous position
            # e.g. token 1 withdrawn from pool 0, and deposited at the next pool
            deposit_pool_index = token_index
            withdrawal_pool_index = token_index - 1
            deposit_variable = withdrawal_variable = forward_token_amount_variables[token]

        _withdrawals[withdrawal_pool_index][token_index] = withdrawal_variable
        _deposits[deposit_pool_index][token_index] = deposit_variable

    deposits = bmat(_deposits)
    withdrawals = bmat(_withdrawals)

    compressed_reserves_post_swap = (
        compressed_reserves_pre_swap + deposits - withdrawals - multiply(swap_fees, deposits)
    )

    pool_ks_post_swap = [
        geo_mean(
            hstack(
                [
                    compressed_reserves_post_swap[
                        global_pool_index[pool], global_token_index[token]
                    ]
                    for token in pool.tokens
                ]
            )
        )
        for pool in ordered_pools
    ]

    constraints = []

    # Pool invariants (x*y=k)
    constraints.extend(
        [
            pool_ks_pre_swap[global_pool_index[pool]] <= pool_ks_post_swap[global_pool_index[pool]]
            for pool in ordered_pools
        ]
    )

    problem = Problem(
        objective=Maximize(final_pool_withdrawal - initial_pool_deposit),
        constraints=constraints,
    )
    assert problem.is_dcp(dpp=True)  # type: ignore[call-arg]
    problem.solve(solver="CLARABEL")
    return problem


type Pool = UniswapV2Pool | AerodromeV2Pool
type PoolState = UniswapV2PoolState | AerodromeV2PoolState


class _UniswapMultiPoolCycleTesting(UniswapLpCycle):
    swap_pools: tuple[Pool, ...]
    convex_problems: ClassVar[
        dict[
            int,  # number of pools
            Problem,
        ]
    ] = {
        3: _build_convex_problem(num_pools=3),
        4: _build_convex_problem(num_pools=4),
    }

    def _calculate(  # type: ignore[override]
        self,
        state_overrides: Mapping[Pool, PoolState] | None = None,
    ) -> ArbitrageCalculationResult[UniswapV2PoolSwapAmounts]:
        """
        Calculate the optimal arbitrage profit using the maximum input as an upper bound.
        """

        pool_states: Mapping[Pool, PoolState]
        pool_states = {pool: pool.state for pool in self.swap_pools}

        if state_overrides is not None:
            pool_states.update(state_overrides)

        def v2_only_calc(
            pools: Sequence[Pool],
            pool_states: Mapping[Pool, PoolState],
        ) -> ArbitrageCalculationResult[UniswapV2PoolSwapAmounts]:
            """
            Calculate the optimal arbitrage for a sequence of Uniswap V2 (or compatible) pools of
            arbitrary length.
            """

            def get_token_balance_at_pool(
                token: Erc20Token,
                pool: Pool,
                state_override: PoolState | None = None,
            ) -> Fraction:
                if token not in pool.tokens:
                    return Fraction(0)

                _state = state_override or pool.state
                return Fraction(
                    _state.reserves_token0 if token == pool.token0 else _state.reserves_token1,
                    10**token.decimals,
                )

            def order_tokens(pools: Sequence[Pool]) -> tuple[Erc20Token, ...]:
                ordered_tokens: list[Erc20Token] = [self.input_token]

                for pool in pools:
                    for token in pool.tokens:
                        if token not in ordered_tokens:
                            ordered_tokens.append(token)

                assert len(ordered_tokens) == len(pools), f"{ordered_tokens=}, {pools=}"
                return tuple(ordered_tokens)

            # Reuse the pre-compiled problem
            problem = type(self).convex_problems[len(self.swap_pools)]

            # TODO: review if pools should be dynamically sorted, or filtered pre-check and assumed
            # to be passed in order
            ordered_pools = pools
            ordered_tokens = order_tokens(pools)

            global_pool_index = {pool: i for i, pool in enumerate(ordered_pools)}
            global_token_index = {token: i for i, token in enumerate(ordered_tokens)}

            uncompressed_reserves = np.array(
                [
                    [
                        get_token_balance_at_pool(
                            token=token,
                            pool=pool,
                            state_override=pool_states.get(pool),
                        )
                        for token in ordered_tokens
                    ]
                    for pool in ordered_pools
                ],
                dtype=np.float64,
            )

            # Identify the largest value to use as a common divisor for each token.
            token_compression_factors = [
                np.max(uncompressed_reserves[:, global_token_index[token]])
                for token in ordered_tokens
            ]

            # SET UP PARAMETERS
            assert len(problem.param_dict) == 3  # noqa: PLR2004
            swap_fees = problem.param_dict["swap_fees"]
            compressed_reserves_pre_swap = problem.param_dict["compressed_reserves_pre_swap"]
            pool_ks_pre_swap = problem.param_dict["pool_ks_pre_swap"]

            if TYPE_CHECKING:
                assert isinstance(swap_fees, cvxpy.Parameter)
                assert isinstance(compressed_reserves_pre_swap, cvxpy.Parameter)
                assert isinstance(pool_ks_pre_swap, cvxpy.Parameter)

            swap_fees.save_value(
                np.array(
                    [
                        [
                            pool.fee_token0
                            if token == pool.token0
                            else pool.fee_token1
                            if token in pool.tokens
                            else 0
                            for token in ordered_tokens
                        ]
                        for pool in ordered_pools
                    ],
                    dtype=np.float64,
                ),
            )
            compressed_reserves_pre_swap.save_value(
                np.multiply(uncompressed_reserves, np.reciprocal(token_compression_factors))
            )
            pool_ks_pre_swap.save_value(
                [
                    geo_mean(
                        hstack(
                            [
                                compressed_reserves_pre_swap[
                                    global_pool_index[pool], global_token_index[token]
                                ]
                                for token in pool.tokens
                            ]
                        )
                    ).value
                    for pool in ordered_pools
                ]
            )

            try:
                with warnings.catch_warnings():
                    warnings.simplefilter("ignore")
                    problem.solve(solver=cvxpy.CLARABEL)
            except SolverError as exc:
                raise ArbitrageError(message=f"Solver error: {exc}") from None

            if problem.status not in cvxpy.settings.SOLUTION_PRESENT:
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
                        new_problem.solve(solver=cvxpy.CLARABEL)
                except SolverError:
                    raise ArbitrageError(message="Solver error") from None
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
                        err_msg = "Result mismatch"
                        raise ValueError(err_msg)

            amounts: list[UniswapV2PoolSwapAmounts] = []
            initial_pool_deposit = problem.var_dict["initial_pool_deposit"]
            initial_amount_in = int(
                initial_pool_deposit.value
                * token_compression_factors[global_token_index[self.input_token]]
                * 10**self.input_token.decimals
            )
            for i, pool in enumerate(ordered_pools):
                if i == 0:
                    amount_in = initial_amount_in
                    token_in = self.input_token

                if amount_in == 0:
                    raise ArbitrageError(message="Zero amount swap")

                zero_for_one = token_in == pool.token0
                token_out = pool.token1 if zero_for_one else pool.token0

                pool_state = pool_states[pool]

                match pool, pool_state:
                    case AerodromeV2Pool(), AerodromeV2PoolState():
                        try:
                            amount_out = pool.calculate_tokens_out_from_tokens_in(
                                token_in=token_in,
                                token_in_quantity=amount_in,
                                override_state=pool_state,
                            )
                        except (EVMRevertError, LiquidityPoolError) as e:
                            raise ArbitrageError from e

                    case UniswapV2Pool(), UniswapV2PoolState():
                        try:
                            amount_out = pool.calculate_tokens_out_from_tokens_in(
                                token_in=token_in,
                                token_in_quantity=amount_in,
                                override_state=pool_state,
                            )
                        except (EVMRevertError, LiquidityPoolError) as e:
                            raise ArbitrageError from e

                if amount_out == 0:
                    raise ArbitrageError(message="Zero amount swap")

                amounts.append(
                    UniswapV2PoolSwapAmounts(
                        pool=pool.address,
                        amounts_in=(amount_in, 0) if zero_for_one else (0, amount_in),
                        amounts_out=(0, amount_out) if zero_for_one else (amount_out, 0),
                    )
                )

                amount_in = amount_out
                token_in = token_out

            if (best_profit := amount_out - initial_amount_in) <= 0:
                raise Unprofitable

            newest_state_block = None
            if not state_overrides:
                pool_state_blocks = tuple(
                    block for pool in self.swap_pools if (block := pool.state.block) is not None
                )
                if len(pool_state_blocks) == len(self.swap_pools):
                    newest_state_block = max(pool_state_blocks)

            return ArbitrageCalculationResult(
                id=self.id,
                input_token=self.input_token,
                profit_token=self.input_token,
                input_amount=initial_amount_in,
                profit_amount=best_profit,
                swap_amounts=tuple(amounts),
                state_block=newest_state_block,
            )

        if all(
            pool for pool in self.swap_pools if isinstance(pool, (AerodromeV2Pool, UniswapV2Pool))
        ):
            return v2_only_calc(
                pools=self.swap_pools,
                pool_states=pool_states,
            )

        err = "One or more pools is not supported by this arbitrage strategy."
        raise ValueError(err)

    def generate_payloads(  # type: ignore[override]
        self,
        from_address: ChecksumAddress | str,
        pool_swap_amounts: Sequence[UniswapV2PoolSwapAmounts],
    ) -> list[tuple[ChecksumAddress, bytes, bool]]:
        from_address = get_checksum_address(from_address)

        """
        PAYLOAD DEFINITION FROM CONTRACT

        struct Payload:
            target: address
            calldata: Bytes[MAX_PAYLOAD_BYTES]
            will_callback: bool
        """

        def _generate_v2_v2_v2_payloads() -> list[tuple[ChecksumAddress, bytes, bool]]:
            return [
                (
                    # Swap at final pool, send profit token back to contract
                    pool_swap_amounts[2].pool,
                    web3.Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "uint256",
                            "uint256",
                            "address",
                            "bytes",
                        ),
                        args=(
                            *pool_swap_amounts[2].amounts_out,
                            from_address,
                            b"x",  # <--- trigger callback
                        ),
                    ),
                    True,
                ),
                (
                    # Transfer token to pay first pool
                    self.input_token.address,
                    web3.Web3.keccak(text="transfer(address,uint256)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "address",
                            "uint256",
                        ),
                        args=(
                            pool_swap_amounts[0].pool,
                            max(pool_swap_amounts[0].amounts_in),
                        ),
                    ),
                    False,
                ),
                (
                    # First pool swap
                    pool_swap_amounts[0].pool,
                    web3.Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "uint256",
                            "uint256",
                            "address",
                            "bytes",
                        ),
                        args=(
                            *pool_swap_amounts[0].amounts_out,
                            pool_swap_amounts[1].pool,
                            b"",
                        ),
                    ),
                    False,
                ),
                (
                    # Final swap
                    pool_swap_amounts[1].pool,
                    web3.Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "uint256",
                            "uint256",
                            "address",
                            "bytes",
                        ),
                        args=(
                            *pool_swap_amounts[1].amounts_out,
                            pool_swap_amounts[2].pool,
                            b"",
                        ),
                    ),
                    False,
                ),
            ]

        def _generate_v2_v2_v2_v2_payloads() -> list[tuple[ChecksumAddress, bytes, bool]]:
            return [
                (
                    # Swap at final pool, send profit token back to contract
                    pool_swap_amounts[3].pool,
                    web3.Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "uint256",
                            "uint256",
                            "address",
                            "bytes",
                        ),
                        args=(
                            *pool_swap_amounts[3].amounts_out,
                            from_address,
                            b"x",  # <--- trigger callback
                        ),
                    ),
                    True,
                ),
                (
                    # Transfer token to pay first pool
                    self.input_token.address,
                    web3.Web3.keccak(text="transfer(address,uint256)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "address",
                            "uint256",
                        ),
                        args=(
                            pool_swap_amounts[0].pool,
                            max(pool_swap_amounts[0].amounts_in),
                        ),
                    ),
                    False,
                ),
                (
                    # First pool swap
                    pool_swap_amounts[0].pool,
                    web3.Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "uint256",
                            "uint256",
                            "address",
                            "bytes",
                        ),
                        args=(
                            *pool_swap_amounts[0].amounts_out,
                            pool_swap_amounts[1].pool,
                            b"",
                        ),
                    ),
                    False,
                ),
                (
                    # Second pool swap
                    pool_swap_amounts[1].pool,
                    web3.Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "uint256",
                            "uint256",
                            "address",
                            "bytes",
                        ),
                        args=(
                            *pool_swap_amounts[1].amounts_out,
                            pool_swap_amounts[2].pool,
                            b"",
                        ),
                    ),
                    False,
                ),
                (
                    # Third pool swap
                    pool_swap_amounts[2].pool,
                    web3.Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
                    + eth_abi.abi.encode(
                        types=(
                            "uint256",
                            "uint256",
                            "address",
                            "bytes",
                        ),
                        args=(
                            *pool_swap_amounts[2].amounts_out,
                            pool_swap_amounts[3].pool,
                            b"",
                        ),
                    ),
                    False,
                ),
            ]

        match self.swap_pools:
            case (
                AerodromeV2Pool() | UniswapV2Pool(),
                AerodromeV2Pool() | UniswapV2Pool(),
                AerodromeV2Pool() | UniswapV2Pool(),
            ):
                return _generate_v2_v2_v2_payloads()
            case (
                AerodromeV2Pool() | UniswapV2Pool(),
                AerodromeV2Pool() | UniswapV2Pool(),
                AerodromeV2Pool() | UniswapV2Pool(),
                AerodromeV2Pool() | UniswapV2Pool(),
            ):
                return _generate_v2_v2_v2_v2_payloads()
            case _:
                err = f"Could not identify pool types {self.swap_pools}"
                raise ValueError(err)
