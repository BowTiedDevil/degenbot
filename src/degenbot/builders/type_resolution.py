"""Shared type resolution for pool construction.

Functions extracted from Bot and AsyncBot so that pool type resolution
logic (DB lookup, factory fetch, on-chain probing, class dispatch) is
defined once and used by both sync and async paths.

Pure functions (no I/O):
- pool_class_for_descriptor()
- _build_descriptor_from_db_result()
- _descriptor_from_probing_result()

Sync functions (accept PoolIO):
- fetch_factory_from_chain()
- resolve_pool_type_by_probing()
- resolve_pool_type()

Async functions (accept AsyncPoolIO):
- fetch_factory_from_chain_async()
- resolve_pool_type_by_probing_async()
- resolve_pool_type_async()
"""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, cast

import eth_abi.abi
from eth_abi.exceptions import DecodingError
from sqlalchemy import select
from web3.exceptions import Web3Exception

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.database.models.pools import LiquidityPoolTable
from degenbot.exceptions.base import DegenbotValueError
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.pool_type import PoolFamily, PoolTypeDescriptor, derive_kind
from degenbot.uniswap.liquidity_pool import LiquidityPool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.builders.pool_io import AsyncPoolIOProtocol, PoolIO
    from degenbot.database.session_manager import DatabaseSessionManager
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


def pool_class_for_descriptor(
    pool_type: PoolTypeDescriptor,
    *,
    chain_id: ChainId,
) -> type[AbstractLiquidityPool]:
    """Resolve a PoolTypeDescriptor to a concrete pool class.

    Consults the pool_type_registry to find the registered class
    for this factory on this chain. Falls back to a default class
    based on the family if no specific registration exists.

    Returns:
        The computed value.

    Raises:
        DegenbotValueError: If the operation fails.

    """
    if pool_type.factory is not None:
        pool_class = pool_type_registry.get_class(chain_id, pool_type.factory)
        if pool_class is not None:
            return pool_class

    # Default classes when no factory-specific registration exists
    match pool_type.family:
        case PoolFamily.CONSTANT_PRODUCT:
            return cast(
                "type[AbstractLiquidityPool]",
                pool_type_registry.get_v2_class(chain_id, pool_type.factory or "") or LiquidityPool,
            )
        case PoolFamily.CONCENTRATED_LIQUIDITY:
            return cast(
                "type[AbstractLiquidityPool]",
                pool_type_registry.get_v3_class(chain_id, pool_type.factory or "") or UniswapV3Pool,
            )
        case PoolFamily.STABLESWAP:
            # Variant-aware: reject Balancer stable pools without factory registration
            if pool_type.variant is not None and pool_type.variant.startswith("balancer"):
                msg = (
                    f"Balancer stable pool with unregistered factory {pool_type.factory}. "
                    f"Register the factory address in pool_type_registry first."
                )
                raise DegenbotValueError(message=msg)
            return CurveStableswapPool
        case PoolFamily.WEIGHTED:
            # No default Balancer weighted class — require factory registration
            if pool_type.variant is not None and pool_type.variant.startswith("balancer"):
                msg = (
                    f"Balancer weighted pool with unregistered factory {pool_type.factory}. "
                    f"Register the factory address in pool_type_registry first."
                )
                raise DegenbotValueError(message=msg)
            msg = f"No pool class for WEIGHTED family with variant {pool_type.variant!r}"
            raise DegenbotValueError(message=msg)
        case _:
            msg = f"No pool class for family {pool_type.family.value!r}"
            raise DegenbotValueError(message=msg)


def _build_descriptor_from_db_result(
    pool_from_db: LiquidityPoolTable,
) -> PoolTypeDescriptor | None:
    """Map a DB row to a PoolTypeDescriptor.

    Returns None if the kind can't be resolved from the registry.
    Read-only dependency on pool_type_registry.

    Returns:
        The computed value.

    """
    kind = pool_from_db.kind
    descriptor = pool_type_registry.get_descriptor_by_kind(kind)
    if descriptor is not None:
        return PoolTypeDescriptor(
            family=descriptor.family,
            variant=descriptor.variant,
            kind=descriptor.kind,
            factory=get_checksum_address(pool_from_db.exchange.factory),
        )
    return None


