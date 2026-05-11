"""
Tests for PoolTypeRegistry — the unified pool type registration system.

The registry replaces the scattered PoolClassRegistry, FACTORY_DEPLOYMENTS,
_KIND_TO_DESCRIPTOR, and _variant_from_class with a single registration
mechanism that auto-derives invariant, variant, and kind from the class
hierarchy and class attributes.
"""

import pytest

from degenbot.aerodrome.pools import AerodromeV3Pool
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.pancakeswap.pools import PancakeswapV2Pool, PancakeswapV3Pool
from degenbot.registry.pool_type import PoolTypeRegistry
from degenbot.sushiswap.pools import SushiswapV2Pool, SushiswapV3Pool
from degenbot.types.pool_type import PoolFamily
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

# --- Auto-derivation of kind from variant + invariant ---


class TestKindDerivation:
    """Test that kind strings are auto-derived from variant + invariant."""

    def test_canonical_v2_kind(self) -> None:
        """No variant → 'uniswap_v2' for CONSTANT_PRODUCT."""
        registry = PoolTypeRegistry()
        registry.register(
            UniswapV2Pool,
            chain_id=1,
            factory_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            pool_init_hash="0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
        )
        desc = registry.get_descriptor(
            chain_id=1, factory_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
        )
        assert desc is not None
        assert desc.kind == "uniswap_v2"

    def test_canonical_v3_kind(self) -> None:
        """No variant → 'uniswap_v3' for CONCENTRATED_LIQUIDITY."""
        registry = PoolTypeRegistry()
        registry.register(
            UniswapV3Pool,
            chain_id=1,
            factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        )
        desc = registry.get_descriptor(
            chain_id=1, factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984"
        )
        assert desc is not None
        assert desc.kind == "uniswap_v3"

    def test_variant_v2_kind(self) -> None:
        """variant='sushiswap' → kind='sushiswap_v2'."""
        registry = PoolTypeRegistry()
        registry.register(
            SushiswapV2Pool,
            chain_id=1,
            factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
            pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
        )
        desc = registry.get_descriptor(
            chain_id=1, factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"
        )
        assert desc is not None
        assert desc.kind == "sushiswap_v2"

    def test_variant_v3_kind(self) -> None:
        """variant='sushiswap' → kind='sushiswap_v3'."""
        registry = PoolTypeRegistry()
        registry.register(
            SushiswapV3Pool,
            chain_id=1,
            factory_address="0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F",
        )
        desc = registry.get_descriptor(
            chain_id=1, factory_address="0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"
        )
        assert desc is not None
        assert desc.kind == "sushiswap_v3"

    def test_underscore_variant_kind(self) -> None:
        """variant='aerodrome' → kind='aerodrome_v3'.

        After fixing AerodromeV3Pool.variant to use the bare DEX name,
        the kind derives correctly without double-suffixing.
        """
        registry = PoolTypeRegistry()
        registry.register(
            AerodromeV3Pool,
            chain_id=8453,
            factory_address="0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A",
        )
        desc = registry.get_descriptor(
            chain_id=8453, factory_address="0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A"
        )
        assert desc is not None
        assert desc.kind == "aerodrome_v3"


# --- Auto-derivation of invariant from class hierarchy ---


class TestInvariantDerivation:
    """Test that PoolFamily is auto-derived from the class hierarchy."""

    def test_v2_subclass_is_constant_product(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            SushiswapV2Pool,
            chain_id=1,
            factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
            pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
        )
        desc = registry.get_descriptor(
            chain_id=1, factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"
        )
        assert desc is not None
        assert desc.family == PoolFamily.CONSTANT_PRODUCT

    def test_v3_subclass_is_concentrated_liquidity(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            SushiswapV3Pool,
            chain_id=1,
            factory_address="0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F",
        )
        desc = registry.get_descriptor(
            chain_id=1, factory_address="0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"
        )
        assert desc is not None
        assert desc.family == PoolFamily.CONCENTRATED_LIQUIDITY

    def test_camelot_is_constant_product(self) -> None:
        """CamelotLiquidityPool extends UniswapV2Pool → CONSTANT_PRODUCT."""
        registry = PoolTypeRegistry()
        registry.register(
            CamelotLiquidityPool,
            chain_id=42161,
            factory_address="0x6EcCab422D763aC031210895C81787E87B43A652",
            pool_init_hash="0xa856464ae65f7619087bc369daaf7e387dae1e5af69cfa7935850ebf754b04c1",
        )
        desc = registry.get_descriptor(
            chain_id=42161, factory_address="0x6EcCab422D763aC031210895C81787E87B43A652"
        )
        assert desc is not None
        assert desc.family == PoolFamily.CONSTANT_PRODUCT


