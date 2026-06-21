from fractions import Fraction

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.uniswap.liquidity_pool import LiquidityPool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from tests.fakes.tokens import FakeToken
from tests.helpers.v2_pool_factory import make_v2_pool
from tests.helpers.v3_pool_factory import make_v3_pool


def _make_token(address: str, decimals: int = 18) -> FakeToken:
    return FakeToken(address=address, decimals=decimals)


def _make_v2_pool(
    token0: FakeToken,
    token1: FakeToken,
    reserve0: int = 10**18,
    reserve1: int = 2 * 10**18,
    fee: Fraction = Fraction(3, 1000),
    address: str = "0x0000000000000000000000000000000000000001",
) -> LiquidityPool:
    return make_v2_pool(
        address=address,  # type: ignore[arg-type]
        token0=token0,  # type: ignore[arg-type]
        token1=token1,  # type: ignore[arg-type]
        factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        fee_token0=fee,
        fee_token1=fee,
        reserves_token0=reserve0,
        reserves_token1=reserve1,
        state_block=1,
    )


def _make_v3_pool(
    token0: FakeToken,
    token1: FakeToken,
    liquidity: int = 10**18,
    sqrt_price_x96: int = 2**96,
    tick: int = 0,
    fee: int = 3000,
    tick_spacing: int = 60,
    address: str = "0x0000000000000000000000000000000000000002",
) -> UniswapV3Pool:
    return make_v3_pool(
        address=address,  # type: ignore[arg-type]
        token0=token0,  # type: ignore[arg-type]
        token1=token1,  # type: ignore[arg-type]
        factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        fee=fee,
        tick_spacing=tick_spacing,
        sqrt_price_x96=sqrt_price_x96,
        tick=tick,
        liquidity=liquidity,
        state_block=1,
    )


def _make_aerodrome_pool(
    token0: FakeToken,
    token1: FakeToken,
    reserve0: int = 10**18,
    reserve1: int = 2 * 10**18,
    fee: Fraction = Fraction(3, 1000),
    *,
    stable: bool = False,
    address: str = "0x0000000000000000000000000000000000000003",
) -> AerodromeV2Pool:
    return AerodromeV2Pool(
        address=address,  # type: ignore[arg-type]
        token0=token0,  # type: ignore[arg-type]
        token1=token1,  # type: ignore[arg-type]
        factory="0xAe461cA67B15dc8dc81CE7615e0320dA1A9aB8D5",
        fee=fee,
        stable=stable,
        reserves_token0=reserve0,
        reserves_token1=reserve1,
        state_block=1,
    )