def _descriptor_from_probing_result(
    *,
    succeeded_method: str | None,
    chain_id: ChainId,
    factory: ChecksumAddress,
) -> PoolTypeDescriptor:
    """Map 'which method succeeded' to a PoolTypeDescriptor.

    If the factory is registered in pool_type_registry, uses the registry
    descriptor. Otherwise derives a default descriptor from the method name.
    If succeeded_method is None (no method succeeded), returns STABLESWAP.

    Returns:
        The computed value.

    """
    if succeeded_method is None:
        return PoolTypeDescriptor(
            family=PoolFamily.STABLESWAP,
            variant=None,
            kind=derive_kind(PoolFamily.STABLESWAP, None),
            factory=factory,
        )

    registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
    if registry_descriptor is not None:
        return registry_descriptor

    match succeeded_method:
        case "slot0":
            family = PoolFamily.CONCENTRATED_LIQUIDITY
        case "getReserves":
            family = PoolFamily.CONSTANT_PRODUCT
        case _:
            family = PoolFamily.STABLESWAP

    return PoolTypeDescriptor(
        family=family,
        variant=None,
        kind=derive_kind(family, None),
        factory=factory,
    )


def fetch_factory_from_chain(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,  # noqa: ARG001 — kept for API consistency with resolve_pool_type
    io: PoolIO,
) -> ChecksumAddress | None:
    """Fetch the factory address from the pool contract's factory() method.

    When ``io`` is a Rust :class:`~degenbot.degenbot_rs.PyBotIo` (the Bot's
    build-path adapter — ADR-005 slice 14a), the encode -> call -> decode ->
    checksum choreography is delegated to ``PyBotIo.fetch_factory_address``
    (Rust, slice 14b), not run in Python. Raw ``SyncPoolIO`` / ``AsyncPoolIO``
    callers (e.g. offline / fork tests) still exercise the Python implementation
    below — a behavior-preserving parity gate against the Rust impl.

    Returns:
        The computed value.

    """
    # ADR-005 slice 14b: route through PyBotIo when available — the
    # choreography is now Rust-owned. `hasattr` keeps the SyncPoolIO fallback
    # path working without importing PyBotIo (which lives in the Rust ext).
    fetch_factory = getattr(io, "fetch_factory_address", None)
    if fetch_factory is not None:
        return fetch_factory(address)

    try:
        factory_result = io.call(
            to=address,
            data=encode_function_calldata("factory()", None),
        )
        (factory_raw,) = eth_abi.abi.decode(types=["address"], data=factory_result)
        return get_checksum_address(factory_raw)
    except (Web3Exception, DecodingError):
        return None


async def fetch_factory_from_chain_async(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,  # noqa: ARG001 — kept for API consistency with resolve_pool_type_async
    io: AsyncPoolIOProtocol,
) -> ChecksumAddress | None:
    """Fetch the factory address from the pool contract's factory() method (async).

    Returns:
        The computed value.

    """
    try:
        factory_result = await io.call(
            to=address,
            data=encode_function_calldata("factory()", None),
        )
        (factory_raw,) = eth_abi.abi.decode(types=["address"], data=factory_result)
        return get_checksum_address(factory_raw)
    except (Web3Exception, DecodingError):
        return None


