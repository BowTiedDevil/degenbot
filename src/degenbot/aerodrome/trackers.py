"""Aerodrome pool and factory trackers for event-driven updates."""
from __future__ import annotations

import contextlib
from threading import Lock
from typing import TYPE_CHECKING, Any, Never

from degenbot.aerodrome.functions import (
    generate_aerodrome_v2_pool_address,
    generate_aerodrome_v3_pool_address,
)
from degenbot.aerodrome.pools import AerodromeV2Pool, AerodromeV3Pool
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions.pool import (
    LiquidityPoolError,
    PoolCreationFailed,
    PoolNotAssociated,
)
from degenbot.logging import logger
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.abstract.pool_tracker import AbstractPoolTracker
from degenbot.uniswap.trackers import AbstractUniswapV3PoolTracker
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.bot import Bot
    from degenbot.types.aliases import ChainId


class _AbstractAerodromeV2PoolTracker[Pool: AerodromeV2Pool](AbstractPoolTracker[Pool]):
    """Inject the AerodromeV2Pool class into the parent abstract pool manager.

    Subclasses define the tracking dicts.
    """

    def get_pool(
        self,
        pool_address: ChecksumAddress | str,
        *,
        silent: bool = False,
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> Pool:
        """Get a pool from its address by checking the registry first.

        If the pool is already tracked or found in the global registry,
        that instance will be returned. Otherwise, a new one will be built.

        Returns:
            The computed value.

        Raises:
            PoolCreationFailed: See function documentation.
            PoolNotAssociated: See function documentation.

        """
        pool_address = get_checksum_address(pool_address)

        with contextlib.suppress(KeyError):
            return self._tracked_pools[pool_address]

        if pool_address in self._untracked_pools:
            raise PoolNotAssociated(pool_address)

        # Check if the pool registry already has this pool
        pool_from_registry = self._bot.pools.get(
            pool_address=pool_address,
            chain_id=self.chain_id,
        )
        if pool_from_registry is not None:
            if TYPE_CHECKING:
                assert isinstance(pool_from_registry, self.pool_factory)
            if pool_from_registry.factory == self._factory_address:
                self._add_tracked_pool(pool_from_registry)
                return pool_from_registry
            self._untracked_pools.add(pool_address)
            raise PoolNotAssociated(pool_address)

        try:
            new_pool = self.pool_factory(
                address=pool_address,
                silent=silent,
                **(pool_class_kwargs or {}),
            )
        except LiquidityPoolError as exc:
            raise PoolCreationFailed(
                message=f"Could not build V2 pool {pool_address}: {exc}"
            ) from exc
        else:
            self._add_tracked_pool(new_pool)
            return new_pool


class AerodromeV2PoolTracker(
    _AbstractAerodromeV2PoolTracker[AerodromeV2Pool], pool_factory=AerodromeV2Pool
):
    """Generate and track concrete instances of V2 liquidity pool helpers.

    Tracks Uniswap V2 liquidity pool helpers or their child classes.
    """

    POOL_IMPLEMENTATION_ADDRESS = get_checksum_address("0xA4e46b4f701c62e14DF11B48dCe76A7d793CD6d7")

    def __init__(
        self,
        *,
        factory_address: str,
        bot: Bot,
        chain_id: ChainId | None = None,
        deployer_address: ChecksumAddress | str | None = None,
        pool_init_hash: str | None = None,
    ) -> None:
        """Initialize the instance."""
        factory_address = get_checksum_address(factory_address)

        self._bot = bot

        if chain_id is None:
            chain_id = bot.connections.default_chain_id

        deployment = pool_type_registry.get_deployment(chain_id, factory_address)
        if deployment is not None:
            deployer_address = deployment.deployer
            pool_init_hash = deployment.pool_init_hash
        else:
            if pool_init_hash is None:  # pragma: no branch
                logger.info("Pool init hash is unknown. Using Uniswap V3 mainnet default.")
                pool_init_hash = UniswapV2Pool.UNISWAP_V2_MAINNET_POOL_INIT_HASH
            deployer_address = (
                get_checksum_address(deployer_address)
                if deployer_address is not None
                else factory_address
            )

        self._lock = Lock()
        self._chain_id = chain_id
        self._deployer_address = get_checksum_address(deployer_address)
        self._factory_address = factory_address
        self._pool_init_hash = pool_init_hash or ""
        self._tracked_pools = {}
        self._untracked_pools: set[ChecksumAddress] = set()

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            A string representation of the object.

        """
        return f"{self.__class__.__name__}(factory={self._factory_address})"

    def get_stable_pool(
        self,
        token_addresses: tuple[str, str],
        *,
        silent: bool = False,
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> AerodromeV2Pool:
        """Get a stable pool by its token addresses.

        The token addresses may be passed in any order.

        Returns:
            The computed value.

        """
        pool_address = generate_aerodrome_v2_pool_address(
            deployer_address=self._deployer_address,
            token_addresses=sorted(token_addresses),
            implementation_address=self.POOL_IMPLEMENTATION_ADDRESS,
            stable=True,
        )

        return self.get_pool(
            pool_address=pool_address,
            silent=silent,
            pool_class_kwargs=pool_class_kwargs,
        )

    def get_volatile_pool(
        self,
        token_addresses: tuple[str, str],
        *,
        silent: bool = False,
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> AerodromeV2Pool:
        """Get a volatile pool by its token addresses.

        The token addresses may be passed in any order.

        Returns:
            The computed value.

        """
        pool_address = generate_aerodrome_v2_pool_address(
            deployer_address=self._deployer_address,
            token_addresses=sorted(token_addresses),
            implementation_address=self.POOL_IMPLEMENTATION_ADDRESS,
            stable=False,
        )

        return self.get_pool(
            pool_address=pool_address,
            silent=silent,
            pool_class_kwargs=pool_class_kwargs,
        )


class AerodromeV3PoolTracker(
    AbstractUniswapV3PoolTracker[AerodromeV3Pool], pool_factory=AerodromeV3Pool
):
    """AerodromeV3PoolTracker class."""

    POOL_IMPLEMENTATION_ADDRESS = get_checksum_address("0xeC8E5342B19977B4eF8892e02D8DAEcfa1315831")

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            A string representation of the object.

        """
        return f"{self.__class__.__name__}(factory={self._factory_address})"

    def get_pool_from_tokens_and_fee(
        self,
        *args: Any,
        **kwargs: Any,
    ) -> Never:
        """Return pool from tokens and fee."""
        raise NotImplementedError

    def get_pool_from_tokens_and_tick_spacing(
        self,
        token_addresses: tuple[str, str],
        tick_spacing: int,
        *,
        silent: bool = False,
        # keyword arguments passed to the pool class constructor
        pool_class_kwargs: dict[str, Any] | None = None,
    ) -> AerodromeV3Pool:
        """Return pool from tokens and tick spacing.

        Returns:
            The computed value.

        """
        pool_address = generate_aerodrome_v3_pool_address(
            deployer_address=self._deployer_address,
            token_addresses=sorted(token_addresses),
            implementation_address=self.POOL_IMPLEMENTATION_ADDRESS,
            tick_spacing=tick_spacing,
        )

        pool = self.get_pool(
            pool_address=pool_address,
            silent=silent,
            pool_class_kwargs=pool_class_kwargs,  # ty:ignore[unknown-argument]
        )
        assert isinstance(pool, AerodromeV3Pool)
        return pool
