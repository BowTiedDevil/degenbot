"""Tests for PoolTypeRegistry — the unified pool type registration system.

ADR-005 slice 7 step 4b: the hollow V2 DEX subclasses are deleted. The
canonical ``LiquidityPool`` is registered for each V2 DEX factory with an
explicit ``variant=`` override (preserving the DB ``kind`` — e.g.
``variant="sushiswap"`` → ``kind="sushiswap_v2"``) + a ``dex_identity``
preset. The variant is now a REGISTRATION field (resolved via
``get_descriptor().variant`` / ``get_v2_identity().variant``), NOT a class
attribute on a per-DEX subclass.
"""

import pytest

from degenbot.aerodrome.pools import AerodromeV3Pool
from degenbot.pancakeswap.pools import PancakeswapV3Pool
from degenbot.registry.pool_type import PoolTypeRegistry
from degenbot.sushiswap.pools import SushiswapV3Pool
from degenbot.types.pool_type import PoolFamily
from degenbot.uniswap.liquidity_pool import LiquidityPool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

# Canonical factory addresses (the canonical-chain ones from the DEX
# self-registration modules). Reused across tests.
UNI_V2_MAINNET = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
UNI_V3_MAINNET = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
SUSHI_V2_MAINNET = "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"
SUSHI_V3_MAINNET = "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"
PANCAKE_V2_MAINNET = "0x1097053Fd2ea711dad45caCcc45EfF7548fCB362"
PANCAKE_V3_MAINNET = "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"
CAMELOT_ARB = "0x6EcCab422D763aC031210895C81787E87B43A652"
AERO_V3_BASE = "0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A"

UNI_V2_INIT_HASH = "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
SUSHI_V2_INIT_HASH = "0xe18a34eb0e04b04f7a0ac29a6e80748dca96319b42c54d679cb821dca90c6303"
CAMELOT_INIT_HASH = "0xa856464ae65f7619087bc369daaf7e387dae1e5af69cfa7935850ebf754b04c1"


# --- Auto-derivation of kind from variant + invariant ---


class TestKindDerivation:
    """Test that kind strings are auto-derived from variant + invariant."""

    def test_canonical_v2_kind(self) -> None:
        """No variant → 'uniswap_v2' for CONSTANT_PRODUCT."""
        registry = PoolTypeRegistry()
        registry.register(
            LiquidityPool,
            chain_id=1,
            factory_address=UNI_V2_MAINNET,
            pool_init_hash=UNI_V2_INIT_HASH,
        )
        desc = registry.get_descriptor(chain_id=1, factory_address=UNI_V2_MAINNET)
        assert desc is not None
        assert desc.kind == "uniswap_v2"

    def test_canonical_v3_kind(self) -> None:
        """No variant → 'uniswap_v3' for CONCENTRATED_LIQUIDITY."""
        registry = PoolTypeRegistry()
        registry.register(
            UniswapV3Pool,
            chain_id=1,
            factory_address=UNI_V3_MAINNET,
        )
        desc = registry.get_descriptor(chain_id=1, factory_address=UNI_V3_MAINNET)
        assert desc is not None
        assert desc.kind == "uniswap_v3"

    def test_variant_v2_kind(self) -> None:
        """variant='sushiswap' → kind='sushiswap_v2' (via the override)."""
        registry = PoolTypeRegistry()
        registry.register(
            LiquidityPool,
            chain_id=1,
            factory_address=SUSHI_V2_MAINNET,
            pool_init_hash=SUSHI_V2_INIT_HASH,
            variant="sushiswap",
        )
        desc = registry.get_descriptor(chain_id=1, factory_address=SUSHI_V2_MAINNET)
        assert desc is not None
        assert desc.kind == "sushiswap_v2"

    def test_variant_v3_kind(self) -> None:
        """variant='sushiswap' → kind='sushiswap_v3' (V3 keeps its class)."""
        registry = PoolTypeRegistry()
        registry.register(
            SushiswapV3Pool,
            chain_id=1,
            factory_address=SUSHI_V3_MAINNET,
        )
        desc = registry.get_descriptor(chain_id=1, factory_address=SUSHI_V3_MAINNET)
        assert desc is not None
        assert desc.kind == "sushiswap_v3"

    def test_underscore_variant_kind(self) -> None:
        """variant='aerodrome' → kind='aerodrome_v3'."""
        registry = PoolTypeRegistry()
        registry.register(
            AerodromeV3Pool,
            chain_id=8453,
            factory_address=AERO_V3_BASE,
        )
        desc = registry.get_descriptor(chain_id=8453, factory_address=AERO_V3_BASE)
        assert desc is not None
        assert desc.kind == "aerodrome_v3"


