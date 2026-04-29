from dataclasses import dataclass
from fractions import Fraction
from typing import TYPE_CHECKING
from weakref import WeakSet

from degenbot.types.abstract import (
    AbstractAerodromeV2Pool,
    AbstractConcentratedLiquidityPool,
    AbstractPoolState,
    AbstractUniswapV2Pool,
)
from degenbot.types.concrete import PublisherMixin
from degenbot.types.hop_types import BoundedProductHop, ConstantProductHop, HopType
from degenbot.types.pool_protocols import SimulationResult

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress


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


@dataclass(frozen=True, kw_only=True)
class FakeV2PoolState(AbstractPoolState):
    reserves_token0: int = 0
    reserves_token1: int = 0


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
        self.address: ChecksumAddress = address  # type: ignore[assignment]
        self.name = f"FakeV2({address})"
        self.fee_token0 = fee
        self.fee_token1 = fee
        self._state = FakeV2PoolState(
            address=self.address,
            block=None,
            reserves_token0=reserve0,
            reserves_token1=reserve1,
        )
        self._subscribers: WeakSet[object] = WeakSet()

    @property
    def state(self) -> FakeV2PoolState:
        return self._state

    def to_hop_state(
        self, zero_for_one: bool,  # noqa: FBT001
        state_override: AbstractPoolState | None = None,
    ) -> HopType:
        state = (
            state_override
            if isinstance(state_override, FakeV2PoolState)
            else self._state
        )
        if zero_for_one:
            return ConstantProductHop(
                reserve_in=state.reserves_token0,
                reserve_out=state.reserves_token1,
                fee=self.fee_token0,
            )
        return ConstantProductHop(
            reserve_in=state.reserves_token1,
            reserve_out=state.reserves_token0,
            fee=self.fee_token1,
        )

    def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001
        return self.fee_token0 if zero_for_one else self.fee_token1

    def simulate_swap(
        self,
        token_in: str,
        amount_in: int,
        token_out: str,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult:
        state = (
            state_override
            if isinstance(state_override, FakeV2PoolState)
            else self._state
        )
        zfo = token_in == self.token0.address
        r_in = state.reserves_token0 if zfo else state.reserves_token1
        r_out = state.reserves_token1 if zfo else state.reserves_token0
        fee = self.fee_token0 if zfo else self.fee_token1
        amount_in_with_fee = amount_in - int(amount_in * fee)
        amount_out = r_out * amount_in_with_fee // (r_in + amount_in_with_fee)
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=state,
            final_state=state,
        )

    def subscribe(self, subscriber: object) -> None:
        self._subscribers.add(subscriber)

    def unsubscribe(self, subscriber: object) -> None:
        self._subscribers.discard(subscriber)


@dataclass(frozen=True, kw_only=True)
class FakeCLPoolState(AbstractPoolState):
    liquidity: int = 0
    sqrt_price_x96: int = 0
    tick: int = 0


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
        self.address: ChecksumAddress = address  # type: ignore[assignment]
        self.name = f"FakeCL({address})"
        self.fee = fee
        self.tick_spacing = tick_spacing
        self.tick_data: dict[str, object] = {}
        self.tick_bitmap: dict[str, object] = {}
        self.sparse_liquidity_map = True
        self._state = FakeCLPoolState(
            address=self.address,
            block=None,
            liquidity=liquidity,
            sqrt_price_x96=sqrt_price_x96,
            tick=tick,
        )
        self._subscribers: WeakSet[object] = WeakSet()

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

    def to_hop_state(
        self, zero_for_one: bool,  # noqa: FBT001
        state_override: AbstractPoolState | None = None,
    ) -> HopType:
        from degenbot.uniswap.v3_libraries.functions import v3_virtual_reserves
        from degenbot.uniswap.v3_libraries.tick_math import MAX_TICK, MIN_TICK

        state = (
            state_override
            if isinstance(state_override, FakeCLPoolState)
            else self._state
        )
        reserve_in, reserve_out = v3_virtual_reserves(
            state.liquidity,
            state.sqrt_price_x96,
            zero_for_one=zero_for_one,
        )
        return BoundedProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=Fraction(self.fee, self.FEE_DENOMINATOR),
            liquidity=state.liquidity,
            sqrt_price=state.sqrt_price_x96,
            tick_lower=MIN_TICK,
            tick_upper=MAX_TICK,
        )

    def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001
        return Fraction(self.fee, self.FEE_DENOMINATOR)

    def simulate_swap(
        self,
        token_in: str,
        amount_in: int,
        token_out: str,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult:
        return SimulationResult(
            amount_in=amount_in,
            amount_out=0,
            initial_state=self._state,
            final_state=self._state,
        )

    def subscribe(self, subscriber: object) -> None:
        self._subscribers.add(subscriber)

    def unsubscribe(self, subscriber: object) -> None:
        self._subscribers.discard(subscriber)


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
        self.address: ChecksumAddress = address  # type: ignore[assignment]
        self.name = f"FakeAero({address})"
        self.fee = fee
        self.stable = stable
        self._state = FakeV2PoolState(
            address=self.address,
            block=None,
            reserves_token0=reserve0,
            reserves_token1=reserve1,
        )
        self._subscribers: WeakSet[object] = WeakSet()

    @property
    def state(self) -> FakeV2PoolState:
        return self._state

    def to_hop_state(
        self, zero_for_one: bool,  # noqa: FBT001
        state_override: AbstractPoolState | None = None,
    ) -> HopType:
        from degenbot.exceptions.arbitrage import IncompatiblePoolInvariant

        if self.stable:
            raise IncompatiblePoolInvariant(message="Stable pools not supported")

        state = (
            state_override
            if isinstance(state_override, FakeV2PoolState)
            else self._state
        )
        if zero_for_one:
            return ConstantProductHop(
                reserve_in=state.reserves_token0,
                reserve_out=state.reserves_token1,
                fee=self.fee,
            )
        return ConstantProductHop(
            reserve_in=state.reserves_token1,
            reserve_out=state.reserves_token0,
            fee=self.fee,
        )

    def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001
        return self.fee

    def simulate_swap(
        self,
        token_in: str,
        amount_in: int,
        token_out: str,
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult:
        state = (
            state_override
            if isinstance(state_override, FakeV2PoolState)
            else self._state
        )
        zfo = token_in == self.token0.address
        r_in = state.reserves_token0 if zfo else state.reserves_token1
        r_out = state.reserves_token1 if zfo else state.reserves_token0
        fee = self.fee
        amount_in_with_fee = amount_in - int(amount_in * fee)
        amount_out = r_out * amount_in_with_fee // (r_in + amount_in_with_fee)
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=state,
            final_state=state,
        )

    def subscribe(self, subscriber: object) -> None:
        self._subscribers.add(subscriber)

    def unsubscribe(self, subscriber: object) -> None:
        self._subscribers.discard(subscriber)


class FakeSubscriber:
    def __init__(self) -> None:
        self.notifications: list[tuple[object, object]] = []

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
