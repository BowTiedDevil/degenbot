from collections.abc import Callable
from fractions import Fraction
from typing import Any, Protocol

from degenbot.arbitrage.path.types import PoolCompatibility, SwapVector
from degenbot.arbitrage.solver.types import HopState
from degenbot.arbitrage.types import AbstractSwapAmounts


class PoolAdapter[PoolT, StateT](Protocol):
    def extract_fee(self, pool: PoolT, *, zero_for_one: bool) -> Fraction: ...
    def to_hop_state(
        self,
        pool: PoolT,
        *,
        zero_for_one: bool,
        state_override: StateT | None = None,
    ) -> HopState: ...
    def build_swap_amount(
        self,
        pool: PoolT,
        swap_vector: SwapVector,
        amount_in: int,
        amount_out: int,
    ) -> AbstractSwapAmounts: ...


type CompatibilityCheck = Callable[[object], PoolCompatibility]

_POOL_ADAPTERS: list[tuple[type, PoolAdapter[Any, Any], CompatibilityCheck]] = []


def register_pool_adapter(
    pool_type: type,
    adapter: PoolAdapter[Any, Any],
    is_compatible: CompatibilityCheck = lambda _: PoolCompatibility.COMPATIBLE,
) -> None:
    _POOL_ADAPTERS.append((pool_type, adapter, is_compatible))


def get_adapter(pool: object) -> PoolAdapter[Any, Any] | None:
    for pool_type, adapter, _ in _POOL_ADAPTERS:
        if isinstance(pool, pool_type):
            return adapter
    return None


def check_pool_compatibility(pool: object) -> PoolCompatibility:
    for pool_type, _, is_compatible in _POOL_ADAPTERS:
        if isinstance(pool, pool_type):
            return is_compatible(pool)
    return PoolCompatibility.INCOMPATIBLE_INVARIANT
