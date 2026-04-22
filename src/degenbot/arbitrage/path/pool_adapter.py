from fractions import Fraction
from typing import Any, Protocol

from degenbot.arbitrage.path.types import PoolCompatibility, SwapVector
from degenbot.arbitrage.solver.types import HopState
from degenbot.arbitrage.types import AbstractSwapAmounts


class PoolAdapter(Protocol):
    def is_compatible(self, pool: Any) -> PoolCompatibility: ...
    def extract_fee(self, pool: Any, *, zero_for_one: bool) -> Fraction: ...
    def to_hop_state(
        self,
        pool: Any,
        *,
        zero_for_one: bool,
        state_override: Any = None,
    ) -> HopState: ...
    def build_swap_amount(
        self,
        pool: Any,
        swap_vector: SwapVector,
        amount_in: int,
        amount_out: int,
    ) -> AbstractSwapAmounts: ...


_POOL_ADAPTERS: list[tuple[type, PoolAdapter]] = []


def register_pool_adapter(pool_type: type, adapter: PoolAdapter) -> None:
    _POOL_ADAPTERS.append((pool_type, adapter))


def get_adapter(pool: Any) -> PoolAdapter | None:
    for pool_type, adapter in _POOL_ADAPTERS:
        if isinstance(pool, pool_type):
            return adapter
    return None
