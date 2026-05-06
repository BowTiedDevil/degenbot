"""Consolidated fake pool implementations.

Replaces the previous ad hoc pool fakes:
- FakeUniswapV4Pool (from tests/test_registry.py, tests/test_registry_offline.py,
  tests/test_managed_pool_registry.py) — triple duplicate
- MockLiquidityPool(UniswapV2Pool) (from tests/test_cvxpy.py,
  tests/arbitrage/integration/test_uniswap_lp_cycle.py) — duplicate
- MockV3LiquidityPool(UniswapV3Pool) (from tests/arbitrage/integration/test_uniswap_lp_cycle.py)
"""

from collections import deque
from threading import Lock
from weakref import WeakSet

from degenbot.constants import ZERO_ADDRESS
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolState
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import UniswapV3PoolState


class FakeUniswapV4Pool(AbstractLiquidityPool):
    """Minimal fake Uniswap V4 pool for registry tests."""

    def __init__(self, address: str, pool_id: str) -> None:
        self.address = address
        self.pool_id = pool_id
        self.name = f"FakeUniswapV4Pool-{address}"

    @property
    def tokens(self) -> tuple[object, object]:
        return (object(), object())

    def simulate_swap(
        self,
        token_in: str,
        amount_in: int,
        token_out: str,
        state_override: object | None = None,
    ) -> object:
        return object()

    def __eq__(self, other: object) -> bool:
        if isinstance(other, FakeUniswapV4Pool):
            return self.address == other.address and self.pool_id == other.pool_id
        return False

    def __hash__(self) -> int:
        return hash(self.address + self.pool_id)


class MockLiquidityPool(UniswapV2Pool):
    """Mock V2 pool with empty state for testing.

    Bypasses the full UniswapV2Pool constructor by providing a minimal
    initial state cache.
    """

    def __init__(self) -> None:
        self._state_cache = deque([
            UniswapV2PoolState(
                address=ZERO_ADDRESS,
                reserves_token0=0,
                reserves_token1=0,
                block=0,
            )
        ])
        self._state_lock = Lock()
        self._subscribers = WeakSet()


class MockV3LiquidityPool(UniswapV3Pool):
    """Mock V3 pool with empty state for testing.

    Bypasses the full UniswapV3Pool constructor by providing a minimal
    initial state cache.
    """

    def __init__(self) -> None:
        self._state_cache = deque()
        self._state_cache.append(
            UniswapV3PoolState(
                address=ZERO_ADDRESS,
                block=0,
                liquidity=0,
                sqrt_price_x96=0,
                tick=0,
                tick_bitmap={},
                tick_data={},
            )
        )
        self._state_lock = Lock()
        self._subscribers = WeakSet()