# --- Auto-derivation of invariant from class hierarchy ---


class TestInvariantDerivation:
    """Test that PoolFamily is auto-derived from the class hierarchy."""

    def test_v2_with_variant_is_constant_product(self) -> None:
        """LiquidityPool (registered with a variant) → CONSTANT_PRODUCT."""
        registry = PoolTypeRegistry()
        registry.register(
            LiquidityPool,
            chain_id=1,
            factory_address=SUSHI_V2_MAINNET,
            pool_init_hash=SUSHI_V2_INIT_HASH,
            variant="sushiswap",
        )
        desc = registry.get_descriptor(chain_id=1, factory_address=SUSHI_V2_MAINNET)
        assert desc is not None
        assert desc.family == PoolFamily.CONSTANT_PRODUCT

    def test_v3_subclass_is_concentrated_liquidity(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            SushiswapV3Pool,
            chain_id=1,
            factory_address=SUSHI_V3_MAINNET,
        )
        desc = registry.get_descriptor(chain_id=1, factory_address=SUSHI_V3_MAINNET)
        assert desc is not None
        assert desc.family == PoolFamily.CONCENTRATED_LIQUIDITY

    def test_camelot_is_constant_product(self) -> None:
        """Camelot (registered as LiquidityPool + variant='camelot') → CONSTANT_PRODUCT."""
        registry = PoolTypeRegistry()
        registry.register(
            LiquidityPool,
            chain_id=42161,
            factory_address=CAMELOT_ARB,
            pool_init_hash=CAMELOT_INIT_HASH,
            variant="camelot",
        )
        desc = registry.get_descriptor(chain_id=42161, factory_address=CAMELOT_ARB)
        assert desc is not None
        assert desc.family == PoolFamily.CONSTANT_PRODUCT


# --- Variant from registration override ---


class TestVariantFromRegistration:
    """The variant is a registration field (post slice 7 step 4b collapse)."""

    def test_canonical_pool_has_none_variant(self) -> None:
        """LiquidityPool / UniswapV3Pool registered with no variant → variant=None."""
        registry = PoolTypeRegistry()
        registry.register(
            LiquidityPool,
            chain_id=1,
            factory_address=UNI_V2_MAINNET,
            pool_init_hash=UNI_V2_INIT_HASH,
        )
        desc = registry.get_descriptor(chain_id=1, factory_address=UNI_V2_MAINNET)
        assert desc is not None
        assert desc.variant is None

    def test_variant_override_takes_precedence_over_class_attribute(self) -> None:
        """variant= wins over the class's own (None) variant attribute."""
        registry = PoolTypeRegistry()
        registry.register(
            LiquidityPool,
            chain_id=1,
            factory_address=SUSHI_V2_MAINNET,
            pool_init_hash=SUSHI_V2_INIT_HASH,
            variant="sushiswap",
        )
        desc = registry.get_descriptor(chain_id=1, factory_address=SUSHI_V2_MAINNET)
        assert desc is not None
        assert desc.variant == "sushiswap"

    def test_variant_falls_back_to_class_attribute_when_no_override(self) -> None:
        """V3 subclasses still derive variant from the class attribute."""
        registry = PoolTypeRegistry()
        registry.register(
            SushiswapV3Pool,
            chain_id=1,
            factory_address=SUSHI_V3_MAINNET,
        )
        desc = registry.get_descriptor(chain_id=1, factory_address=SUSHI_V3_MAINNET)
        assert desc is not None
        assert desc.variant == "sushiswap"


# --- Registration and lookup ---


