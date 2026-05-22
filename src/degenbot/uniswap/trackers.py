"""Uniswap pool and factory trackers for event-driven updates."""
from __future__ import annotations

import contextlib
from threading import Lock
from typing import TYPE_CHECKING

from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions.pool import (
    LiquidityPoolError,
    PoolCreationFailed,
    PoolNotAssociated,
)
from degenbot.logging import logger
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.abstract import AbstractPoolTracker
from degenbot.uniswap.v2_functions import generate_v2_pool_address
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_functions import generate_v3_pool_address
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.bot import Bot
    from degenbot.types.aliases import ChainId
    from degenbot.uniswap.v3_snapshot import UniswapV3LiquiditySnapshot


class AbstractUniswapV2PoolTracker[Pool: UniswapV2Pool](AbstractPoolTracker[Pool]):
    """AbstractUniswapV2PoolTracker class."""

    def __init__(
        self,
        factory_address: str,
        bot: Bot,
        *,
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

    def get_pool(
        self,
        pool_address: ChecksumAddress | str,
        *,
        silent: bool = False,
    ) -> Pool:
        """Get a pool from its address.

        If the pool is already tracked or found in the global registry,
        that instance will be returned. Otherwise, a new one will be built.

        Returns:
            The pool instance.

        Raises:
            PoolNotAssociated: If the pool is not associated with this tracker.
            PoolCreationFailed: If the pool could not be built.

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
            new_pool = self._bot.build_pool(
                pool_address,
                chain_id=self.chain_id,
                silent=silent,
            )
        except LiquidityPoolError as exc:
            raise PoolCreationFailed(
                message=f"Could not build V2 pool {pool_address}: {exc}"
            ) from exc
        else:
            if not isinstance(new_pool, self.pool_factory):
                raise PoolCreationFailed(
                    message=f"Pool {pool_address} is not a {self.pool_factory.__name__}"
                )
            self._add_tracked_pool(new_pool)
            return new_pool


class UniswapV2PoolTracker(AbstractUniswapV2PoolTracker[UniswapV2Pool], pool_factory=UniswapV2Pool):
    """A class that generates and tracks concrete instances of a Uniswap V2.

    liquidity pool helper or one of its child classes.
    """

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            The string representation of the V2 tracker.

        """
        return f"{self.__class__.__name__}(factory={self._factory_address})"

    @property
    def pool_init_hash(self) -> str:
        """Return pool init hash.

        Returns:
            The pool init hash string.

        """
        return self._pool_init_hash

    def get_pool_from_tokens(
        self,
        token_addresses: tuple[str, str],
        *,
        silent: bool = False,
    ) -> UniswapV2Pool:
        """Get a pool by its token addresses.

        Returns:
            The pool instance.

        """
        pool_address = generate_v2_pool_address(
            deployer_address=self._deployer_address,
            token_addresses=token_addresses,
            init_hash=self._pool_init_hash,
        )

        return self.get_pool(
            pool_address=pool_address,
            silent=silent,
        )


class AbstractUniswapV3PoolTracker[Pool: UniswapV3Pool](AbstractPoolTracker[Pool]):
    """AbstractUniswapV3PoolTracker class."""

    _chain_id: ChainId
    _deployer_address: ChecksumAddress
    _factory_address: ChecksumAddress
    _tracked_pools: dict[ChecksumAddress, Pool]
    _untracked_pools: set[ChecksumAddress]
    _lock: Lock
    _pool_init_hash: str
    _snapshot: UniswapV3LiquiditySnapshot | None

    def __init__(
        self,
        factory_address: ChecksumAddress | str,
        bot: Bot,
        *,
        deployer_address: ChecksumAddress | str | None = None,
        chain_id: ChainId | None = None,
        pool_init_hash: str | None = None,
        snapshot: UniswapV3LiquiditySnapshot | None = None,
    ) -> None:
        """Initialize the instance."""
        self._bot = bot

        if chain_id is None:
            chain_id = bot.connections.default_chain_id

        factory_address = get_checksum_address(factory_address)

        deployment = pool_type_registry.get_deployment(chain_id, factory_address)
        if deployment is not None:
            deployer_address = deployment.deployer
            pool_init_hash = deployment.pool_init_hash
        else:
            if pool_init_hash is None:  # pragma: no branch
                logger.info("Pool init hash is unknown. Using Uniswap V3 mainnet default.")
                pool_init_hash = UniswapV3Pool.UNISWAP_V3_MAINNET_POOL_INIT_HASH
            deployer_address = (
                get_checksum_address(deployer_address)
                if deployer_address is not None
                else factory_address
            )

        self._lock = Lock()
        self._chain_id = chain_id
        self._factory_address = factory_address
        self._deployer_address = get_checksum_address(deployer_address)
        self._pool_init_hash = pool_init_hash or ""
        self._snapshot = snapshot
        self._tracked_pools = {}
        self._untracked_pools = set()

    def _apply_pending_liquidity_updates(self, pool: UniswapV3Pool) -> None:
        """Apply all pending updates from the snapshot."""
        if self._snapshot is not None:
            for liquidity_update in self._snapshot.pending_updates(pool.address):
                pool.update_liquidity_map(liquidity_update)

    def get_pool(
        self,
        pool_address: ChecksumAddress | str,
        *,
        silent: bool = False,
    ) -> Pool:
        """Get a pool from its address.

        If the pool is already tracked or found in the pool registry,
        that instance will be returned. Otherwise, a new one will be built via Bot.

        Returns:
            The pool instance.

        Raises:
            PoolNotAssociated: If the pool is not associated with this tracker.
            PoolCreationFailed: If the pool could not be built.

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
            new_pool = self._bot.build_pool(
                pool_address,
                chain_id=self.chain_id,
                silent=silent,
                tick_bitmap=self._snapshot.tick_bitmap(pool_address) if self._snapshot else None,
                tick_data=self._snapshot.tick_data(pool_address) if self._snapshot else None,
            )
        except LiquidityPoolError as exc:  # pragma: no cover
            raise PoolCreationFailed(
                message=f"Could not build V3 pool {pool_address}: {exc}"
            ) from exc
        else:
            if not isinstance(new_pool, self.pool_factory):
                raise PoolCreationFailed(
                    message=f"Pool {pool_address} is not a {self.pool_factory.__name__}"
                )
            self._apply_pending_liquidity_updates(new_pool)
            self._add_tracked_pool(new_pool)
            return new_pool

    def unload_snapshot(self) -> None:
        """Drop the snapshot reference so it can be garbage-collected."""
        self._snapshot = None


class UniswapV3PoolTracker(AbstractUniswapV3PoolTracker[UniswapV3Pool], pool_factory=UniswapV3Pool):
    """A class that generates and tracks concrete instances of a Uniswap V3.

    liquidity pool helper or one of its child classes.
    """

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            The string representation of the V3 tracker.

        """
        return f"{self.__class__.__name__}(factory={self._factory_address})"

    def get_pool_from_tokens_and_fee(
        self,
        token_addresses: tuple[
            ChecksumAddress | str,
            ChecksumAddress | str,
        ],
        pool_fee: int,
        *,
        silent: bool = False,
    ) -> UniswapV3Pool:
        """Get a pool by its token addresses and fee.

        Returns:
            The pool instance.

        """
        pool_address = generate_v3_pool_address(
            deployer_address=self._deployer_address,
            token_addresses=sorted(token_addresses),
            fee=pool_fee,
            init_hash=self._pool_init_hash,
        )

        return self.get_pool(
            pool_address=pool_address,
            silent=silent,
        )
