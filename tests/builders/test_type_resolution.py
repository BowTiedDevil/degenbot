"""Tests for the shared type resolution module.

Pool identity resolution is core-owned (the ONE ladder in `degenbot-bot`'s
`pool_identity`); what this module holds is the companion layer's own
decision — which Python pool class a resolved identity selects — plus the
pure projection of the core's structured identity onto the companion's
descriptor. These tests pin exactly that, separately from the core identity
resolution (whose arms are `degenbot-bot`'s own tests) and separately from
the seam projection (`tests/builders/test_pybot_io.py`).
"""

from __future__ import annotations

from types import SimpleNamespace

import pytest

from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.builders.type_resolution import (
    descriptor_for_identity,
    pool_class_for_descriptor,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.exceptions.base import DegenbotValueError
from degenbot.types.pool_type import PoolFamily, PoolTypeDescriptor
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

CHAIN_ID = 1


class TestDescriptorForIdentity:
    """The pure projection of the core's structured identity.

    A `PoolIdentity` pyclass double (duck-typed: species/family/variant/
    factory/persisted_kind) — the projection reads exactly those fields and
    derives nothing.
    """

    def test_projects_the_structured_identity(self) -> None:
        identity = SimpleNamespace(
            species="sushiswap_v3",
            family="concentrated_liquidity",
            variant="sushiswap",
            factory="0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F",
            persisted_kind="sushiswap_v3",
        )
        descriptor = descriptor_for_identity(identity)  # type: ignore[arg-type]
        assert descriptor == PoolTypeDescriptor(
            family=PoolFamily.CONCENTRATED_LIQUIDITY,
            variant="sushiswap",
            kind="sushiswap_v3",
            factory=get_checksum_address("0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"),
        )

    def test_projects_a_factory_less_identity(self) -> None:
        identity = SimpleNamespace(
            species=None,
            family="stableswap",
            variant=None,
            factory=None,
            persisted_kind="stableswap",
        )
        descriptor = descriptor_for_identity(identity)  # type: ignore[arg-type]
        assert descriptor == PoolTypeDescriptor(
            family=PoolFamily.STABLESWAP,
            variant=None,
            kind="stableswap",
            factory=None,
        )

    def test_projects_the_unmodeled_refusal_identity(self) -> None:
        # The refusal-only identity survives the projection under its own name
        # — it exists so the construction switchboard refuses it there, and no
        # class lookup is ever asked for it.
        identity = SimpleNamespace(
            species=None,
            family="unmodeled",
            variant="balancer_unmodeled",
            factory=None,
            persisted_kind="balancer_unmodeled",
        )
        descriptor = descriptor_for_identity(identity)  # type: ignore[arg-type]
        assert descriptor.family is PoolFamily.UNMODELED
        assert descriptor.kind == "balancer_unmodeled"
        with pytest.raises(DegenbotValueError, match="No pool class for family 'unmodeled'"):
            pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)


class TestPoolClassForDescriptor:
    """Tests for the pure pool_class_for_descriptor function."""

    def test_constant_product_returns_v2_class(self) -> None:
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.CONSTANT_PRODUCT,
            variant=None,
            kind="uniswap_v2",
            factory=None,
        )
        result = pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)
        assert result is UniswapV2Pool

    def test_concentrated_liquidity_returns_v3_class(self) -> None:
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.CONCENTRATED_LIQUIDITY,
            variant=None,
            kind="uniswap_v3",
            factory=None,
        )
        result = pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)
        assert result is UniswapV3Pool

    def test_unknown_family_raises(self) -> None:
        # PoolFamily.WEIGHTED has no default class registered
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.WEIGHTED,
            variant=None,
            kind="weighted",
            factory=None,
        )
        with pytest.raises(DegenbotValueError, match="No pool class for WEIGHTED"):
            pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)

    def test_stableswap_returns_curve_class(self) -> None:
        """A positive Curve identity (variant=None) keeps its default class."""
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.STABLESWAP,
            variant=None,
            kind="stableswap",
            factory=None,
        )
        result = pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)
        assert result is CurveStableswapPool

    def test_stableswap_balancer_variant_without_registered_factory_is_balancer(self) -> None:
        """A Balancer identity never falls back to CurveStableswapPool.

        The variant is authoritative (probed on-chain or read from the DB kind);
        the factory only refines it to one deployment, so an unregistered
        revision (the MetaStablePool factory is not a registry row) still
        resolves to the Balancer stable class.
        """
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.STABLESWAP,
            variant="balancer_stable",
            kind="balancer_stable",
            factory=None,
        )
        assert pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID) is BalancerV2StablePool

    def test_weighted_balancer_variant_without_registered_factory_is_balancer(self) -> None:
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.WEIGHTED,
            variant="balancer_weighted",
            kind="balancer_weighted",
            factory=None,
        )
        assert pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID) is BalancerV2Pool

    def test_stableswap_unrecognized_variant_with_unregistered_factory_raises(self) -> None:
        """An unrecognized stableswap variant raises instead of silently
        defaulting to CurveStableswapPool — classify or raise, never guess."""
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.STABLESWAP,
            variant="mycustom",
            kind="mycustom",
            factory=None,
        )
        with pytest.raises(DegenbotValueError, match="Unrecognized stableswap variant"):
            pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)

    def test_unmodeled_balancer_family_has_no_class(self) -> None:
        """The unmodeled family exists to reach the construction switchboard,
        which refuses it under its own name — no wrapper class can exist, so the
        lookup refuses loudly instead of guessing a family's class."""
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.UNMODELED,
            variant="balancer_unmodeled",
            kind="balancer_unmodeled",
            factory=None,
        )
        with pytest.raises(DegenbotValueError, match="No pool class for family 'unmodeled'"):
            pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)

    def test_registered_factory_returns_registered_class(self) -> None:
        """When the descriptor has a registered factory, returns the registered class."""
        factory = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.CONSTANT_PRODUCT,
            variant=None,
            kind="uniswap_v2",
            factory=factory,  # type: ignore[arg-type]
        )
        result = pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)
        assert issubclass(result, UniswapV2Pool)