class TestPoolTypeRegistryRegistration:
    """Test the unified register() and lookup methods."""

    def test_register_and_get_class(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            LiquidityPool,
            chain_id=1,
            factory_address=SUSHI_V2_MAINNET,
            pool_init_hash=SUSHI_V2_INIT_HASH,
            variant="sushiswap",
        )
        result = registry.get_class(chain_id=1, factory_address=SUSHI_V2_MAINNET)
        assert result is LiquidityPool

    def test_register_and_get_descriptor(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            LiquidityPool,
            chain_id=1,
            factory_address=SUSHI_V2_MAINNET,
            pool_init_hash=SUSHI_V2_INIT_HASH,
            variant="sushiswap",
        )
        desc = registry.get_descriptor(chain_id=1, factory_address=SUSHI_V2_MAINNET)
        assert desc is not None
        assert desc.family == PoolFamily.CONSTANT_PRODUCT
        assert desc.variant == "sushiswap"
        assert desc.kind == "sushiswap_v2"
        assert desc.factory == SUSHI_V2_MAINNET

    def test_register_and_get_deployment(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            LiquidityPool,
            chain_id=1,
            factory_address=SUSHI_V2_MAINNET,
            pool_init_hash=SUSHI_V2_INIT_HASH,
            variant="sushiswap",
        )
        deployment = registry.get_deployment(chain_id=1, factory_address=SUSHI_V2_MAINNET)
        assert deployment is not None
        assert deployment.pool_init_hash == SUSHI_V2_INIT_HASH
        assert deployment.deployer == SUSHI_V2_MAINNET

    def test_deployer_default_is_factory(self) -> None:
        """If deployer is not specified, it defaults to the factory address."""
        registry = PoolTypeRegistry()
        registry.register(
            LiquidityPool,
            chain_id=1,
            factory_address=SUSHI_V2_MAINNET,
            pool_init_hash=SUSHI_V2_INIT_HASH,
            variant="sushiswap",
        )
        deployment = registry.get_deployment(chain_id=1, factory_address=SUSHI_V2_MAINNET)
        assert deployment is not None
        assert deployment.deployer == SUSHI_V2_MAINNET

    def test_deployer_override(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            UniswapV3Pool,
            chain_id=1,
            factory_address=UNI_V3_MAINNET,
            deployer="0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9",
        )
        deployment = registry.get_deployment(chain_id=1, factory_address=UNI_V3_MAINNET)
        assert deployment is not None
        assert deployment.deployer == "0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9"

    def test_has_registration(self) -> None:
        registry = PoolTypeRegistry()
        assert not registry.has_registration(chain_id=1, factory_address=SUSHI_V2_MAINNET)
        registry.register(
            LiquidityPool,
            chain_id=1,
            factory_address=SUSHI_V2_MAINNET,
            pool_init_hash=SUSHI_V2_INIT_HASH,
            variant="sushiswap",
        )
        assert registry.has_registration(chain_id=1, factory_address=SUSHI_V2_MAINNET)

    def test_unknown_factory_returns_none(self) -> None:
        registry = PoolTypeRegistry()
        assert registry.get_class(chain_id=1, factory_address="0x" + "0" * 40) is None
        assert registry.get_descriptor(chain_id=1, factory_address="0x" + "0" * 40) is None
        assert registry.get_deployment(chain_id=1, factory_address="0x" + "0" * 40) is None

    def test_register_same_factory_twice_raises(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            LiquidityPool,
            chain_id=1,
            factory_address=SUSHI_V2_MAINNET,
            pool_init_hash=SUSHI_V2_INIT_HASH,
            variant="sushiswap",
        )
        with pytest.raises(ValueError, match="already registered"):
            registry.register(
                LiquidityPool,
                chain_id=1,
                factory_address=SUSHI_V2_MAINNET,
                pool_init_hash=SUSHI_V2_INIT_HASH,
                variant="sushiswap",
            )


# --- Default class fallback ---


