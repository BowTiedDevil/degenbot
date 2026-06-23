"""ArbitragePath: multi-hop swap vector with solver integration."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any
from weakref import WeakSet

from degenbot.arbitrage.path.types import PathValidationError, SwapVector
from degenbot.arbitrage.solvers.hop_types import SolveInput
from degenbot.arbitrage.types import (
    AbstractSwapAmounts,
    ArbitrageCalculationResult,
)
from degenbot.constants import WRAPPED_NATIVE_TOKENS
from degenbot.erc20 import Erc20Token, EtherPlaceholder
from degenbot.exceptions import OptimizationError
from degenbot.exceptions.arbitrage import IncompatiblePoolInvariant


def _tokens_equivalent(a: Erc20Token, b: Erc20Token) -> bool:
    (
        """Return True if two tokens are equivalent, treating EtherPlaceholder and WETH as the same.

    On chains where native ETH exists, V4 pools use EtherPlaceholder while V3 pools
    use the wrapped native ERC20 (WETH). For arbitrage path validation, these are
    economically equivalent.

    Returns:
        True if the tokens are equivalent, False otherwise.
    """
        ""
    )
    if a == b:
        return True

    # Check if one is EtherPlaceholder and the other is the wrapped native token
    a_is_ether = isinstance(a, EtherPlaceholder)
    b_is_ether = isinstance(b, EtherPlaceholder)
    if a_is_ether != b_is_ether:
        wrapped_token = b if a_is_ether else a
        chain_id = wrapped_token.chain_id
        if chain_id is not None:
            weth_address = WRAPPED_NATIVE_TOKENS.get(chain_id)
            if weth_address is not None and wrapped_token.address == weth_address:
                return True

    return False


from degenbot.types.concrete import (  # noqa: E402
    AbstractPublisherMessage,
    PoolStateMessage,
    Publisher,
    PublisherMixin,
    Subscriber,
)

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence

    from eth_typing import ChecksumAddress

    from degenbot.arbitrage.solvers.hop_types import Solver, SolveResult
    from degenbot.types.abstract import AbstractPoolState
    from degenbot.types.hop_types import HopType
    from degenbot.types.pool_protocols import ArbitragePathPool


_MIN_POOLS_FOR_ARBITRAGE_PATH = 2


class _ProfitableStateDiscovered(AbstractPublisherMessage):
    __slots__ = ("path", "result")

    def __init__(self, result: SolveResult, path: ArbitragePath) -> None:
        self.result = result
        self.path = path


class _StateUpdatedNoProfit(AbstractPublisherMessage):
    __slots__ = ("path",)

    def __init__(self, path: ArbitragePath) -> None:
        self.path = path


class ArbitragePath(PublisherMixin):
    """Event-driven arbitrage path helper.

    Wraps a sequence of Mobius-compatible pools, validates token flow, pre-computes directional
    data, subscribes to state updates, and delegates solving to a swappable Solver
    implementation.
    """

    def __init__(
        self,
        pools: Sequence[ArbitragePathPool],
        input_token: Erc20Token,
        solver: Solver,
        max_input: int | None = None,
        id: str | None = None,  # noqa:A002
    ) -> None:
        """Initialize the instance.

        Raises:
            PathValidationError: If the operation fails.

        """
        if len(pools) < _MIN_POOLS_FOR_ARBITRAGE_PATH:
            msg = f"Arbitrage path requires at least {_MIN_POOLS_FOR_ARBITRAGE_PATH} pools"
            raise PathValidationError(msg)

        self._pools: tuple[ArbitragePathPool, ...] = tuple(pools)
        self._input_token = input_token
        self._solver = solver
        self._max_input = max_input
        self._id = id or ""
        self._subscribers: WeakSet[Subscriber] = WeakSet()
        self._last_result: SolveResult | None = None

        self._validate_pools()
        self._swap_vectors = self._build_swap_vectors()
        self._pool_index: dict[ArbitragePathPool, int] = {
            pool: i for i, pool in enumerate(self._pools)
        }
        self._hop_states: list[HopType] = [
            pool.to_hop_state(zero_for_one=self._swap_vectors[i].zero_for_one)
            for i, pool in enumerate(self._pools)
        ]

        for pool in self._pools:
            pool.subscribe(self)

    def _validate_pools(self) -> None:
        for i, pool in enumerate(self._pools):
            try:
                pool.to_hop_state(zero_for_one=True)
            except (IncompatiblePoolInvariant, AttributeError):
                msg = f"Pool {i} ({type(pool).__name__}) is not Mobius-compatible"
                raise PathValidationError(msg) from None

        tokens: list[tuple[Any, Any]] = []
        current = self._input_token
        for pool in self._pools:
            if _tokens_equivalent(current, pool.token0):
                tokens.append((pool.token0, pool.token1))
                current = pool.token1
            elif _tokens_equivalent(current, pool.token1):
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
        if not _tokens_equivalent(final_output, self._input_token):
            msg = f"Path is not cyclic: starts with {self._input_token}, ends with {final_output}"
            raise PathValidationError(msg)

    def _build_swap_vectors(self) -> tuple[SwapVector, ...]:
        vectors: list[SwapVector] = []
        current = self._input_token

        for pool in self._pools:
            if _tokens_equivalent(current, pool.token0):
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
                ),
            )
            current = token_out

        return tuple(vectors)

    @property
    def pools(self) -> tuple[ArbitragePathPool, ...]:
        """Pools."""
        return self._pools

    @property
    def input_token(self) -> Erc20Token:
        """Input token."""
        return self._input_token

    @property
    def swap_vectors(self) -> tuple[SwapVector, ...]:
        """Swap vectors."""
        return self._swap_vectors

    @property
    def solver(self) -> Solver:
        """Solver."""
        return self._solver

    @property
    def last_result(self) -> SolveResult | None:
        """Last result."""
        return self._last_result

    @property
    def hop_states(self) -> tuple[HopType, ...]:
        """Hop states."""
        return tuple(self._hop_states)

    @property
    def max_input(self) -> int | None:
        """Return max input."""
        return self._max_input

    @max_input.setter
    def max_input(self, value: int | None) -> None:
        self._max_input = value

    @property
    def id(self) -> str:
        """Return id."""
        return self._id

    def set_solver(self, solver: Solver) -> None:
        """Set solver."""
        self._solver = solver

    def close(self) -> None:
        """Perform close."""
        for pool in self._pools:
            pool.unsubscribe(self)
        self._subscribers.clear()

    def calculate(self) -> SolveResult:
        """Calculate.

        Returns:
            The computed value.

        """
        self._refresh_hop_states()
        result = self._solver.solve(self._build_solve_input())
        self._last_result = result
        return result

    def calculate_with_state_override(
        self,
        state_overrides: dict[ChecksumAddress, AbstractPoolState],
    ) -> SolveResult:
        """Calculate with state override.

        Returns:
            The computed value.

        """
        hops = self._resolve_state_overrides(state_overrides)
        return self._solver.solve(self._build_solve_input(hops=hops))

    def _resolve_state_overrides(
        self,
        state_overrides: Mapping[ChecksumAddress, AbstractPoolState],
    ) -> tuple[HopType, ...]:
        hop_states: list[HopType] = []
        for i, pool in enumerate(self._pools):
            state = state_overrides.get(pool.address)
            if state is not None:
                hop_states.append(
                    pool.to_hop_state(
                        zero_for_one=self._swap_vectors[i].zero_for_one,
                        state_override=state,
                    ),
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
        """Build swap amounts.

        Returns:
            The computed value.

        Raises:
            PathValidationError: If the operation fails.

        """
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
            swap_amounts.append(
                pool.build_swap_amount(sv.zero_for_one, token_in_quantity, token_out_quantity),
            )
            token_in_quantity = token_out_quantity

        input_swap = swap_amounts[0]
        output_swap = swap_amounts[-1]
        input_amount = input_swap.input_amount()
        output_amount = output_swap.output_amount()
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
            self._hop_states[i] = pool.to_hop_state(zero_for_one=self._swap_vectors[i].zero_for_one)

    def notify(self, publisher: Publisher, message: AbstractPublisherMessage) -> None:
        """Perform notify."""
        if not isinstance(message, PoolStateMessage):
            return

        pool: ArbitragePathPool | None = None
        for p in self._pool_index:
            if p is publisher:
                pool = p
                break
        if pool is None:
            return

        idx = self._pool_index[pool]
        self._hop_states[idx] = pool.to_hop_state(
            zero_for_one=self._swap_vectors[idx].zero_for_one,
        )
        try:
            result = self.calculate()
            self._notify_subscribers(_ProfitableStateDiscovered(result, self))
        except OptimizationError:
            self._notify_subscribers(_StateUpdatedNoProfit(self))
