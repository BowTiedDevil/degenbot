from __future__ import annotations

from typing import TYPE_CHECKING, overload

from degenbot.registry.base import AddressRegistry, MultiKeyAddressRegistry

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.types.abstract import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId
    from degenbot.types.pool_protocols import ConcentratedLiquidityPool


type PoolId = bytes


class ManagedPoolRegistry(MultiKeyAddressRegistry["ConcentratedLiquidityPool"]):
    """Registry for V4 pools keyed by (chain_id, pool_manager_address, pool_id)."""

    def __init__(self) -> None:
        super().__init__(
            address_fields=("pool_manager_address", "pool_id"),
            name="ManagedPool",
        )

    def get(
        self,
        chain_id: ChainId,
        pool_manager_address: ChecksumAddress,
        pool_id: PoolId,
    ) -> ConcentratedLiquidityPool | None:
        """Retrieve a V4 pool by chain, manager address, and pool ID."""
        return self._get(
            chain_id=chain_id,
            pool_manager_address=pool_manager_address,
            pool_id=pool_id,
        )

    def add(
        self,
        pool: ConcentratedLiquidityPool,
        chain_id: ChainId,
        pool_manager_address: ChecksumAddress,
        pool_id: PoolId,
    ) -> None:
        """Register a V4 pool."""
        self._add(
            item=pool,
            chain_id=chain_id,
            pool_manager_address=pool_manager_address,
            pool_id=pool_id,
        )

    def remove(
        self,
        chain_id: ChainId,
        pool_manager_address: ChecksumAddress,
        pool_id: PoolId,
    ) -> None:
        """Remove a V4 pool."""
        self._remove(
            chain_id=chain_id,
            pool_manager_address=pool_manager_address,
            pool_id=pool_id,
        )


class PoolRegistry(AddressRegistry["AbstractLiquidityPool"]):
    """Registry for liquidity pools keyed by (chain_id, pool_address)."""

    def __init__(
        self,
        managed_pool_registry: ManagedPoolRegistry | None = None,
    ) -> None:
        super().__init__(name="Pool")
        self._managed_pool_registry = managed_pool_registry or ManagedPoolRegistry()

    @overload
    def get(
        self,
        chain_id: ChainId,
        pool_address: ChecksumAddress,
        pool_id: None = None,
    ) -> AbstractLiquidityPool | None: ...

    @overload
    def get(
        self,
        chain_id: ChainId,
        pool_address: ChecksumAddress,
        pool_id: PoolId,
    ) -> ConcentratedLiquidityPool | None: ...

    def get(
        self,
        chain_id: ChainId,
        pool_address: ChecksumAddress,
        pool_id: PoolId | None = None,
    ) -> AbstractLiquidityPool | ConcentratedLiquidityPool | None:
        """Retrieve a pool by chain and address."""
        if isinstance(pool_id, bytes):
            return self._managed_pool_registry.get(
                chain_id=chain_id,
                pool_manager_address=pool_address,
                pool_id=pool_id,
            )
        return self._get(chain_id=chain_id, address=pool_address)

    def add(
        self,
        pool: AbstractLiquidityPool,
        chain_id: ChainId,
        pool_address: ChecksumAddress,
        pool_id: PoolId | None = None,
    ) -> None:
        """Register a pool.

        When pool_id is provided, the pool must satisfy the
        ConcentratedLiquidityPool protocol and is registered in the
        managed pool sub-registry. Otherwise, it is registered as a
        standard pool.
        """
        if isinstance(pool_id, bytes):
            if not isinstance(pool, ConcentratedLiquidityPool):
                msg = "pool must satisfy ConcentratedLiquidityPool when pool_id is provided"
                raise TypeError(msg)
            self._managed_pool_registry.add(
                pool=pool,
                chain_id=chain_id,
                pool_manager_address=pool_address,
                pool_id=pool_id,
            )
        else:
            self._add(item=pool, chain_id=chain_id, address=pool_address)

    @overload
    def remove(
        self,
        chain_id: ChainId,
        pool_address: ChecksumAddress,
        pool_id: PoolId,
    ) -> None: ...

    @overload
    def remove(
        self,
        chain_id: ChainId,
        pool_address: ChecksumAddress,
        pool_id: None = None,
    ) -> None: ...

    def remove(
        self,
        chain_id: ChainId,
        pool_address: ChecksumAddress,
        pool_id: PoolId | None = None,
    ) -> None:
        """Remove a pool."""
        if isinstance(pool_id, bytes):
            self._managed_pool_registry.remove(
                chain_id=chain_id,
                pool_manager_address=pool_address,
                pool_id=pool_id,
            )
        self._remove(chain_id=chain_id, address=pool_address)

    def _reset(self) -> None:
        """Reset both the main registry and the managed pool registry."""
        self.reset()
        self._managed_pool_registry.reset()
