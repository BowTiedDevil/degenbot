"""
Unified pool type registry.

Replaces PoolClassRegistry, FACTORY_DEPLOYMENTS lookups, _KIND_TO_DESCRIPTOR,
and _variant_from_class with a single registration mechanism.

Identity (family, variant, kind) is auto-derived from the class hierarchy
and class attributes. Deployment data (chain_id, factory, deployer, init_hash)
is carried by the registration call.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from degenbot.types.pool_type import PoolFamily, PoolTypeDescriptor, derive_kind

if TYPE_CHECKING:
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


@dataclass(frozen=True)
class PoolDeploymentData:
    """Per-chain deployment data for a pool factory."""

    factory_address: str
    deployer: str
    pool_init_hash: str | None


def _derive_family(pool_class: type[AbstractLiquidityPool]) -> PoolFamily:
    """Derive the pool family from the class hierarchy."""
    from degenbot.types.abstract.liquidity_pool import (  # noqa: PLC0415
        AbstractAerodromeV2Pool,
        AbstractConcentratedLiquidityPool,
        AbstractUniswapV2Pool,
    )

    if issubclass(pool_class, AbstractConcentratedLiquidityPool):
        return PoolFamily.CONCENTRATED_LIQUIDITY
    if issubclass(pool_class, (AbstractUniswapV2Pool, AbstractAerodromeV2Pool)):
        return PoolFamily.CONSTANT_PRODUCT

    msg = (
        f"Cannot derive pool family for {pool_class.__name__}. "
        f"The class must inherit from AbstractUniswapV2Pool, "
        f"AbstractConcentratedLiquidityPool, or AbstractAerodromeV2Pool."
    )
    raise ValueError(msg)


class PoolTypeRegistry:
    """
    Unified registry mapping (chain_id, factory_address) → pool type identity.

    Each DEX module registers its pool subclass at import time via register().
    Builders consult this registry to select the concrete class and its
    deployment data.

    Family, variant, and kind are auto-derived from the class hierarchy
    and the class's `variant` attribute.

    Public API for external callers
    --------------------------------
    Library users who want to register a custom DEX pool class should:

    1. Subclass an abstract pool base (``AbstractUniswapV2Pool`` or
       ``AbstractConcentratedLiquidityPool``).

    2. Add a ``variant: ClassVar[str | None] = "your_dex_name"`` class
       attribute. Use the bare DEX name without a ``_v2``/``_v3`` suffix
       — the suffix is derived automatically from the family.

    3. If the pool has a non-standard constructor (e.g. requires
       chain fetches for extra parameters), add a builder method
       (e.g. ``_build_aerodrome_v2``, ``_build_camelot``) on the
       appropriate pool builder class. The builder will be dispatched
       via ``issubclass`` checks in the ``build()`` method.

    4. Call ``pool_type_registry.register()`` with the class, chain ID,
       factory address, and optional deployment data.

    Example::

        from degenbot.registry import pool_type_registry
        from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

        class MyCustomPool(UniswapV2Pool):
            variant: ClassVar[str | None] = "my_dex"

        pool_type_registry.register(
            MyCustomPool,
            chain_id=1,
            factory_address="0x...",
            pool_init_hash="0x...",
        )

    After registration, ``Bot.build_pool()`` will automatically select
    ``MyCustomPool`` for any pool whose ``factory()`` returns the registered
    address on the given chain.
    """

    def __init__(self) -> None:
        self._entries: dict[tuple[int, str], _RegistryEntry] = {}
        self._kind_index: dict[str, PoolTypeDescriptor] = {}
        self._default_v2_class: type[AbstractLiquidityPool] | None = None
        self._default_v3_class: type[AbstractLiquidityPool] | None = None

    # --- Registration ---

    def register(
        self,
        pool_class: type[AbstractLiquidityPool],
        *,
        chain_id: ChainId,
        factory_address: str,
        pool_init_hash: str | None = None,
        deployer: str | None = None,
    ) -> None:
        """
        Register a pool class for a specific (chain_id, factory) deployment.

        Identity (family, variant, kind) is auto-derived from the class.
        Deployment data (chain_id, factory, deployer, init_hash) is stored
        alongside for lookup.

        Args:
            pool_class: The concrete pool class.
            chain_id: The chain ID for this deployment.
            factory_address: The factory contract address.
            pool_init_hash: The CREATE2 init code hash (V2 only).
            deployer: The CREATE2 deployer address (defaults to factory_address).
        """
        key = (chain_id, factory_address)
        if key in self._entries:
            msg = f"Factory {factory_address} on chain {chain_id} is already registered."
            raise ValueError(msg)

        family = _derive_family(pool_class)
        variant = getattr(pool_class, "variant", None)
        kind = derive_kind(family, variant)

        self._entries[key] = _RegistryEntry(
            pool_class=pool_class,
            family=family,
            variant=variant,
            kind=kind,
            deployment=PoolDeploymentData(
                factory_address=factory_address,
                deployer=deployer if deployer is not None else factory_address,
                pool_init_hash=pool_init_hash,
            ),
        )

        # Update reverse index: kind → descriptor. When multiple deployments
        # share the same kind (e.g. SushiswapV2 on multiple chains), the last
        # registration wins.
        self._kind_index[kind] = PoolTypeDescriptor(
            family=family,
            variant=variant,
            kind=kind,
            factory=factory_address,
        )

    def set_default_v2_class(self, pool_class: type[AbstractLiquidityPool]) -> None:
        """Set the default V2 pool class when no factory-specific mapping exists."""
        self._default_v2_class = pool_class

    def set_default_v3_class(self, pool_class: type[AbstractLiquidityPool]) -> None:
        """Set the default V3 pool class when no factory-specific mapping exists."""
        self._default_v3_class = pool_class

    # --- Lookup ---

    def has_registration(self, chain_id: ChainId, factory_address: str) -> bool:
        """Whether a pool class is registered for (chain_id, factory)."""
        return (chain_id, factory_address) in self._entries

    def get_class(
        self, chain_id: ChainId, factory_address: str
    ) -> type[AbstractLiquidityPool] | None:
        """Get the pool class for (chain_id, factory).

        Returns None if no specific registration exists and no default is set.
        """
        entry = self._entries.get((chain_id, factory_address))
        if entry is not None:
            return entry.pool_class
        return None

    def get_v2_class(
        self, chain_id: ChainId, factory_address: str
    ) -> type[AbstractLiquidityPool] | None:
        """Get the V2 pool class for (chain_id, factory), with default fallback."""
        entry = self._entries.get((chain_id, factory_address))
        if entry is not None:
            return entry.pool_class
        return self._default_v2_class

    def get_v3_class(
        self, chain_id: ChainId, factory_address: str
    ) -> type[AbstractLiquidityPool] | None:
        """Get the V3 pool class for (chain_id, factory), with default fallback."""
        entry = self._entries.get((chain_id, factory_address))
        if entry is not None:
            return entry.pool_class
        return self._default_v3_class

    def get_descriptor(self, chain_id: ChainId, factory_address: str) -> PoolTypeDescriptor | None:
        """Get the PoolTypeDescriptor for (chain_id, factory)."""
        entry = self._entries.get((chain_id, factory_address))
        if entry is None:
            return None
        return PoolTypeDescriptor(
            family=entry.family,
            variant=entry.variant,
            kind=entry.kind,
            factory=factory_address,
        )

    def get_deployment(self, chain_id: ChainId, factory_address: str) -> PoolDeploymentData | None:
        """Get the deployment data for (chain_id, factory)."""
        entry = self._entries.get((chain_id, factory_address))
        if entry is None:
            return None
        return entry.deployment

    def get_descriptor_by_kind(self, kind: str) -> PoolTypeDescriptor | None:
        """Get a PoolTypeDescriptor by its kind string.

        Used for DB lookups where the kind is known but the factory
        address is not. When multiple deployments share a kind, returns
        the descriptor from the last registration.
        """
        return self._kind_index.get(kind)

    # --- Introspection ---

    @property
    def registrations(
        self,
    ) -> dict[
        tuple[ChainId, str],
        tuple[type[AbstractLiquidityPool], PoolTypeDescriptor, PoolDeploymentData],
    ]:
        """Return a copy of all registrations."""
        return {
            key: (entry.pool_class, entry.descriptor, entry.deployment)
            for key, entry in self._entries.items()
        }


@dataclass(frozen=True)
class _RegistryEntry:
    """Internal storage for a single registration."""

    pool_class: type[AbstractLiquidityPool]
    family: PoolFamily
    variant: str | None
    kind: str
    deployment: PoolDeploymentData

    @property
    def descriptor(self) -> PoolTypeDescriptor:
        return PoolTypeDescriptor(
            family=self.family,
            variant=self.variant,
            kind=self.kind,
            factory=self.deployment.factory_address,
        )


# Module-level singleton
pool_type_registry = PoolTypeRegistry()
