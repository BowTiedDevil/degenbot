"""Register DEX deployment records into the pool-type registry.

Companion layer (ADR-005): the data itself is read by
:mod:`degenbot.registry.deployment_records` (a leaf module — it knows the
JSON schema and the valid ``pool_type`` keys but resolves nothing to Python
classes). This module owns the ``pool_type`` → Python class map, resolves
the JSON string keys (``pool_type`` → class, ``dex_variant`` →
``DexIdentity`` preset, ``family`` string → ``PoolFamily`` enum), and drives
the registry's low-level ``register()``.

The shipped ``deployments.json`` is the single source of deployment data
(``chain_id``, ``factory`` → ``deployer`` / ``init_hash`` / ``variant`` /
``dex_identity`` preset). A user overlay path may be declared in
``config.toml`` under ``[deployments]`` → ``overlay``; entries there
override shipped entries on ``(chain_id, factory)`` conflict (user wins).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.aerodrome.pools import AerodromeV2Pool, AerodromeV3Pool
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.pancakeswap.pools import PancakeswapV3Pool
from degenbot.registry.deployment_records import (
    DeploymentRecord,
    load_deployments,
    load_json_deployments,
)
from degenbot.registry.pool_type import PoolRegistration
from degenbot.types import dex_identity
from degenbot.types.pool_type import PoolFamily
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from degenbot.registry.pool_type import PoolTypeRegistry
    from degenbot.types import DexIdentity
    from degenbot.types.abstract import AbstractLiquidityPool

__all__ = [
    "POOL_TYPE_MAP",
    "DeploymentRecord",
    "load_deployments",
    "load_json_deployments",
    "register_from_deployments",
]

# ``pool_type`` string → Python companion class. This map lives in the loader
# (companion layer, ADR-005) — the JSON carries only the string key, so the
# data file stays free of Python-class coupling and a standalone-Rust consumer
# reading the same JSON would carry its own (Rust-side) pool_type → enum map.
# The keys must cover exactly the leaf's ``KNOWN_POOL_TYPES``; the alignment
# is pinned by tests/registry/test_deployment_loader.py.
POOL_TYPE_MAP: dict[str, type[AbstractLiquidityPool]] = {
    "uniswap-v2": UniswapV2Pool,
    "uniswap-v3": UniswapV3Pool,
    "pancakeswap-v3": PancakeswapV3Pool,
    "sushiswap-v3": UniswapV3Pool,
    "aerodrome-v2": AerodromeV2Pool,
    "aerodrome-v3": AerodromeV3Pool,
    "balancer-weighted": BalancerV2Pool,
    "balancer-stable": BalancerV2StablePool,
}


def _pool_type_map() -> dict[str, type[AbstractLiquidityPool]]:
    """Return the pool_type → class map.

    Returns:
        A mapping of pool type string to its Python pool class.

    """
    return POOL_TYPE_MAP


def _resolve_family(family_str: str | None) -> PoolFamily | None:
    """Resolve a JSON family string to a :class:`PoolFamily` enum value.

    Returns:
        The matching ``PoolFamily``, or ``None`` when ``family_str`` is
        ``None`` (auto-derive at register time via ``_derive_family``).
        The string must match a ``PoolFamily`` enum value (``"weighted"``,
        ``"stableswap"``, …).

    """
    if family_str is None:
        return None
    return PoolFamily(family_str)


def register_from_deployments(records: list[DeploymentRecord], registry: PoolTypeRegistry) -> None:
    """Register deployment records into a :class:`PoolTypeRegistry`.

    Companion-layer orchestration (ADR-005): resolves the JSON string keys
    (``pool_type`` → Python class, ``dex_variant`` → ``DexIdentity`` preset,
    ``family`` string → ``PoolFamily`` enum) and calls the registry's
    low-level ``register()`` primitive for each record.

    The ``dex_variant`` preset is resolved via
    :func:`~degenbot._ffi.dex_identity` and asserted non-None — a
    preset string in the JSON must resolve, otherwise the deployment data is
    inconsistent with the compiled ``DexIdentity`` presets (Rust-side).

    Args:
        records: The deployment records (typically from :func:`load_deployments`).
        registry: The target registry (e.g. the ``pool_type_registry`` singleton).

    """
    for record in records:
        pool_class = POOL_TYPE_MAP[record.pool_type]
        family = _resolve_family(record.family)
        if record.dex_variant is not None:
            identity: DexIdentity | None = dex_identity(record.dex_variant)
            assert identity is not None, (
                f"dex_variant {record.dex_variant!r} for "
                f"{record.name} (chain {record.chain_id}, {record.factory}) "
                f"did not resolve to a DexIdentity preset"
            )
        else:
            identity = None
        init_hash = record.init_hash or None
        registry.register(
            PoolRegistration(
                pool_class=pool_class,
                chain_id=record.chain_id,
                factory_address=record.factory,
                pool_init_hash=init_hash,
                deployer=record.deployer,
                family=family,
                variant=record.variant,
                dex_identity=identity,
                implementation_address=record.implementation_address,
            )
        )