def resolve_pool_type_by_probing(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,
    factory: ChecksumAddress,
    io: PoolIO,
) -> PoolTypeDescriptor:
    """Determine pool type by probing the contract on-chain.

    Tries V3 methods first (slot0), then V2 methods (getReserves),
    then falls back to STABLESWAP. Descriptor construction is
    delegated to _descriptor_from_probing_result.

    Returns:
        The computed value.

    """
    # ADR-005 slice 14i: when io is a PyBotIo (Bot's build path), delegate
    # the 4-call probing choreography to Rust. SyncPoolIO fallback keeps the
    # Python implementation as a parity gate.
    probe_pool_type = getattr(io, "probe_pool_type", None)
    if probe_pool_type is not None:
        result = probe_pool_type(address)
        if result == "slot0":
            return _descriptor_from_probing_result(
                succeeded_method="slot0",
                chain_id=chain_id,
                factory=factory,
            )
        if result == "getReserves":
            return _descriptor_from_probing_result(
                succeeded_method="getReserves",
                chain_id=chain_id,
                factory=factory,
            )
        if result == "balancer_weighted":
            return PoolTypeDescriptor(
                family=PoolFamily.WEIGHTED,
                variant="balancer_weighted",
                kind=derive_kind(PoolFamily.WEIGHTED, "balancer_weighted"),
                factory=factory,
            )
        if result == "balancer_stable":
            return PoolTypeDescriptor(
                family=PoolFamily.STABLESWAP,
                variant="balancer_stable",
                kind=derive_kind(PoolFamily.STABLESWAP, "balancer_stable"),
                factory=factory,
            )
        # "stableswap" fallback.
        return _descriptor_from_probing_result(
            succeeded_method=None,
            chain_id=chain_id,
            factory=factory,
        )

    # Try V3: slot0() exists → CONCENTRATED_LIQUIDITY
    try:
        io.call(
            to=address,
            data=encode_function_calldata("slot0()", None),
        )
    except Web3Exception:
        pass
    else:
        return _descriptor_from_probing_result(
            succeeded_method="slot0",
            chain_id=chain_id,
            factory=factory,
        )

    # Try V2: getReserves() exists → CONSTANT_PRODUCT
    try:
        io.call(
            to=address,
            data=encode_function_calldata("getReserves()", None),
        )
    except Web3Exception:
        pass
    else:
        return _descriptor_from_probing_result(
            succeeded_method="getReserves",
            chain_id=chain_id,
            factory=factory,
        )

    # Try Balancer: getPoolId() exists → Balancer pool
    try:
        io.call(
            to=address,
            data=encode_function_calldata("getPoolId()", None),
        )
    except Web3Exception:
        pass
    else:
        # Balancer pool detected — determine weighted vs stable
        try:
            io.call(
                to=address,
                data=encode_function_calldata("getNormalizedWeights()", None),
            )
            variant = "balancer_weighted"
            family = PoolFamily.WEIGHTED
        except Web3Exception:
            variant = "balancer_stable"
            family = PoolFamily.STABLESWAP

        return PoolTypeDescriptor(
            family=family,
            variant=variant,
            kind=derive_kind(family, variant),
            factory=factory,
        )

    # Fall through to Curve — assume STABLESWAP if nothing else matched
    return _descriptor_from_probing_result(
        succeeded_method=None,
        chain_id=chain_id,
        factory=factory,
    )


async def resolve_pool_type_by_probing_async(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,
    factory: ChecksumAddress,
    io: AsyncPoolIOProtocol,
) -> PoolTypeDescriptor:
    """Determine pool type by probing the contract on-chain (async).

    Returns:
        The computed value.

    """
    # Try V3: slot0() exists → CONCENTRATED_LIQUIDITY
    try:
        await io.call(
            to=address,
            data=encode_function_calldata("slot0()", None),
        )
    except Web3Exception:
        pass
    else:
        return _descriptor_from_probing_result(
            succeeded_method="slot0",
            chain_id=chain_id,
            factory=factory,
        )

    # Try V2: getReserves() exists → CONSTANT_PRODUCT
    try:
        await io.call(
            to=address,
            data=encode_function_calldata("getReserves()", None),
        )
    except Web3Exception:
        pass
    else:
        return _descriptor_from_probing_result(
            succeeded_method="getReserves",
            chain_id=chain_id,
            factory=factory,
        )

    # Try Balancer: getPoolId() exists → Balancer pool
    try:
        await io.call(
            to=address,
            data=encode_function_calldata("getPoolId()", None),
        )
    except Web3Exception:
        pass
    else:
        # Balancer pool detected — determine weighted vs stable
        try:
            await io.call(
                to=address,
                data=encode_function_calldata("getNormalizedWeights()", None),
            )
            variant = "balancer_weighted"
            family = PoolFamily.WEIGHTED
        except Web3Exception:
            variant = "balancer_stable"
            family = PoolFamily.STABLESWAP

        return PoolTypeDescriptor(
            family=family,
            variant=variant,
            kind=derive_kind(family, variant),
            factory=factory,
        )

    # Fall through — assume STABLESWAP
    return _descriptor_from_probing_result(
        succeeded_method=None,
        chain_id=chain_id,
        factory=factory,
    )