# --- Variant from class attribute ---


class TestVariantFromClassAttribute:
    """Test that the variant is read from the class's `variant` attribute."""

    def test_canonical_pool_has_none_variant(self) -> None:
        assert UniswapV2Pool.variant is None
        assert UniswapV3Pool.variant is None

    def test_sushiswap_variant(self) -> None:
        assert SushiswapV2Pool.variant == "sushiswap"
        assert SushiswapV3Pool.variant == "sushiswap"

    def test_pancakeswap_variant(self) -> None:
        assert PancakeswapV2Pool.variant == "pancakeswap"
        assert PancakeswapV3Pool.variant == "pancakeswap"

    def test_camelot_variant(self) -> None:
        assert CamelotLiquidityPool.variant == "camelot"


# --- Registration and lookup ---


class TestPoolTypeRegistryRegistration:
    """Test the unified register() and lookup methods."""

    def test_register_and_get_class(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            SushiswapV2Pool,
            chain_id=1,
            factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
            pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
        )
        result = registry.get_class(
            chain_id=1, factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"
        )
        assert result is SushiswapV2Pool

    def test_register_and_get_descriptor(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            SushiswapV2Pool,
            chain_id=1,
            factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
            pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
        )
        desc = registry.get_descriptor(
            chain_id=1, factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"
        )
        assert desc is not None
        assert desc.family == PoolFamily.CONSTANT_PRODUCT
        assert desc.variant == "sushiswap"
        assert desc.kind == "sushiswap_v2"
        assert desc.factory == "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"

    def test_register_and_get_deployment(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            SushiswapV2Pool,
            chain_id=1,
            factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
            pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
        )
        deployment = registry.get_deployment(
            chain_id=1, factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"
        )
        assert deployment is not None
        assert (
            deployment.pool_init_hash
            == "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303"
        )
        assert deployment.deployer == "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"

    def test_deployer_default_is_factory(self) -> None:
        """If deployer is not specified, it defaults to the factory address."""
        registry = PoolTypeRegistry()
        registry.register(
            SushiswapV2Pool,
            chain_id=1,
            factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
            pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
        )
        deployment = registry.get_deployment(
            chain_id=1, factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"
        )
        assert deployment is not None
        assert deployment.deployer == "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"

    def test_deployer_override(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            UniswapV3Pool,
            chain_id=1,
            factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984",
            deployer="0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9",
        )
        deployment = registry.get_deployment(
            chain_id=1, factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984"
        )
        assert deployment is not None
        assert deployment.deployer == "0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9"

    def test_has_registration(self) -> None:
        registry = PoolTypeRegistry()
        factory = "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"
        assert not registry.has_registration(chain_id=1, factory_address=factory)
        registry.register(
            SushiswapV2Pool,
            chain_id=1,
            factory_address=factory,
            pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
        )
        assert registry.has_registration(chain_id=1, factory_address=factory)

    def test_unknown_factory_returns_none(self) -> None:
        registry = PoolTypeRegistry()
        assert registry.get_class(chain_id=1, factory_address="0x" + "0" * 40) is None
        assert registry.get_descriptor(chain_id=1, factory_address="0x" + "0" * 40) is None
        assert registry.get_deployment(chain_id=1, factory_address="0x" + "0" * 40) is None

    def test_register_same_factory_twice_raises(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            SushiswapV2Pool,
            chain_id=1,
            factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
            pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
        )
        with pytest.raises(ValueError, match="already registered"):
            registry.register(
                SushiswapV2Pool,
                chain_id=1,
                factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
                pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
            )


# --- Default class fallback ---


class TestPoolTypeRegistryDefaults:
    """Test default class fallback for unrecognized factories."""

    def test_set_default_and_get_class(self) -> None:
        registry = PoolTypeRegistry()
        registry.set_default_v2_class(UniswapV2Pool)
        registry.set_default_v3_class(UniswapV3Pool)

        assert registry.get_v2_class(chain_id=1, factory_address="0x" + "0" * 40) is UniswapV2Pool
        assert registry.get_v3_class(chain_id=1, factory_address="0x" + "0" * 40) is UniswapV3Pool

    def test_no_default_returns_none(self) -> None:
        registry = PoolTypeRegistry()
        assert registry.get_class(chain_id=1, factory_address="0x" + "0" * 40) is None


# --- Descriptor is a PoolTypeDescriptor ---


