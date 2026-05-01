import asyncio
from collections.abc import Mapping, Sequence
from concurrent.futures import ProcessPoolExecutor, ThreadPoolExecutor
from fractions import Fraction
from typing import Any, cast
from weakref import WeakSet

from eth_typing import ChecksumAddress

from degenbot.arbitrage.optimizers.hop_types import SolveInput, Solver, SolveResult
from degenbot.arbitrage.path.swap_amount_builder import build_swap_amount
from degenbot.arbitrage.path.pool_hop_adapter import extract_fee as _adapter_extract_fee, to_hop_state as _adapter_to_hop_state
from degenbot.arbitrage.path.types import PathValidationError, PoolCompatibility, SwapVector
from degenbot.arbitrage.types import (
    AbstractSwapAmounts,
    ArbitrageCalculationResult,
    UniswapV2PoolSwapAmounts,
    UniswapV3PoolSwapAmounts,
    UniswapV4PoolSwapAmounts,
)
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import OptimizationError
from degenbot.exceptions.arbitrage import IncompatiblePoolInvariant
from degenbot.types.abstract import AbstractPoolState
from degenbot.types.concrete import (
    AbstractPublisherMessage,
    PoolStateMessage,
    Publisher,
    PublisherMixin,
    Subscriber,
)
from degenbot.types.hop_types import HopType
from degenbot.types.pool_protocols import ArbitrageCapablePool
from degenbot.uniswap.v3_libraries.functions import (
    v3_virtual_reserves as _v3_virtual_reserves,  # noqa: F401 re-export for tests
)

_MIN_POOLS_FOR_ARBITRAGE_PATH = 2


def _check_pool_compatibility(pool: object) -> PoolCompatibility:
    try:
        _adapter_to_hop_state(pool, zero_for_one=True)
    except (IncompatiblePoolInvariant, TypeError, AttributeError):
        return PoolCompatibility.INCOMPATIBLE_INVARIANT
    else:
        return PoolCompatibility.COMPATIBLE


def _extract_fee(pool: object, zero_for_one: bool) -> Fraction:  # noqa: FBT001
    return _adapter_extract_fee(pool, zero_for_one=zero_for_one)


def _pool_to_hop_state(
    pool: object,
    zero_for_one: bool,  # noqa: FBT001
    state_override: AbstractPoolState | None = None,
) -> HopType:
    return _adapter_to_hop_state(pool, zero_for_one=zero_for_one, state_override=state_override)


class _ProfitableStateDiscovered(AbstractPublisherMessage):
    __slots__ = ("path", "result")

    def __init__(self, result: SolveResult, path: "ArbitragePath") -> None:
        self.result = result
        self.path = path


class _StateUpdatedNoProfit(AbstractPublisherMessage):
    __slots__ = ("path",)

    def __init__(self, path: "ArbitragePath") -> None:
        self.path = path


