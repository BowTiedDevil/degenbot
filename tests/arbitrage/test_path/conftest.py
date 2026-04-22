from fractions import Fraction
from unittest.mock import MagicMock

from degenbot.erc20 import Erc20Token
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


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


def _make_token(address: str, decimals: int = 18) -> FakeToken:
    return FakeToken(address, decimals)


def _make_v2_pool(
    token0: Erc20Token,
    token1: Erc20Token,
    reserve0: int = 10**18,
    reserve1: int = 2 * 10**18,
    fee: Fraction = Fraction(3, 1000),
) -> MagicMock:
    pool = MagicMock(spec=UniswapV2Pool)
    pool.token0 = token0
    pool.token1 = token1
    pool.address = "0xpool"
    pool.fee_token0 = fee
    pool.fee_token1 = fee
    pool.state = MagicMock()
    pool.state.reserves_token0 = reserve0
    pool.state.reserves_token1 = reserve1
    pool.subscribe = MagicMock()
    return pool


def _make_v3_pool(
    token0: Erc20Token,
    token1: Erc20Token,
    liquidity: int = 10**18,
    sqrt_price_x96: int = 2**96,
    tick: int = 0,
    fee: int = 3000,
    tick_spacing: int = 60,
) -> MagicMock:
    pool = MagicMock(spec=UniswapV3Pool)
    pool.token0 = token0
    pool.token1 = token1
    pool.address = "0xpool_v3"
    pool.liquidity = liquidity
    pool.sqrt_price_x96 = sqrt_price_x96
    pool.tick = tick
    pool.fee = fee
    pool.FEE_DENOMINATOR = 1_000_000
    pool.tick_spacing = tick_spacing
    pool.tick_data = {}
    pool.tick_bitmap = {}
    pool.sparse_liquidity_map = True
    pool.subscribe = MagicMock()
    return pool