class TestDescriptorShape:
    """Test that the descriptor has all expected fields."""

    def test_descriptor_fields(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            CamelotLiquidityPool,
            chain_id=42161,
            factory_address="0x6EcCab422D763aC031210895C81787E87B43A652",
            pool_init_hash="0xa856464ae65f7619087bc369daaf7e387dae1e5af69cfa7935850ebf754b04c1",
        )
        desc = registry.get_descriptor(
            chain_id=42161, factory_address="0x6EcCab422D763aC031210895C81787E87B43A652"
        )
        assert desc is not None
        assert desc.family == PoolFamily.CONSTANT_PRODUCT
        assert desc.variant == "camelot"
        assert desc.kind == "camelot_v2"
        assert desc.factory == "0x6EcCab422D763aC031210895C81787E87B43A652"


# --- Reverse lookup by kind ---


class TestKindReverseLookup:
    """Test get_descriptor_by_kind() reverse index."""

    def test_unknown_kind_returns_none(self) -> None:
        registry = PoolTypeRegistry()
        assert registry.get_descriptor_by_kind("nonexistent_v2") is None

    def test_lookup_by_kind_after_registration(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            SushiswapV2Pool,
            chain_id=1,
            factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
            pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
        )
        desc = registry.get_descriptor_by_kind("sushiswap_v2")
        assert desc is not None
        assert desc.family == PoolFamily.CONSTANT_PRODUCT
        assert desc.variant == "sushiswap"
        assert desc.kind == "sushiswap_v2"

    def test_lookup_by_kind_returns_last_registered(self) -> None:
        """When multiple deployments share a kind, the reverse index returns the last one."""
        registry = PoolTypeRegistry()
        registry.register(
            SushiswapV2Pool,
            chain_id=1,
            factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
            pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
        )
        registry.register(
            SushiswapV2Pool,
            chain_id=8453,
            factory_address="0x71524B4f93c58fcbF659783284E38825f0622859",
            pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
        )
        desc = registry.get_descriptor_by_kind("sushiswap_v2")
        assert desc is not None
        assert desc.kind == "sushiswap_v2"
        # The descriptor's factory matches the last-registered deployment
        assert desc.factory == "0x71524B4f93c58fcbF659783284E38825f0622859"

    def test_lookup_all_kinds(self) -> None:
        """Every built-in kind string should be resolvable via reverse lookup."""
        registry = PoolTypeRegistry()
        registry.set_default_v2_class(UniswapV2Pool)
        registry.set_default_v3_class(UniswapV3Pool)
        registry.register(
            UniswapV2Pool,
            chain_id=1,
            factory_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            pool_init_hash="0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f",
        )
        registry.register(
            UniswapV3Pool,
            chain_id=1,
            factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        )
        registry.register(
            SushiswapV2Pool,
            chain_id=1,
            factory_address="0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",
            pool_init_hash="0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303",
        )
        registry.register(
            SushiswapV3Pool,
            chain_id=1,
            factory_address="0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F",
        )
        registry.register(
            PancakeswapV2Pool,
            chain_id=1,
            factory_address="0x1097053Fd2ea711dad45caCcc45EfF7548fCB362",
        )
        registry.register(
            PancakeswapV3Pool,
            chain_id=1,
            factory_address="0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865",
        )
        registry.register(
            CamelotLiquidityPool,
            chain_id=42161,
            factory_address="0x6EcCab422D763aC031210895C81787E87B43A652",
            pool_init_hash="0xa856464ae65f7619087bc369daaf7e387dae1e5af69cfa7935850ebf754b04c1",
        )
        registry.register(
            AerodromeV3Pool,
            chain_id=8453,
            factory_address="0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A",
        )

        for kind in [
            "uniswap_v2",
            "uniswap_v3",
            "sushiswap_v2",
            "sushiswap_v3",
            "pancakeswap_v2",
            "pancakeswap_v3",
            "camelot_v2",
            "aerodrome_v3",
        ]:
            desc = registry.get_descriptor_by_kind(kind)
            assert desc is not None, f"No descriptor for kind={kind}"
            assert desc.kind == kind


# --- V3 pools don't need pool_init_hash ---


class TestV3Registration:
    """Test that V3 pools can be registered without pool_init_hash."""

    def test_register_v3_without_init_hash(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            UniswapV3Pool,
            chain_id=1,
            factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        )
        desc = registry.get_descriptor(
            chain_id=1, factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984"
        )
        assert desc is not None
        assert desc.family == PoolFamily.CONCENTRATED_LIQUIDITY
        assert desc.kind == "uniswap_v3"

    def test_v3_deployment_has_no_init_hash(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            UniswapV3Pool,
            chain_id=1,
            factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        )
        deployment = registry.get_deployment(
            chain_id=1, factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984"
        )
        assert deployment is not None
        assert deployment.pool_init_hash is None