class ArbitragePath(PublisherMixin):
    """
    Event-driven arbitrage path helper.

    Wraps a sequence of Mobius-compatible pools, validates token flow, pre-computes directional
    data, subscribes to state updates, and delegates solving to a swappable Solver
    implementation.
    """

    def __init__(
        self,
        pools: Sequence[Any],
        input_token: Erc20Token,
        solver: Solver,
        max_input: int | None = None,
        id: str | None = None,  # noqa:A002
    ) -> None:
        if len(pools) < _MIN_POOLS_FOR_ARBITRAGE_PATH:
            msg = f"Arbitrage path requires at least {_MIN_POOLS_FOR_ARBITRAGE_PATH} pools"
            raise PathValidationError(msg)

        self._pools: tuple[Any, ...] = tuple(pools)
        self._input_token = input_token
        self._solver = solver
        self._max_input = max_input
        self._id = id or ""
        self._subscribers: WeakSet[Subscriber] = WeakSet()
        self._last_result: SolveResult | None = None

        self._validate_pools()
        self._swap_vectors = self._build_swap_vectors()
        self._pool_index: dict[Any, int] = {pool: i for i, pool in enumerate(self._pools)}
        self._hop_states: list[HopType] = [
            _pool_to_hop_state(pool, self._swap_vectors[i].zero_for_one)
            for i, pool in enumerate(self._pools)
        ]

        for pool in self._pools:
            pool.subscribe(self)

    def _validate_pools(self) -> None:
        for i, pool in enumerate(self._pools):
            compat = _check_pool_compatibility(pool)
            if compat != PoolCompatibility.COMPATIBLE:
                msg = f"Pool {i} ({type(pool).__name__}) is not Mobius-compatible: {compat.value}"
                raise PathValidationError(msg)

        tokens: list[tuple[Any, Any]] = []
        current = self._input_token
        for pool in self._pools:
            if current == pool.token0:
                tokens.append((pool.token0, pool.token1))
                current = pool.token1
            elif current == pool.token1:
                tokens.append((pool.token1, pool.token0))
                current = pool.token0
            else:
                idx = self._pools.index(pool)
                msg = (
                    f"Token chain broken at pool {idx}: "
                    f"expected input {current}, pool has {pool.token0}/{pool.token1}"
                )
                raise PathValidationError(msg)

        final_output = tokens[-1][1]
        if final_output != self._input_token:
            msg = f"Path is not cyclic: starts with {self._input_token}, ends with {final_output}"
            raise PathValidationError(msg)

    def _build_swap_vectors(self) -> tuple[SwapVector, ...]:
        vectors: list[SwapVector] = []
        current = self._input_token

        for pool in self._pools:
            if current == pool.token0:
                zero_for_one = True
                token_out = pool.token1
            else:
                zero_for_one = False
                token_out = pool.token0

            vectors.append(
                SwapVector(
                    token_in=current,
                    token_out=token_out,
                    zero_for_one=zero_for_one,
                )
            )
            current = token_out

        return tuple(vectors)

    @property
    def pools(self) -> tuple[Any, ...]:
        return self._pools

    @property
    def input_token(self) -> Erc20Token:
        return self._input_token

    @property
    def swap_vectors(self) -> tuple[SwapVector, ...]:
        return self._swap_vectors

    @property
    def solver(self) -> Solver:
        return self._solver

    @property
    def last_result(self) -> SolveResult | None:
        return self._last_result

    @property
    def hop_states(self) -> tuple[HopType, ...]:
        return tuple(self._hop_states)

    @property
    def max_input(self) -> int | None:
        return self._max_input

    @max_input.setter
    def max_input(self, value: int | None) -> None:
        self._max_input = value

    @property
    def id(self) -> str:
        return self._id

    def set_solver(self, solver: Solver) -> None:
        self._solver = solver

    def close(self) -> None:
        for pool in self._pools:
            pool.unsubscribe(self)
        self._subscribers.clear()

    def calculate(self) -> SolveResult:
        self._refresh_hop_states()
        result = self._solver.solve(self._build_solve_input())
        self._last_result = result
        return result

    def calculate_with_state_override(
        self,
        state_overrides: dict[ChecksumAddress, AbstractPoolState],
    ) -> SolveResult:
        hops = self._resolve_state_overrides(state_overrides)
        return self._solver.solve(self._build_solve_input(hops=hops))

    def calculate_with_pool(
        self,
        executor: ProcessPoolExecutor | ThreadPoolExecutor,
        state_overrides: Mapping[ChecksumAddress, AbstractPoolState] | None = None,
    ) -> asyncio.Future[SolveResult]:
        """
        Execute calculation in the given executor (ProcessPool recommended for CPU-bound work).

        Unlike the legacy UniswapLpCycle.calculate_with_pool, this method serializes only
        the lightweight SolveInput (tuple of frozen HopType dataclasses) — not full pool
        objects. It therefore never fails on sparse V3 bitmaps or non-pickleable state.
        """
        self._refresh_hop_states()

        hops = tuple(self._hop_states)
        if state_overrides is not None and state_overrides:
            hops = self._resolve_state_overrides(state_overrides)

        solve_input = self._build_solve_input(hops=hops)
        return asyncio.get_running_loop().run_in_executor(
            executor,
            self._solver.solve,
            solve_input,
        )

    def _resolve_state_overrides(
        self,
        state_overrides: Mapping[ChecksumAddress, AbstractPoolState],
    ) -> tuple[HopType, ...]:
        hop_states: list[HopType] = []
        for i, pool in enumerate(self._pools):
            state = state_overrides.get(pool.address)
            if state is not None:
                hop_states.append(
                    _pool_to_hop_state(
                        pool,
                        self._swap_vectors[i].zero_for_one,
                        state_override=state,
                    )
                )
            else:
                hop_states.append(self._hop_states[i])
        return tuple(hop_states)

    def _build_solve_input(
        self,
        hops: tuple[HopType, ...] | None = None,
    ) -> SolveInput:
        return SolveInput(
            hops=hops or tuple(self._hop_states),
            max_input=self._max_input,
        )

    def build_swap_amounts(
        self,
        result: SolveResult,
        state_overrides: Mapping[ChecksumAddress, AbstractPoolState] | None = None,
    ) -> ArbitrageCalculationResult[AbstractSwapAmounts]:
        if state_overrides is None:
            state_overrides = {}

        token_in_quantity = result.optimal_input
        swap_amounts: list[AbstractSwapAmounts] = []

        for pool, sv in zip(self._pools, self._swap_vectors, strict=True):
            if token_in_quantity == 0:
                msg = "A swap would result in an output of zero"
                raise PathValidationError(msg)

            pool_state = state_overrides.get(pool.address)
            token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                token_in=sv.token_in,
                token_in_quantity=token_in_quantity,
                override_state=pool_state,
            )
            swap_amounts.append(build_swap_amount(pool, sv, token_in_quantity, token_out_quantity))
            token_in_quantity = token_out_quantity

        input_swap = swap_amounts[0]
        output_swap = swap_amounts[-1]
        input_amount = _extract_amount_in(input_swap)
        output_amount = _extract_amount_out(output_swap)
        profit_amount = output_amount - input_amount

        return ArbitrageCalculationResult(
            id=self._id,
            input_token=self._input_token,
            profit_token=self._input_token,
            input_amount=input_amount,
            profit_amount=profit_amount,
            swap_amounts=tuple(swap_amounts),
            state_block=None,
        )

    def _refresh_hop_states(self) -> None:
        for i, pool in enumerate(self._pools):
            self._hop_states[i] = _pool_to_hop_state(pool, self._swap_vectors[i].zero_for_one)

    def notify(self, publisher: Publisher, message: AbstractPublisherMessage) -> None:
        if not isinstance(message, PoolStateMessage):
            return
        if publisher not in self._pool_index:
            return

        idx = self._pool_index[publisher]
        self._hop_states[idx] = _pool_to_hop_state(
            cast("ArbitrageCapablePool", publisher),
            self._swap_vectors[idx].zero_for_one,
        )
        try:
            result = self.calculate()
            self._notify_subscribers(_ProfitableStateDiscovered(result, self))
        except OptimizationError:
            self._notify_subscribers(_StateUpdatedNoProfit(self))

    def _notify_subscribers(self: Publisher, message: AbstractPublisherMessage) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(publisher=self, message=message)


def _extract_amount_in(swap: AbstractSwapAmounts) -> int:

    match swap:
        case UniswapV2PoolSwapAmounts():
            return max(swap.amounts_in)
        case UniswapV3PoolSwapAmounts():
            return swap.amount_in
        case UniswapV4PoolSwapAmounts():
            return swap.amount_in
    msg = f"Unsupported swap amount type: {type(swap).__name__}"
    raise PathValidationError(msg)


def _extract_amount_out(swap: AbstractSwapAmounts) -> int:

    match swap:
        case UniswapV2PoolSwapAmounts():
            return max(swap.amounts_out)
        case UniswapV3PoolSwapAmounts():
            return swap.amount_out
        case UniswapV4PoolSwapAmounts():
            return swap.amount_out
    msg = f"Unsupported swap amount type: {type(swap).__name__}"
    raise PathValidationError(msg)
