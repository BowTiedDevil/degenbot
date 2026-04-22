from dataclasses import dataclass
from fractions import Fraction
from weakref import WeakSet

from degenbot.types.abstract import (
    AbstractAerodromeV2Pool,
    AbstractConcentratedLiquidityPool,
    AbstractUniswapV2Pool,
)
from degenbot.types.concrete import PublisherMixin


class FakeToken:
    def __init__(self, address: str, decimals: int = 18) -> None:
        self.address = address
        self.decimals = decimals

    def __eq__(self, other: object) -> bool:
        if isinstance(other, FakeToken):
            return self.address.lower() == other.address.lower()
        return NotImplemented

    def __hash__(self) -> int:
        return hash(self.address.lower())

    def __repr__(self) -> str:
        return f"FakeToken({self.address})"


@dataclass
class FakeV2PoolState:
    reserves_token0: int
    reserves_token1: int


class FakeUniswapV2Pool(AbstractUniswapV2Pool, PublisherMixin):
    def __init__(
        self,
        token0: FakeToken,
        token1: FakeToken,
        reserve0: int = 10**18,
        reserve1: int = 2 * 10**18,
        fee: Fraction = Fraction(3, 1000),
        address: str = "0xpool",
    ) -> None:
        self.token0 = token0
        self.token1 = token1
        self.address = address
        self.name = f"FakeV2({address})"
        self.fee_token0 = fee
        self.fee_token1 = fee
        self._state = FakeV2PoolState(reserve0, reserve1)
        self._subscribers: WeakSet = WeakSet()

    @property
    def state(self) -> FakeV2PoolState:
        return self._state


@dataclass
class FakeCLPoolState:
    liquidity: int
    sqrt_price_x96: int
    tick: int


class FakeConcentratedLiquidityPool(AbstractConcentratedLiquidityPool, PublisherMixin):
    FEE_DENOMINATOR = 1_000_000

    def __init__(
        self,
        token0: FakeToken,
        token1: FakeToken,
        liquidity: int = 10**18,
        sqrt_price_x96: int = 2**96,
        tick: int = 0,
        fee: int = 3000,
        tick_spacing: int = 60,
        address: str = "0xpool_cl",
    ) -> None:
        self.token0 = token0
        self.token1 = token1
        self.address = address
        self.name = f"FakeCL({address})"
        self.fee = fee
        self.tick_spacing = tick_spacing
        self.tick_data: dict = {}
        self.tick_bitmap: dict = {}
        self.sparse_liquidity_map = True
        self._state = FakeCLPoolState(liquidity, sqrt_price_x96, tick)
        self._subscribers: WeakSet = WeakSet()

    @property
    def state(self) -> FakeCLPoolState:
        return self._state

    @property
    def liquidity(self) -> int:
        return self._state.liquidity

    @property
    def sqrt_price_x96(self) -> int:
        return self._state.sqrt_price_x96

    @property
    def tick(self) -> int:
        return self._state.tick


class FakeAerodromeV2Pool(AbstractAerodromeV2Pool, PublisherMixin):
    def __init__(
        self,
        token0: FakeToken,
        token1: FakeToken,
        reserve0: int = 10**18,
        reserve1: int = 2 * 10**18,
        fee: Fraction = Fraction(3, 1000),
        *,
        stable: bool = False,
        address: str = "0xpool_aero",
    ) -> None:
        self.token0 = token0
        self.token1 = token1
        self.address = address
        self.name = f"FakeAero({address})"
        self.fee = fee
        self.stable = stable
        self._state = FakeV2PoolState(reserve0, reserve1)
        self._subscribers: WeakSet = WeakSet()

    @property
    def state(self) -> FakeV2PoolState:
        return self._state


class FakeSubscriber:
    def __init__(self) -> None:
        self.notifications: list[tuple] = []

    def notify(self, publisher: object, message: object) -> None:
        self.notifications.append((publisher, message))


def _make_token(address: str, decimals: int = 18) -> FakeToken:
    return FakeToken(address, decimals)


def _make_v2_pool(
    token0: FakeToken,
    token1: FakeToken,
    reserve0: int = 10**18,
    reserve1: int = 2 * 10**18,
    fee: Fraction = Fraction(3, 1000),
) -> FakeUniswapV2Pool:
    return FakeUniswapV2Pool(
        token0=token0,
        token1=token1,
        reserve0=reserve0,
        reserve1=reserve1,
        fee=fee,
    )


def _make_v3_pool(
    token0: FakeToken,
    token1: FakeToken,
    liquidity: int = 10**18,
    sqrt_price_x96: int = 2**96,
    tick: int = 0,
    fee: int = 3000,
    tick_spacing: int = 60,
) -> FakeConcentratedLiquidityPool:
    return FakeConcentratedLiquidityPool(
        token0=token0,
        token1=token1,
        liquidity=liquidity,
        sqrt_price_x96=sqrt_price_x96,
        tick=tick,
        fee=fee,
        tick_spacing=tick_spacing,
    )
