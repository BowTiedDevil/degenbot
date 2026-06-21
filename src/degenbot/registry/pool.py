"""Pool registry: address-keyed store of built pool instances."""

from __future__ import annotations

from typing import TYPE_CHECKING, overload

from degenbot.registry.base import AddressRegistry, MultiKeyAddressRegistry
from degenbot.types.pool_protocols import ConcentratedLiquidityPool

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.degenbot_rs import PyBot
    from degenbot.types.abstract import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


type PoolId = bytes


class ManagedPoolRegistry(MultiKeyAddressRegistry["ConcentratedLiquidityPool"]):
    """Registry for V4 pools keyed by (chain_id, pool_manager_address, pool_id)."""

    def __init__(self) -> None:
        """Initialize the instance."""
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
        """Retrieve a V4 pool by chain, manager address, and pool ID.

        Returns:
            The registered V4 pool, or None if not found.

        """
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
        *,
        py_bot: PyBot | None = None,
    ) -> None:
        """Initialize the instance.

        Args:
            managed_pool_registry: Optional managed (V4) pool sub-registry.
            py_bot: Optional ``PyBot`` handle for Rust-state propagation.
                When set, ``remove`` and ``_reset`` propagate to the Rust
                ``BotState`` via ``py_bot.unregister_pool`` (ADR-007). When
                ``None`` (e.g. tests that construct ``PoolRegistry()``
                standalone), removal is Python-only — Rust state is untouched.

        """
        super().__init__(name="Pool")
        self._managed_pool_registry = managed_pool_registry or ManagedPoolRegistry()
        self._py_bot = py_bot

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
        """Retrieve a pool by chain and address.

        Returns:
            The registered pool, or None if not found.

        """
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

        Raises:
            TypeError: If pool_id is provided but pool does not satisfy ConcentratedLiquidityPool.

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
        """Remove a pool.

        For V2/V3 pools (``pool_id`` is ``None``), propagates to the Rust
        ``BotState`` via ``py_bot.unregister_pool`` so the Rust-owned state
        stays symmetric with the Python registry (ADR-007). V4 pools
        (``pool_id`` is bytes) are Python-only here — V4 unregister is
        engine-side (see ADR-007 Deferred).
        """
        if isinstance(pool_id, bytes):
            self._managed_pool_registry.remove(
                chain_id=chain_id,
                pool_manager_address=pool_address,
                pool_id=pool_id,
            )
        elif self._py_bot is not None:
            # V2/V3 path: propagate to Rust before removing the Python entry
            # (the Rust side is silent-on-miss, so ordering is safe).
            self._py_bot.unregister_pool(address=pool_address)
        self._remove(chain_id=chain_id, address=pool_address)

    def _reset(self) -> None:
        """Reset both the main registry and the managed pool registry.

        Propagates V2/V3 removal to the Rust ``BotState`` before clearing
        Python storage (ADR-007). V4 pools are Python-only (engine-side
        unregister is deferred).
        """
        if self._py_bot is not None:
            # Iterate storage keys (not pool objects) — the key's second
            # element is the checksummed address; tests may store mocks.
            for _chain_id, address in self._storage():
                self._py_bot.unregister_pool(address=address)
        self.reset()
        self._managed_pool_registry.reset()
