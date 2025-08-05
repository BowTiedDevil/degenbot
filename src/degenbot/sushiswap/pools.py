from degenbot.database.models import SushiswapV2PoolTable
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


class SushiswapV2Pool(UniswapV2Pool):
    type DatabasePoolType = SushiswapV2PoolTable


class SushiswapV3Pool(UniswapV3Pool): ...
