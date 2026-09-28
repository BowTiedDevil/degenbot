"""Pool registry: the Python companion side of the session's pool identities.

Canonical identity for a session's pools is owned by the Rust
`SessionObjectRegistry`; these registries delegate to it. What lives here is
the *presentation* object: a `UniswapV2Pool` / `BalancerV2Pool` / … companion
wrapping live Rust state, which is not a session object. The split matters
because a companion is a Python value that `release_python_state` drops and a
later build re-mints, while the identity behind it is session-lifetime.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, overload

from degenbot.exceptions.base import DegenbotValueError
from degenbot.registry.session import CompanionCache, SessionObjects, pool_family_of
from degenbot.types.pool_protocols import ConcentratedLiquidityPool
from degenbot.utils.bytes import to_bytes

if TYPE_CHECKING:
    from collections.abc import Iterator

    from degenbot._ffi import Bot, SessionObject
    from degenbot.types.abstract import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId
    from degenbot.types.chain import ChecksummedAddress


type PoolId = bytes


class ManagedPoolRegistry:
    """V4 pool companions, keyed by the session's `(PoolManager, pool_id)` identities.

    A V4 pool is named by its pair, never by the `PoolManager` address alone,
    so every read here carries both. This is the registry `Bot.managed_pools`
    and the one `PoolRegistry` delegates its V4 branch to, so the two are one
    companion store for the session's V4 pools.
    """

    def __init__(self, *, py_bot: Bot) -> None:
        """Bind to the session that owns the V4 pool identities.

        Args:
            py_bot: The session's Rust `Bot` handle. Required: identity is the
                session's, so a registry cannot stand up without one.

        """
        self._session = SessionObjects(py_bot)
        self._companions: CompanionCache[ConcentratedLiquidityPool] = CompanionCache()

    def _handle(
        self,
        chain_id: ChainId,
        pool_manager_address: ChecksummedAddress,
        pool_id: PoolId,
    ) -> SessionObject | None:
        """Resolve the session object naming this V4 pool.

        Returns:
            The session object, or None when the session does not hold it.

        """
        return self._session.resolve_pool(
            chain_id=chain_id,
            family="v4",
            address=pool_manager_address,
            pool_id=to_bytes(pool_id),
        )

    def get(
        self,
        chain_id: ChainId,
        pool_manager_address: ChecksummedAddress,
        pool_id: PoolId,
    ) -> ConcentratedLiquidityPool | None:
        """Retrieve a V4 pool by chain, manager address, and pool ID.

        Returns:
            The registered V4 pool, or None if not found.

        """
        handle = self._handle(chain_id, pool_manager_address, pool_id)
        return None if handle is None else self._companions.resolve(handle)

    def add(
        self,
        pool: ConcentratedLiquidityPool,
        chain_id: ChainId,
        pool_manager_address: ChecksummedAddress,
        pool_id: PoolId,
    ) -> None:
        """Register a V4 pool.

        Raises:
            DegenbotValueError: A companion is already registered for this
                identity.

        """
        handle = self._session.get_or_create_pool(
            chain_id=chain_id,
            family="v4",
            address=pool_manager_address,
            pool_id=to_bytes(pool_id),
        )
        if self._companions.resolve(handle) is not None:
            msg = f"ManagedPool is already registered at key {handle.key}"
            raise DegenbotValueError(message=msg)
        self._companions.store(handle, pool)

    def get_or_add(
        self,
        pool: ConcentratedLiquidityPool,
        chain_id: ChainId,
        pool_manager_address: ChecksummedAddress,
        pool_id: PoolId,
    ) -> ConcentratedLiquidityPool:
        """Idempotently register a V4 pool, returning the stored instance.

        If a concurrent registration worker already built this pool, return the
        canonical stored instance instead of raising — a
        distinct path sharing this pool is not lossily skipped.

        Returns:
            The stored pool instance (the existing canonical one on a duplicate).

        """
        handle = self._session.get_or_create_pool(
            chain_id=chain_id,
            family="v4",
            address=pool_manager_address,
            pool_id=to_bytes(pool_id),
        )
        return self._companions.get_or_store(handle, pool)

    def remove(
        self,
        chain_id: ChainId,
        pool_manager_address: ChecksummedAddress,
        pool_id: PoolId,
    ) -> None:
        """Drop the V4 companion for this identity.

        The session's identity for the pool is session-lifetime and stays; only
        the Python companion goes.
        """
        handle = self._handle(chain_id, pool_manager_address, pool_id)
        if handle is not None:
            self._companions.drop(handle)

    def list_all(self) -> Iterator[ConcentratedLiquidityPool]:
        """Yield every registered V4 pool.

        Yields:
            Each V4 pool companion filed in this registry.

        """
        for entry in self._companions.entries():
            yield entry.item

    def reset(self) -> None:
        """Drop every V4 companion. The session's V4 identities are untouched."""
        self._companions.clear()

    def __len__(self) -> int:
        """Count the registered V4 pools.

        Returns:
            The number of V4 pool companions filed.

        """
        return len(self._companions)


