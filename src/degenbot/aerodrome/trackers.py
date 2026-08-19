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
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import LiquidityPoolError, PoolCreationFailed, PoolNotAssociated
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.abstract.pool_tracker import AbstractPoolTracker
from degenbot.uniswap import resolve_deployer, resolve_v2_init_hash
from degenbot.uniswap.trackers import AbstractUniswapV3PoolTracker

if TYPE_CHECKING:
    from degenbot._ffi import ChecksummedAddress
    from degenbot.bot import Bot
    from degenbot.types.aliases import ChainId


class _AbstractAerodromeV2PoolTracker[Pool: AerodromeV2Pool](AbstractPoolTracker[Pool]):
    """Inject the AerodromeV2Pool class into the parent abstract pool manager.

    Subclasses define the tracking dicts.
    """

    def get_pool(
        self,
        pool_address: ChecksummedAddress | str,
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
                message=f"Could not build V2 pool {pool_address}: {exc}",
            ) from exc
        else:
            self._add_tracked_pool(new_pool)
            return new_pool


class AerodromeV2PoolTracker(
    _AbstractAerodromeV2PoolTracker[AerodromeV2Pool],
    pool_factory=AerodromeV2Pool,
):
    """Generate and track concrete instances of V2 liquidity pool helpers.

    Tracks Uniswap V2 liquidity pool helpers or their child classes.
    """

    def __init__(
        self,
        *,
        factory_address: str,
        bot: Bot,
        chain_id: ChainId | None = None,
        deployer_address: ChecksummedAddress | str | None = None,
        pool_init_hash: str | None = None,
    ) -> None:
        """Initialize the instance."""
        factory_address = get_checksum_address(factory_address)

        self._bot = bot

        if chain_id is None:
            chain_id = bot.chain_id

        deployment = pool_type_registry.get_deployment(chain_id, factory_address)
        pool_implementation_address: str | None = None
        if deployment is not None:
            deployer_address = deployment.deployer
            pool_init_hash = deployment.pool_init_hash
            pool_implementation_address = deployment.implementation_address
        else:
            # Non-JSON Aerodrome/V2 fork: the Rust resolver gives the factory
            # deployer + the Uniswap V2 mainnet fallback init hash (Fork A,
            # NSAZ4X). The EIP-1167 implementation address has no fallback —
            # only JSON-registered Aerodrome factories carry it (a non-JSON
            # Aerodrome fork must be added to deployments.json to derive
            # clone addresses).
            deployer_address = resolve_deployer(chain_id, factory_address)
            pool_init_hash = resolve_v2_init_hash(chain_id, factory_address)

        self._lock = Lock()
        self._chain_id = chain_id
        self._deployer_address = get_checksum_address(deployer_address)
        self._factory_address = factory_address
        self._pool_init_hash = pool_init_hash or ""
        self._pool_implementation_address = (
            get_checksum_address(pool_implementation_address)
            if pool_implementation_address is not None
            else None
        )
        self._tracked_pools = {}
        self._untracked_pools: set[ChecksummedAddress] = set()

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
            implementation_address=self._require_pool_implementation_address(),
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
            implementation_address=self._require_pool_implementation_address(),
            stable=False,
        )

        return self.get_pool(
            pool_address=pool_address,
            silent=silent,
            pool_class_kwargs=pool_class_kwargs,
        )

    def _require_pool_implementation_address(self) -> ChecksummedAddress:
        """Return the EIP-1167 master implementation address, or raise.

        Returns:
            The checksummed implementation address.

        Raises:
            DegenbotValueError: If no implementation address was resolved for
                this factory (the factory is not in ``deployments.json``).

        """
        if self._pool_implementation_address is None:
            msg = (
                f"No Aerodrome pool implementation address registered for "
                f"factory {self._factory_address} on chain {self._chain_id}. "
                "Add the factory to deployments.json with an "
                "'implementation_address' row."
            )
            raise DegenbotValueError(message=msg)
        return self._pool_implementation_address


class AerodromeV3PoolTracker(
    AbstractUniswapV3PoolTracker[AerodromeV3Pool],
    pool_factory=AerodromeV3Pool,
):
    """AerodromeV3PoolTracker class."""

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        """Initialize the instance."""
        super().__init__(*args, **kwargs)
        # Resolve the EIP-1167 master implementation address the Slipstream
        # factory clones (from deployments.json). None for non-JSON factories.
        deployment = pool_type_registry.get_deployment(self._chain_id, self._factory_address)
        self._pool_implementation_address = (
            get_checksum_address(deployment.implementation_address)
            if deployment is not None and deployment.implementation_address is not None
            else None
        )

    def _require_pool_implementation_address(self) -> ChecksummedAddress:
        """Return the EIP-1167 master implementation address, or raise.

        Returns:
            The checksummed implementation address.

        Raises:
            DegenbotValueError: If no implementation address was resolved for
                this factory (the factory is not in ``deployments.json``).

        """
        if self._pool_implementation_address is None:
            msg = (
                f"No Aerodrome V3 (Slipstream) pool implementation address "
                f"registered for factory {self._factory_address} on chain "
                f"{self._chain_id}. Add the factory to deployments.json with "
                "an 'implementation_address' row."
            )
            raise DegenbotValueError(message=msg)
        return self._pool_implementation_address

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
            implementation_address=self._require_pool_implementation_address(),
            tick_spacing=tick_spacing,
        )

        pool = self.get_pool(
            pool_address=pool_address,
            silent=silent,
            pool_class_kwargs=pool_class_kwargs,  # ty:ignore[unknown-argument]
        )
        assert isinstance(pool, AerodromeV3Pool)
        return pool