class TestPoolTypeRegistryDefaults:
    """Test default class fallback for unrecognized factories."""

    def test_set_default_and_get_class(self) -> None:
        registry = PoolTypeRegistry()
        registry.set_default_v2_class(LiquidityPool)
        registry.set_default_v3_class(UniswapV3Pool)

        assert registry.get_v2_class(chain_id=1, factory_address="0x" + "0" * 40) is LiquidityPool
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
            LiquidityPool,
            chain_id=42161,
            factory_address=CAMELOT_ARB,
            pool_init_hash=CAMELOT_INIT_HASH,
            variant="camelot",
        )
        desc = registry.get_descriptor(chain_id=42161, factory_address=CAMELOT_ARB)
        assert desc is not None
        assert desc.family == PoolFamily.CONSTANT_PRODUCT
        assert desc.variant == "camelot"
        assert desc.kind == "camelot_v2"
        assert desc.factory == CAMELOT_ARB


# --- Reverse lookup by kind ---


class TestKindReverseLookup:
    """Test get_descriptor_by_kind() reverse index."""

    def test_unknown_kind_returns_none(self) -> None:
        registry = PoolTypeRegistry()
        assert registry.get_descriptor_by_kind("nonexistent_v2") is None

    def test_lookup_by_kind_after_registration(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            LiquidityPool,
            chain_id=1,
            factory_address=SUSHI_V2_MAINNET,
            pool_init_hash=SUSHI_V2_INIT_HASH,
            variant="sushiswap",
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
            LiquidityPool,
            chain_id=1,
            factory_address=SUSHI_V2_MAINNET,
            pool_init_hash=SUSHI_V2_INIT_HASH,
            variant="sushiswap",
        )
        sushi_base = "0x71524B4f93c58fcbF659783284E38825f0622859"
        registry.register(
            LiquidityPool,
            chain_id=8453,
            factory_address=sushi_base,
            pool_init_hash=SUSHI_V2_INIT_HASH,
            variant="sushiswap",
        )
        desc = registry.get_descriptor_by_kind("sushiswap_v2")
        assert desc is not None
        assert desc.kind == "sushiswap_v2"
        # The descriptor's factory matches the last-registered deployment
        assert desc.factory == sushi_base

    def test_lookup_all_kinds(self) -> None:
        """Every built-in kind string should be resolvable via reverse lookup."""
        registry = PoolTypeRegistry()
        registry.set_default_v2_class(LiquidityPool)
        registry.set_default_v3_class(UniswapV3Pool)
        registry.register(
            LiquidityPool,
            chain_id=1,
            factory_address=UNI_V2_MAINNET,
            pool_init_hash=UNI_V2_INIT_HASH,
        )
        registry.register(
            UniswapV3Pool,
            chain_id=1,
            factory_address=UNI_V3_MAINNET,
        )
        registry.register(
            LiquidityPool,
            chain_id=1,
            factory_address=SUSHI_V2_MAINNET,
            pool_init_hash=SUSHI_V2_INIT_HASH,
            variant="sushiswap",
        )
        registry.register(
            SushiswapV3Pool,
            chain_id=1,
            factory_address=SUSHI_V3_MAINNET,
        )
        registry.register(
            LiquidityPool,
            chain_id=1,
            factory_address=PANCAKE_V2_MAINNET,
            variant="pancakeswap",
        )
        registry.register(
            PancakeswapV3Pool,
            chain_id=1,
            factory_address=PANCAKE_V3_MAINNET,
        )
        registry.register(
            LiquidityPool,
            chain_id=42161,
            factory_address=CAMELOT_ARB,
            pool_init_hash=CAMELOT_INIT_HASH,
            variant="camelot",
        )
        registry.register(
            AerodromeV3Pool,
            chain_id=8453,
            factory_address=AERO_V3_BASE,
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
            factory_address=UNI_V3_MAINNET,
        )
        desc = registry.get_descriptor(chain_id=1, factory_address=UNI_V3_MAINNET)
        assert desc is not None
        assert desc.family == PoolFamily.CONCENTRATED_LIQUIDITY
        assert desc.kind == "uniswap_v3"

    def test_v3_deployment_has_no_init_hash(self) -> None:
        registry = PoolTypeRegistry()
        registry.register(
            UniswapV3Pool,
            chain_id=1,
            factory_address=UNI_V3_MAINNET,
        )
        deployment = registry.get_deployment(chain_id=1, factory_address=UNI_V3_MAINNET)
        assert deployment is not None
        assert deployment.pool_init_hash is None
