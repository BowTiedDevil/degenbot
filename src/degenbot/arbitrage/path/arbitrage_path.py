from collections.abc import Mapping, Sequence
from fractions import Fraction
from typing import Any
from weakref import WeakSet

from degenbot.arbitrage.path import adapters as _adapters  # noqa: F401 trigger registration
from degenbot.arbitrage.path.adapters.concentrated_liquidity import (
    _v3_virtual_reserves,  # noqa: F401 re-export for tests
)
from degenbot.arbitrage.path.pool_adapter import get_adapter
from degenbot.arbitrage.path.types import (
    PathValidationError,
    PoolCompatibility,
    SwapVector,
)
from degenbot.arbitrage.solver.protocol import SolverProtocol
from degenbot.arbitrage.solver.types import HopState, MobiusSolveResult
from degenbot.arbitrage.types import (
    AbstractSwapAmounts,
    ArbitrageCalculationResult,
)
from degenbot.erc20 import Erc20Token
from degenbot.types.concrete import (
    AbstractPublisherMessage,
    PoolStateMessage,
    Publisher,
    PublisherMixin,
    Subscriber,
)


def _check_pool_compatibility(pool: Any) -> PoolCompatibility:
    adapter = get_adapter(pool)
    if adapter is None:
        return PoolCompatibility.INCOMPATIBLE_INVARIANT
    return adapter.is_compatible(pool)


def _extract_fee(pool: Any, zero_for_one: bool) -> Fraction:
    adapter = get_adapter(pool)
    if adapter is None:
        msg = f"Cannot extract fee from {type(pool).__name__}"
        raise PathValidationError(msg)
    return adapter.extract_fee(pool, zero_for_one=zero_for_one)


def _pool_to_hop_state(
    pool: Any,
    zero_for_one: bool,
    state_override: Any = None,
) -> HopState:
    adapter = get_adapter(pool)
    if adapter is None:
        msg = f"No adapter for {type(pool).__name__}"
        raise PathValidationError(msg)
    return adapter.to_hop_state(pool, zero_for_one=zero_for_one, state_override=state_override)


class _ProfitableStateDiscovered(AbstractPublisherMessage):
    __slots__ = ("path", "result")

    def __init__(self, result: MobiusSolveResult, path: "ArbitragePath") -> None:
        self.result = result
        self.path = path


class _StateUpdatedNoProfit(AbstractPublisherMessage):
    __slots__ = ("path",)

    def __init__(self, path: "ArbitragePath") -> None:
        self.path = path


class ArbitragePath(PublisherMixin):
    """
    Event-driven arbitrage path helper.

    Wraps a sequence of Mobius-compatible pools, validates token flow,
    pre-computes directional data, subscribes to state updates, and
    delegates solving to a swappable SolverProtocol implementation.
    """

    def __init__(
        self,
        pools: Sequence[Any],
        input_token: Erc20Token,
        solver: SolverProtocol,
        max_input: int | None = None,
        id: str | None = None,
    ) -> None:
        if len(pools) < 2:
            msg = "Arbitrage path requires at least 2 pools"
            raise PathValidationError(msg)

        self._pools: tuple[Any, ...] = tuple(pools)
        self._input_token = input_token
        self._solver = solver
        self._max_input = max_input
        self._id = id or ""
        self._subscribers: WeakSet[Subscriber] = WeakSet()
        self._last_result: MobiusSolveResult | None = None

        self._validate_pools()
        self._swap_vectors = self._build_swap_vectors()
        self._pool_index: dict[Any, int] = {pool: i for i, pool in enumerate(self._pools)}
        self._hop_states: list[HopState] = [
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
    def solver(self) -> SolverProtocol:
        return self._solver

    @property
    def last_result(self) -> MobiusSolveResult | None:
        return self._last_result

    @property
    def hop_states(self) -> tuple[HopState, ...]:
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

    def set_solver(self, solver: SolverProtocol) -> None:
        self._solver = solver

    def close(self) -> None:
        for pool in self._pools:
            pool.unsubscribe(self)
        self._subscribers.clear()

    def calculate(self) -> MobiusSolveResult:
        self._refresh_hop_states()
        result = self._solver.solve(self._hop_states, max_input=self._max_input)
        self._last_result = result
        return result

    def calculate_with_state_override(
        self,
        state_overrides: dict[Any, Any],
    ) -> MobiusSolveResult:
        hop_states: list[HopState] = []
        for i, pool in enumerate(self._pools):
            state = state_overrides.get(pool)
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

        return self._solver.solve(hop_states, max_input=self._max_input)

    def build_swap_amounts(
        self,
        result: MobiusSolveResult,
        state_overrides: Mapping[Any, Any] | None = None,
    ) -> ArbitrageCalculationResult[AbstractSwapAmounts]:
        if not result.is_profitable:
            msg = "Cannot build swap amounts for unprofitable result"
            raise PathValidationError(msg)

        if state_overrides is None:
            state_overrides = {}

        token_in_quantity = result.optimal_input
        swap_amounts: list[AbstractSwapAmounts] = []

        for pool, sv in zip(self._pools, self._swap_vectors, strict=True):
            if token_in_quantity == 0:
                msg = "A swap would result in an output of zero"
                raise PathValidationError(msg)

            adapter = get_adapter(pool)
            if adapter is None:
                msg = f"No adapter for {type(pool).__name__}"
                raise PathValidationError(msg)

            pool_state = state_overrides.get(pool)
            token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                token_in=sv.token_in,
                token_in_quantity=token_in_quantity,
                override_state=pool_state,
            )
            swap_amounts.append(
                adapter.build_swap_amount(pool, sv, token_in_quantity, token_out_quantity)
            )
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
            publisher,
            self._swap_vectors[idx].zero_for_one,
        )
        result = self.calculate()
        if result.is_profitable:
            self._notify_subscribers(_ProfitableStateDiscovered(result, self))
        else:
            self._notify_subscribers(_StateUpdatedNoProfit(self))

    def _notify_subscribers(self: Publisher, message: AbstractPublisherMessage) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(publisher=self, message=message)


def _extract_amount_in(swap: AbstractSwapAmounts) -> int:
    from degenbot.arbitrage.types import (
        UniswapV2PoolSwapAmounts,
        UniswapV3PoolSwapAmounts,
        UniswapV4PoolSwapAmounts,
    )

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
    from degenbot.arbitrage.types import (
        UniswapV2PoolSwapAmounts,
        UniswapV3PoolSwapAmounts,
        UniswapV4PoolSwapAmounts,
    )

    match swap:
        case UniswapV2PoolSwapAmounts():
            return max(swap.amounts_out)
        case UniswapV3PoolSwapAmounts():
            return swap.amount_out
        case UniswapV4PoolSwapAmounts():
            return swap.amount_out
    msg = f"Unsupported swap amount type: {type(swap).__name__}"
    raise PathValidationError(msg)
