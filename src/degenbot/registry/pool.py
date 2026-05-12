from typing import TYPE_CHECKING

from degenbot.registry.base import AddressRegistry, MultiKeyAddressRegistry
from degenbot.types.aliases import ChainId

if TYPE_CHECKING:
    from degenbot.types.abstract import AbstractLiquidityPool


PoolId = bytes | str
Address = bytes | str


class ManagedPoolRegistry(MultiKeyAddressRegistry["AbstractLiquidityPool"]):
    """Registry for V4 pools keyed by (chain_id, pool_manager_address, pool_id)."""

    def __init__(self) -> None:
        super().__init__(
            address_fields=("pool_manager_address", "pool_id"),
            name="ManagedPool",
        )

    def get(  # type: ignore[override]
        self,
        chain_id: ChainId,
        pool_manager_address: Address,
        pool_id: PoolId,
    ) -> "AbstractLiquidityPool | None":
        """Retrieve a V4 pool by chain, manager address, and pool ID."""
        return super().get(
            chain_id=chain_id,
            pool_manager_address=pool_manager_address,
            pool_id=pool_id,
        )

    def add(  # type: ignore[override]
        self,
        pool: "AbstractLiquidityPool",
        chain_id: ChainId,
        pool_manager_address: Address,
        pool_id: PoolId,
    ) -> None:
        """Register a V4 pool."""
        super().add(
            item=pool,
            chain_id=chain_id,
            pool_manager_address=pool_manager_address,
            pool_id=pool_id,
        )

    def remove(  # type: ignore[override]
        self,
        pool_manager_address: Address,
        chain_id: ChainId,
        pool_id: PoolId,
    ) -> "AbstractLiquidityPool | None":
        """Remove a V4 pool."""
        return super().remove(
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

    def get(  # type: ignore[override]
        self,
        chain_id: ChainId,
        pool_address: Address,
        pool_id: PoolId | None = None,
    ) -> "AbstractLiquidityPool | None":
        """Retrieve a pool by chain and address."""
        if pool_id is not None:
            return self._managed_pool_registry.get(
                chain_id=chain_id,
                pool_manager_address=pool_address,
                pool_id=pool_id,
            )
        return super().get(chain_id=chain_id, address=pool_address)

    def add(  # type: ignore[override]
        self,
        pool: "AbstractLiquidityPool",
        chain_id: ChainId,
        pool_address: Address,
        pool_id: PoolId | None = None,
    ) -> None:
        """Register a pool."""
        if pool_id is not None:
            self._managed_pool_registry.add(
                pool=pool,
                chain_id=chain_id,
                pool_manager_address=pool_address,
                pool_id=pool_id,
            )
        else:
            super().add(item=pool, chain_id=chain_id, address=pool_address)

    def remove(  # type: ignore[override]
        self,
        chain_id: ChainId,
        pool_address: Address,
        pool_id: PoolId | None = None,
    ) -> "AbstractLiquidityPool | None":
        """Remove a pool."""
        if pool_id is not None:
            return self._managed_pool_registry.remove(
                chain_id=chain_id,
                pool_manager_address=pool_address,
                pool_id=pool_id,
            )
        return super().remove(chain_id=chain_id, address=pool_address)

    def _reset(self) -> None:
        """Reset both the main registry and the managed pool registry."""
        super().reset()
        self._managed_pool_registry.reset()