def resolve_pool_type(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,
    io: PoolIO,
    db: DatabaseSessionManager,
) -> PoolTypeDescriptor:
    """Resolve the pool type for the given address.

    Consults these sources in order:
    1. Database `kind` column (exact polymorphic type)
    2. PoolTypeRegistry registration (factory address → descriptor)
    3. On-chain probing (slot0 vs getReserves) when factory is unknown

    Raises DegenbotValueError if the type cannot be determined.

    Returns:
        The computed value.

    Raises:
        DegenbotValueError: If the operation fails.

    """
    # Step 1: DB lookup — the `kind` column is the most direct signal
    with contextlib.suppress(Exception), db() as session:
        pool_from_db = session.scalar(
            select(LiquidityPoolTable).where(
                LiquidityPoolTable.address == address,
                LiquidityPoolTable.chain == chain_id,
            ),
        )
        if pool_from_db is not None:
            descriptor = _build_descriptor_from_db_result(pool_from_db)
            if descriptor is not None:
                return descriptor

    # Step 2: Factory address lookup via PoolTypeRegistry
    factory = fetch_factory_from_chain(address, chain_id=chain_id, io=io)
    if factory is not None:
        # Check if the factory is registered in the pool type registry
        registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
        if registry_descriptor is not None:
            return registry_descriptor

        # Step 3: No registry match — probe the contract to determine invariant
        return resolve_pool_type_by_probing(address, chain_id=chain_id, factory=factory, io=io)

    raise DegenbotValueError(
        message=(
            f"Cannot resolve pool type for address {address} on chain {chain_id}. "
            f"The factory() call failed and no database entry exists."
        ),
    )


async def resolve_pool_type_async(
    address: ChecksumAddress,
    *,
    chain_id: ChainId,
    io: AsyncPoolIOProtocol,
    db: DatabaseSessionManager,
) -> PoolTypeDescriptor:
    """Resolve the pool type for the given address (async).

    Same logic as resolve_pool_type but uses await for I/O calls.

    Returns:
        The computed value.

    Raises:
        DegenbotValueError: If the operation fails.

    """
    # Step 1: DB lookup — the `kind` column is the most direct signal
    with contextlib.suppress(Exception), db() as session:
        pool_from_db = session.scalar(
            select(LiquidityPoolTable).where(
                LiquidityPoolTable.address == address,
                LiquidityPoolTable.chain == chain_id,
            ),
        )
        if pool_from_db is not None:
            descriptor = _build_descriptor_from_db_result(pool_from_db)
            if descriptor is not None:
                return descriptor

    # Step 2: Factory address lookup via PoolTypeRegistry
    factory = await fetch_factory_from_chain_async(address, chain_id=chain_id, io=io)
    if factory is not None:
        registry_descriptor = pool_type_registry.get_descriptor(chain_id, factory)
        if registry_descriptor is not None:
            return registry_descriptor

        # Step 3: No registry match — probe the contract
        return await resolve_pool_type_by_probing_async(
            address,
            chain_id=chain_id,
            factory=factory,
            io=io,
        )

    raise DegenbotValueError(
        message=(
            f"Cannot resolve pool type for address {address} on chain {chain_id}. "
            f"The factory() call failed and no database entry exists."
        ),
    )
