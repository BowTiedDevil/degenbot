from typing import Any

from eth_typing import ChecksumAddress

from degenbot.logging import logger
from degenbot.pancakeswap.pools import PancakeswapV2Pool, PancakeswapV3Pool
from degenbot.uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager


class PancakeswapV2PoolManager(UniswapV2PoolManager, pool_factory=PancakeswapV2Pool): ...


class PancakeswapV3PoolManager(UniswapV3PoolManager, pool_factory=PancakeswapV3Pool):
    def _build_pool(
        self,
        pool_address: ChecksumAddress,
        *,
        silent: bool,
        pool_class_kwargs: dict[str, Any] | None,
    ) -> PancakeswapV3Pool:
        if pool_class_kwargs is None:
            pool_class_kwargs = {}

        if self._snapshot is not None:
            pool = PancakeswapV3Pool(
                address=pool_address,
                tick_bitmap=self._snapshot.tick_bitmap(pool_address),
                tick_data=self._snapshot.tick_data(pool_address),
                silent=silent,
                **pool_class_kwargs,
            )
        else:
            logger.info("Initializing pool without liquidity snapshot")
            pool = PancakeswapV3Pool(
                address=pool_address,
                silent=silent,
                **pool_class_kwargs,
            )
        return pool
