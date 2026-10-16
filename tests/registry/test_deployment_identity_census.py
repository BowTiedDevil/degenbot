"""Deployment identity census: registrations must agree with their data.

The registry derives (family, variant, kind) from the registered class unless a
record overrides it — and some records MUST override (SushiSwap V3 is
byte-identical to Uniswap V3 in layout, so both are carried by UniswapV3Pool).
This census pins that sharing as deliberate: a layout-deviant factory silently
mapped onto the vanilla class is exactly how a pool ends up priced with the
wrong fee model, so every registration's derived identity must be internally
consistent and the shared classes must be enumerated, not accidental.
"""

from __future__ import annotations

from degenbot.checksum_cache import get_checksum_address
from degenbot.pancakeswap.pools import PancakeswapV3Pool
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.pool_type import derive_kind
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


def test_every_registration_carries_a_consistent_identity() -> None:
    registrations = pool_type_registry.registrations
    assert registrations, "the deployment census is empty — nothing is pinned"
    for (chain_id, factory), (_cls, descriptor, deployment) in registrations.items():
        assert deployment is not None
        assert descriptor.factory == get_checksum_address(factory)
        assert descriptor.kind == derive_kind(descriptor.family, descriptor.variant), (
            f"registration for chain {chain_id} factory {factory} derives "
            f"{derive_kind(descriptor.family, descriptor.variant)!r} "
            f"but carries kind {descriptor.kind!r}"
        )


def test_shared_pool_classes_are_deliberate_and_variant_tagged() -> None:
    kinds_by_class: dict[str, set[str]] = {}
    for (_chain, _factory), (
        pool_class,
        descriptor,
        _deployment,
    ) in pool_type_registry.registrations.items():
        kinds_by_class.setdefault(pool_class.__name__, set()).add(descriptor.kind)
    # SushiSwap V3 shares UniswapV3Pool BY DESIGN (identical slot0/tick layout);
    # PancakeSwap V3 deviates and must never share the vanilla class.
    assert kinds_by_class.get("UniswapV3Pool") == {"uniswap_v3", "sushiswap_v3"}
    assert kinds_by_class.get("PancakeswapV3Pool") == {"pancakeswap_v3"}
    assert "pancakeswap_v3" not in kinds_by_class.get("UniswapV3Pool", set())