class PoolRegistry:
    """Address-keyed pool companions, delegating identity to the session registry.

    The non-V4 families are address-keyed, which is why reads here are
    family-agnostic: the session resolves the address to whichever family
    registered it first. Registering a pool, by contrast, names the family —
    read off the companion's live handle — so a V3 pool and a Balancer pool at
    one address stay two identities, as they are in the core.
    """

    def __init__(
        self,
        *,
        py_bot: Bot,
        managed_pool_registry: ManagedPoolRegistry | None = None,
    ) -> None:
        """Bind to the session that owns the pool identities.

        Args:
            py_bot: The session's Rust `Bot` handle. Required: identity is the
                session's, so a registry cannot stand up without one.
            managed_pool_registry: The V4 companion store to delegate to. Pass
                the session's own `ManagedPoolRegistry` so the V4 pools a build
                registers are the same companions this registry hands out.

        """
        self._session = SessionObjects(py_bot)
        self._py_bot = py_bot
        # `is None`, never `or`: an empty registry is falsy (`__len__` is 0),
        # so `or` would silently replace the session's V4 store with a fresh one.
        self._managed_pool_registry = (
            managed_pool_registry
            if managed_pool_registry is not None
            else ManagedPoolRegistry(py_bot=py_bot)
        )
        self._companions: CompanionCache[AbstractLiquidityPool] = CompanionCache()

    @overload
    def get(
        self,
        chain_id: ChainId,
        pool_address: ChecksummedAddress,
        pool_id: None = None,
    ) -> AbstractLiquidityPool | None: ...

    @overload
    def get(
        self,
        chain_id: ChainId,
        pool_address: ChecksummedAddress,
        pool_id: PoolId,
    ) -> ConcentratedLiquidityPool | None: ...

    def get(
        self,
        chain_id: ChainId,
        pool_address: ChecksummedAddress,
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
        handle = self._session.resolve_pool_by_address(chain_id=chain_id, address=pool_address)
        return None if handle is None else self._companions.resolve(handle)

    def add(
        self,
        pool: AbstractLiquidityPool,
        chain_id: ChainId,
        pool_address: ChecksummedAddress,
        pool_id: PoolId | None = None,
    ) -> None:
        """Register a pool.

        When pool_id is provided, the pool must satisfy the
        ConcentratedLiquidityPool protocol and is registered in the
        managed pool sub-registry. Otherwise, it is registered as a
        standard pool.

        Raises:
            TypeError: If pool_id is provided but pool does not satisfy ConcentratedLiquidityPool.
            DegenbotValueError: A companion is already registered for this identity.

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
            return
        handle = self._session.get_or_create_pool(
            chain_id=chain_id,
            family=pool_family_of(pool),
            address=pool_address,
        )
        if self._companions.resolve(handle) is not None:
            msg = f"Pool is already registered at key {handle.key}"
            raise DegenbotValueError(message=msg)
        self._companions.store(handle, pool)

    def get_or_add(
        self,
        pool: AbstractLiquidityPool,
        chain_id: ChainId,
        pool_address: ChecksummedAddress,
        pool_id: PoolId | None = None,
    ) -> AbstractLiquidityPool | ConcentratedLiquidityPool:
        """Idempotently register a pool, returning the stored instance.

        Used by the concurrent registration build path: if
        another worker already built this pool, return the canonical stored
        instance instead of raising, so a distinct path sharing the pool is not
        lossily skipped. Mirrors :meth:`add`'s managed/V4 dispatch.

        Returns:
            The stored pool instance (the existing canonical one on a duplicate).

        Raises:
            TypeError: If ``pool_id`` is provided but pool does not satisfy
                ConcentratedLiquidityPool.

        """
        if isinstance(pool_id, bytes):
            if not isinstance(pool, ConcentratedLiquidityPool):
                msg = "pool must satisfy ConcentratedLiquidityPool when pool_id is provided"
                raise TypeError(msg)
            return self._managed_pool_registry.get_or_add(
                pool=pool,
                chain_id=chain_id,
                pool_manager_address=pool_address,
                pool_id=pool_id,
            )
        handle = self._session.get_or_create_pool(
            chain_id=chain_id,
            family=pool_family_of(pool),
            address=pool_address,
        )
        return self._companions.get_or_store(handle, pool)

    @overload
    def remove(
        self,
        chain_id: ChainId,
        pool_address: ChecksummedAddress,
        pool_id: PoolId,
    ) -> None: ...

    @overload
    def remove(
        self,
        chain_id: ChainId,
        pool_address: ChecksummedAddress,
        pool_id: None = None,
    ) -> None: ...

    def remove(
        self,
        chain_id: ChainId,
        pool_address: ChecksummedAddress,
        pool_id: PoolId | None = None,
    ) -> None:
        """Remove a pool.

        For V2/V3 pools (``pool_id`` is ``None``), propagates to the Rust
        ``BotState`` via ``py_bot.unregister_pool`` so the Rust-owned state
        stays symmetric with the Python registry (ADR-007). V4 pools
        (``pool_id`` is bytes) are Python-only here — V4 unregister is
        engine-side (see ADR-007 Deferred).

        The removal is of the *companion* and the live pool state; the
        session's identity for the pool is session-lifetime and is not
        withdrawn, so a later build of the same address re-files a companion
        under the same canonical identity.
        """
        if isinstance(pool_id, bytes):
            self._managed_pool_registry.remove(
                chain_id=chain_id,
                pool_manager_address=pool_address,
                pool_id=pool_id,
            )
            return
        handle = self._session.resolve_pool_by_address(chain_id=chain_id, address=pool_address)
        # V2/V3 path: propagate to Rust before dropping the companion (the
        # Rust side is silent-on-miss, so ordering is safe).
        self._py_bot.unregister_pool(address=pool_address)
        if handle is not None:
            self._companions.drop(handle)

    def _reset(self, *, propagate_to_rust: bool = True) -> None:
        """Reset both the main registry and the managed pool registry.

        When ``propagate_to_rust`` is True (the default — end-of-life
        teardown), V2/V3 removal is propagated to the Rust ``BotState`` via
        ``py_bot.unregister_pool`` before clearing Python storage (ADR-007).
        V4 pools are Python-only (engine-side unregister is deferred).

        When ``propagate_to_rust`` is False (the mid-lifecycle
        ``release_python_state`` handoff), Rust keeps the pools — the live pump
        keeps writing V3 Mint/Burn/Swap through the shared ``BotState``, so the
        release must NOT unregister the very state it is handing canonical
        ownership to. Unregistering there stranded every live Mint/Burn (the
        pump routed them to the buffer because ``registered=false``) and
        dropped every Swap, freezing the tick map (the V3 desync in the
        permutation run). Only Python storage is dropped; Rust stays canonical.
        """
        if propagate_to_rust:
            # Only address-keyed families are filed here (the V4 branch
            # delegates), so this cannot unregister a PoolManager. The handle
            # carries the canonical address, so no key is re-derived.
            for entry in self._companions.entries():
                self._py_bot.unregister_pool(address=entry.handle.address)
        self.reset()

    def list_all(self) -> Iterator[AbstractLiquidityPool]:
        """Yield every registered address-keyed pool.

        Yields:
            Each address-keyed pool companion filed in this registry.

        """
        for entry in self._companions.entries():
            yield entry.item

    def reset(self) -> None:
        """Drop every companion, V4 included. The session's identities are untouched."""
        self._companions.clear()
        self._managed_pool_registry.reset()

    def __len__(self) -> int:
        """Count the registered address-keyed pools.

        Returns:
            The number of address-keyed pool companions filed; V4 pools, which
            the managed registry holds, are excluded.

        """
        return len(self._companions)
