from degenbot.database.models import SwapbasedV2PoolTable
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool


class SwapbasedV2Pool(UniswapV2Pool):
    type DatabasePoolType = SwapbasedV2PoolTable
