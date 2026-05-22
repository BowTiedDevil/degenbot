"""
Consolidated fake pool implementations.

Minimal protocol-fakes for testing:
- FakeV2Pool: captures external_update calls (test spy)
- FakeV3Pool: captures external_update and update_liquidity_map calls (test spy)
- FakeUniswapV4Pool: minimal V4 pool for registry tests

These fake pools are test spies, not mock math engines. For pool math testing,
use production pool classes (UniswapV2Pool, UniswapV3Pool, etc.) constructed
with FakeToken arguments.
"""

from degenbot.types.abstract import AbstractLiquidityPool


class FakeV2Pool:
    """Minimal fake V2 pool that captures external_update calls."""

    def __init__(self) -> None:
        self.last_update = None

    def external_update(self, update: object) -> None:
        self.last_update = update


class FakeV3Pool:
    """Minimal fake V3 pool that captures external_update and update_liquidity_map calls."""

    def __init__(self) -> None:
        self.last_update = None
        self.last_liquidity_update = None

    def external_update(self, update: object) -> None:
        self.last_update = update

    def update_liquidity_map(self, update: object) -> None:
        self.last_liquidity_update = update


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
        _token_in: str,
        _amount_in: int,
        _token_out: str,
        _state_override: object | None = None,
    ) -> object:
        return object()

    def __eq__(self, other: object) -> bool:
        if isinstance(other, FakeUniswapV4Pool):
            return self.address == other.address and self.pool_id == other.pool_id
        return False

    def __hash__(self) -> int:
        return hash(self.address + self.pool_id)
