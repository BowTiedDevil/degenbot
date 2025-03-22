from typing import Any

from degenbot.aerodrome.functions import (
    generate_aerodrome_v2_pool_address,
    generate_aerodrome_v3_pool_address,
)
from degenbot.aerodrome.pools import AerodromeV2Pool, AerodromeV3Pool
from degenbot.cache import get_checksum_address
from degenbot.uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager


class AerodromeV2PoolManager(UniswapV2PoolManager):
    type Pool = AerodromeV2Pool
    POOL_IMPLEMENTATION_ADDRESS = get_checksum_address("0xA4e46b4f701c62e14DF11B48dCe76A7d793CD6d7")

    def __repr__(self) -> str:  # pragma: no cover
        return f"AerodromeV2PoolManager(factory={self._factory_address})"

    def get_pool_from_tokens_and_stable_type(
        self,
        token_addresses: tuple[str, str],
        stable: bool,
        silent: bool = False,
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> Pool:
        """
        Get a pool by its token addresses and the stable bool. The token addresses may be passed in
        any order.
        """

        pool_address = generate_aerodrome_v2_pool_address(
            deployer_address=self._deployer_address,
            token_addresses=sorted(token_addresses),
            implementation_address=self.POOL_IMPLEMENTATION_ADDRESS,
            stable=stable,
        )

        pool = self.get_pool(
            pool_address=pool_address,
            silent=silent,
            pool_class_kwargs=pool_class_kwargs,
        )
        assert isinstance(pool, AerodromeV2Pool)
        return pool


class AerodromeV3PoolManager(UniswapV3PoolManager):
    POOL_IMPLEMENTATION_ADDRESS = get_checksum_address("0xeC8E5342B19977B4eF8892e02D8DAEcfa1315831")
    type Pool = AerodromeV3Pool

    def __repr__(self) -> str:  # pragma: no cover
        return f"AerodromeV3PoolManager(factory={self._factory_address})"

    def get_pool_from_tokens_and_tick_spacing(
        self,
        token_addresses: tuple[str, str],
        tick_spacing: int,
        silent: bool = False,
        # keyword arguments passed to the pool class constructor
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> Pool:
        pool_address = generate_aerodrome_v3_pool_address(
            deployer_address=self._deployer_address,
            token_addresses=sorted(token_addresses),
            implementation_address=self.POOL_IMPLEMENTATION_ADDRESS,
            tick_spacing=tick_spacing,
        )

        pool = self.get_pool(
            pool_address=pool_address,
            silent=silent,
            pool_class_kwargs=pool_class_kwargs,
        )
        assert isinstance(pool, AerodromeV3Pool)
        return pool
