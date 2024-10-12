from ..uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager


class PancakeV2PoolManager(UniswapV2PoolManager):
    from .pools import PancakeV2Pool as Pool


class PancakeV3PoolManager(UniswapV3PoolManager):
    from .pools import PancakeV3Pool as Pool
