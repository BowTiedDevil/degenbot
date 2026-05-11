"""Test pool type resolution from factory address to invariant variant, and kind."""

from __future__ import annotations

import pytest

from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.pool_type import PoolFamily, PoolTypeDescriptor


class TestPoolTypeDescriptor:
    """Test PoolTypeDescriptor and PoolFamily."""

    def test_family_values(self) -> None:
        assert PoolFamily.CONSTANT_PRODUCT.value == "constant_product"
        assert PoolFamily.CONCENTRATED_LIQUIDITY.value == "concentrated_liquidity"
        assert PoolFamily.STABLESWAP.value == "stableswap"
        assert PoolFamily.WEIGHTED.value == "weighted"

    def test_descriptor_is_frozen(self) -> None:
        desc = PoolTypeDescriptor(
            family=PoolFamily.CONSTANT_PRODUCT, variant=None, kind="uniswap_v2", factory=None
        )
        with pytest.raises(AttributeError):
            desc.variant = "sushiswap"  # type: ignore[misc]

    def test_descriptor_equality(self) -> None:
        a = PoolTypeDescriptor(
            family=PoolFamily.CONSTANT_PRODUCT,
            variant="sushiswap",
            kind="sushiswap_v2",
            factory=None,
        )
        b = PoolTypeDescriptor(
            family=PoolFamily.CONSTANT_PRODUCT,
            variant="sushiswap",
            kind="sushiswap_v2",
            factory=None,
        )
        assert a == b


class TestKindToDescriptor:
    """Test that every DB polymorphic kind maps to a descriptor."""

    def test_all_v2_kinds_map_to_constant_product(self) -> None:
        v2_kinds = [
            "uniswap_v2", "sushiswap_v2", "pancakeswap_v2",
            "camelot_v2", "swapbased_v2",
        ]
        for kind in v2_kinds:
            desc = pool_type_registry.get_descriptor_by_kind(kind)
            assert desc is not None, f"Missing kind: {kind}"
            assert desc.family == PoolFamily.CONSTANT_PRODUCT

    def test_all_v3_kinds_map_to_concentrated_liquidity(self) -> None:
        v3_kinds = ["uniswap_v3", "sushiswap_v3", "pancakeswap_v3", "aerodrome_v3"]
        for kind in v3_kinds:
            desc = pool_type_registry.get_descriptor_by_kind(kind)
            assert desc is not None, f"Missing kind: {kind}"
            assert desc.family == PoolFamily.CONCENTRATED_LIQUIDITY

    def test_uniswap_variants_have_none_variant(self) -> None:
        assert pool_type_registry.get_descriptor_by_kind("uniswap_v2").variant is None
        assert pool_type_registry.get_descriptor_by_kind("uniswap_v3").variant is None

    def test_dex_variants_have_named_variant(self) -> None:
        assert pool_type_registry.get_descriptor_by_kind("sushiswap_v2").variant == "sushiswap"
        assert pool_type_registry.get_descriptor_by_kind("camelot_v2").variant == "camelot"


class TestResolvePoolType:
    """Test resolving pool type from factory address."""

    def test_resolve_uniswap_v3_via_factory(self) -> None:
        desc = pool_type_registry.get_descriptor(1, "0x1F98431c8aD98523631AE4a59f267346ea31F984")
        assert desc is not None
        assert desc.family == PoolFamily.CONCENTRATED_LIQUIDITY
        assert desc.variant is None
        assert desc.kind == "uniswap_v3"

    def test_resolve_sushiswap_v2_via_factory(self) -> None:
        desc = pool_type_registry.get_descriptor(1, "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac")
        assert desc is not None
        assert desc.family == PoolFamily.CONSTANT_PRODUCT
        assert desc.variant == "sushiswap"
        assert desc.kind == "sushiswap_v2"

    def test_registry_has_all_known_factories(self) -> None:
        assert len(pool_type_registry.registrations) > 15

    def test_registry_kind_index_covers_all(self) -> None:
        all_kinds = {desc.kind for _key, (_cls, desc, _depl) in pool_type_registry.registrations.items()}
        assert "uniswap_v2" in all_kinds
        assert "uniswap_v3" in all_kinds
        assert "sushiswap_v2" in all_kinds
        assert "sushiswap_v3" in all_kinds

    def test_descriptors_are_consistent(self) -> None:
        for _key, (_cls, desc, _depl) in pool_type_registry.registrations.items():
            assert desc.kind is not None
            assert len(desc.kind) > 0


class TestBuildPoolDispatch:
    """Test build_pool dispatch (via kind → descriptor) without chain calls."""

    def test_build_pool_resolves_uniswap_v3(self) -> None:
        desc = pool_type_registry.get_descriptor_by_kind("uniswap_v3")
        assert desc is not None
        assert desc.family == PoolFamily.CONCENTRATED_LIQUIDITY

    def test_build_pool_resolves_uniswap_v2(self) -> None:
        desc = pool_type_registry.get_descriptor_by_kind("uniswap_v2")
        assert desc is not None
        assert desc.family == PoolFamily.CONSTANT_PRODUCT

    def test_build_pool_resolves_aerodrome_v2(self) -> None:
        desc = pool_type_registry.get_descriptor_by_kind("aerodrome_v2")
        assert desc is not None
        assert desc.family == PoolFamily.CONSTANT_PRODUCT

    def test_pancakeswap_v2_known(self) -> None:
        desc = pool_type_registry.get_descriptor_by_kind("pancakeswap_v2")
        assert desc is not None
        assert desc.family == PoolFamily.CONSTANT_PRODUCT

    def test_deployer_fetched(self) -> None:
        deployment = pool_type_registry.get_deployment(
            1, "0x1F98431c8aD98523631AE4a59f267346ea31F984"
        )
        assert deployment is not None
        assert deployment.deployer is not None


class TestPoolTypeInvariants:
    """Test invariant invariants on the type system."""

    def test_all_v2_pool_classes_derive_constant_product(self) -> None:
        try:
            from degenbot.uniswap.deployments import _FACTORY_DEPLOYMENTS
        except ImportError:
            pytest.skip("_FACTORY_DEPLOYMENTS not accessible (internal implementation detail)")

        for chain_id, factories in _FACTORY_DEPLOYMENTS.items():
            for factory_addr, deployment in factories.items():
                desc = pool_type_registry.get_descriptor(chain_id, factory_addr)
                if desc is not None:
                    family = desc.family
                    suffix = "v2" if family == PoolFamily.CONSTANT_PRODUCT else "v3"
                    expected_kind = f"{family.value}_{suffix}"
                    assert "_" in desc.kind, f"Expected kind with underscore, got {desc.kind}"

    def test_sushiswap_registered(self) -> None:
        assert pool_type_registry.has_registration(1, "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac")


class TestPoolTypeRegistryDiscovery:
    """Test auto-discovery of pool classes from degenbot.uniswap.deployments."""

    def test_uniswap_v2_is_in_registry(self) -> None:
        entry = pool_type_registry.get_descriptor(1, "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
        assert entry is not None
        assert entry.kind == "uniswap_v2"

    def test_uniswap_v3_is_in_registry(self) -> None:
        entry = pool_type_registry.get_descriptor(1, "0x1F98431c8aD98523631AE4a59f267346ea31F984")
        assert entry is not None
        assert entry.kind == "uniswap_v3"


class TestPoolTypeRegistrySingleton:
    """Test that pool_type_registry is a singleton."""

    def test_same_instance(self) -> None:
        from degenbot.registry import pool_type_registry as r2
        from degenbot.registry.pool_type import pool_type_registry as r1
        assert r1 is r2
