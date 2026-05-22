"""
Tests that all built-in exchanges, deployments, and factories can be registered
under the new PoolTypeRegistry scheme.

This test reconstructs the full registration table that the DEX __init__.py
modules will eventually produce, and validates every field against the
deployment data in uniswap/deployments.py.
"""

import pytest

from degenbot.aerodrome.pools import AerodromeV2Pool, AerodromeV3Pool
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.pancakeswap.pools import PancakeswapV2Pool, PancakeswapV3Pool
from degenbot.registry.pool_type import PoolTypeRegistry
from degenbot.sushiswap.pools import SushiswapV2Pool, SushiswapV3Pool
from degenbot.swapbased.pools import SwapbasedV2Pool
from degenbot.types.pool_type import PoolFamily
from degenbot.uniswap.deployments import (
    ArbitrumCamelotV2,
    ArbitrumSushiswapV2,
    ArbitrumSushiswapV3,
    ArbitrumUniswapV3,
    BaseAerodromeV2,
    BaseAerodromeV3,
    BasePancakeswapV2,
    BasePancakeswapV3,
    BaseSushiswapV2,
    BaseSushiswapV3,
    BaseSwapbasedV2,
    BaseUniswapV2,
    BaseUniswapV3,
    BaseUniswapV4,
    EthereumMainnetPancakeswapV2,
    EthereumMainnetPancakeswapV3,
    EthereumMainnetSushiswapV2,
    EthereumMainnetSushiswapV3,
    EthereumMainnetUniswapV2,
    EthereumMainnetUniswapV3,
    EthereumMainnetUniswapV4,
)
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _factory(deployment: object) -> str:
    """Extract the factory address from a deployment object."""
    return deployment.factory.address


def _deployer(deployment: object) -> str:
    """Extract the deployer from a deployment, falling back to factory."""
    d = deployment.factory.deployer
    if d is not None:
        return d
    return deployment.factory.address


def _init_hash(deployment: object) -> str | None:
    """Extract pool_init_hash, treating empty string as None."""
    h = deployment.factory.pool_init_hash
    return h or None


# ---------------------------------------------------------------------------
# All registrations, keyed by (chain_id, factory_address)
# ---------------------------------------------------------------------------

# Each entry: (pool_class, deployment_object, expected_variant, expected_kind)
# Excludes AerodromeV2 (not buildable via build_pool) and V4 deployments.
REGISTRATIONS: dict[tuple[int, str], tuple[type, object, str | None, str]] = {
    # Ethereum Mainnet (chain_id=1)
    (1, _factory(EthereumMainnetUniswapV2)): (
        UniswapV2Pool,
        EthereumMainnetUniswapV2,
        None,
        "uniswap_v2",
    ),
    (1, _factory(EthereumMainnetUniswapV3)): (
        UniswapV3Pool,
        EthereumMainnetUniswapV3,
        None,
        "uniswap_v3",
    ),
    (1, _factory(EthereumMainnetSushiswapV2)): (
        SushiswapV2Pool,
        EthereumMainnetSushiswapV2,
        "sushiswap",
        "sushiswap_v2",
    ),
    (1, _factory(EthereumMainnetSushiswapV3)): (
        SushiswapV3Pool,
        EthereumMainnetSushiswapV3,
        "sushiswap",
        "sushiswap_v3",
    ),
    (1, _factory(EthereumMainnetPancakeswapV2)): (
        PancakeswapV2Pool,
        EthereumMainnetPancakeswapV2,
        "pancakeswap",
        "pancakeswap_v2",
    ),
    (1, _factory(EthereumMainnetPancakeswapV3)): (
        PancakeswapV3Pool,
        EthereumMainnetPancakeswapV3,
        "pancakeswap",
        "pancakeswap_v3",
    ),
    (8453, _factory(BaseUniswapV2)): (UniswapV2Pool, BaseUniswapV2, None, "uniswap_v2"),
    (8453, _factory(BaseUniswapV3)): (UniswapV3Pool, BaseUniswapV3, None, "uniswap_v3"),
    (8453, _factory(BaseSushiswapV2)): (
        SushiswapV2Pool,
        BaseSushiswapV2,
        "sushiswap",
        "sushiswap_v2",
    ),
    (8453, _factory(BaseSushiswapV3)): (
        SushiswapV3Pool,
        BaseSushiswapV3,
        "sushiswap",
        "sushiswap_v3",
    ),
    (8453, _factory(BasePancakeswapV2)): (
        PancakeswapV2Pool,
        BasePancakeswapV2,
        "pancakeswap",
        "pancakeswap_v2",
    ),
    (8453, _factory(BasePancakeswapV3)): (
        PancakeswapV3Pool,
        BasePancakeswapV3,
        "pancakeswap",
        "pancakeswap_v3",
    ),
    (8453, _factory(BaseAerodromeV3)): (
        AerodromeV3Pool,
        BaseAerodromeV3,
        "aerodrome",
        "aerodrome_v3",
    ),
    (8453, _factory(BaseAerodromeV2)): (
        AerodromeV2Pool,
        BaseAerodromeV2,
        "aerodrome",
        "aerodrome_v2",
    ),
    (8453, _factory(BaseSwapbasedV2)): (
        SwapbasedV2Pool,
        BaseSwapbasedV2,
        "swapbased",
        "swapbased_v2",
    ),
    (42161, _factory(ArbitrumUniswapV3)): (UniswapV3Pool, ArbitrumUniswapV3, None, "uniswap_v3"),
    (42161, _factory(ArbitrumSushiswapV2)): (
        SushiswapV2Pool,
        ArbitrumSushiswapV2,
        "sushiswap",
        "sushiswap_v2",
    ),
    (42161, _factory(ArbitrumSushiswapV3)): (
        SushiswapV3Pool,
        ArbitrumSushiswapV3,
        "sushiswap",
        "sushiswap_v3",
    ),
    (42161, _factory(ArbitrumCamelotV2)): (
        CamelotLiquidityPool,
        ArbitrumCamelotV2,
        "camelot",
        "camelot_v2",
    ),
}


class TestFullRegistration:
    """Register all built-in deployments and validate every field."""

    @pytest.fixture
    def registry(self) -> PoolTypeRegistry:
        """A fresh registry with all built-in deployments registered."""
        reg = PoolTypeRegistry()
        reg.set_default_v2_class(UniswapV2Pool)
        reg.set_default_v3_class(UniswapV3Pool)

        for (chain_id, factory), (pool_class, deployment, _, _) in REGISTRATIONS.items():
            reg.register(
                pool_class,
                chain_id=chain_id,
                factory_address=factory,
                pool_init_hash=_init_hash(deployment),
                deployer=_deployer(deployment),
            )
        return reg

    def test_all_registrations_succeed(self, registry: PoolTypeRegistry) -> None:
        """Every deployment should be registered without error."""
        for chain_id, factory in REGISTRATIONS:
            assert registry.has_registration(chain_id, factory), (
                f"Missing registration for chain={chain_id} factory={factory}"
            )

    def test_total_count(self, registry: PoolTypeRegistry) -> None:
        """The registry should contain exactly the built-in registrations."""
        assert len(registry.registrations) == len(REGISTRATIONS)

    def test_no_duplicate_registrations(self) -> None:
        """Each (chain_id, factory) should be unique across all deployments."""
        keys = list(REGISTRATIONS.keys())
        assert len(keys) == len(set(keys))

    @pytest.mark.parametrize(
        ("chain_id", "factory"),
        list(REGISTRATIONS.keys()),
        ids=[f"{c}-{f[:10]}…" for (c, f) in REGISTRATIONS],
    )
    def test_pool_class(self, registry: PoolTypeRegistry, chain_id: int, factory: str) -> None:
        """get_class returns the correct pool class."""
        expected_class = REGISTRATIONS[chain_id, factory][0]
        result = registry.get_class(chain_id, factory)
        assert result is expected_class

    @pytest.mark.parametrize(
        ("chain_id", "factory"),
        list(REGISTRATIONS.keys()),
        ids=[f"{c}-{f[:10]}…" for (c, f) in REGISTRATIONS],
    )
    def test_invariant(self, registry: PoolTypeRegistry, chain_id: int, factory: str) -> None:
        """The family is correctly derived from the class hierarchy."""
        pool_class = REGISTRATIONS[chain_id, factory][0]
        desc = registry.get_descriptor(chain_id, factory)
        assert desc is not None

        if issubclass(pool_class, (UniswapV2Pool, AerodromeV2Pool)):
            assert desc.family == PoolFamily.CONSTANT_PRODUCT
        elif issubclass(pool_class, UniswapV3Pool):
            assert desc.family == PoolFamily.CONCENTRATED_LIQUIDITY
        else:
            pytest.fail(f"Unexpected pool class hierarchy: {pool_class}")

    @pytest.mark.parametrize(
        ("chain_id", "factory"),
        list(REGISTRATIONS.keys()),
        ids=[f"{c}-{f[:10]}…" for (c, f) in REGISTRATIONS],
    )
    def test_variant(self, registry: PoolTypeRegistry, chain_id: int, factory: str) -> None:
        """The variant is read from the class's `variant` attribute."""
        expected_variant = REGISTRATIONS[chain_id, factory][2]
        desc = registry.get_descriptor(chain_id, factory)
        assert desc is not None
        assert desc.variant == expected_variant

    @pytest.mark.parametrize(
        ("chain_id", "factory"),
        list(REGISTRATIONS.keys()),
        ids=[f"{c}-{f[:10]}…" for (c, f) in REGISTRATIONS],
    )
    def test_kind(self, registry: PoolTypeRegistry, chain_id: int, factory: str) -> None:
        """The kind is correctly derived from invariant + variant."""
        expected_kind = REGISTRATIONS[chain_id, factory][3]
        desc = registry.get_descriptor(chain_id, factory)
        assert desc is not None
        assert desc.kind == expected_kind

    @pytest.mark.parametrize(
        ("chain_id", "factory"),
        list(REGISTRATIONS.keys()),
        ids=[f"{c}-{f[:10]}…" for (c, f) in REGISTRATIONS],
    )
    def test_deployment_data(self, registry: PoolTypeRegistry, chain_id: int, factory: str) -> None:
        """Deployment data matches hardcoded registration data."""
        deployment = REGISTRATIONS[chain_id, factory][1]
        result = registry.get_deployment(chain_id, factory)
        assert result is not None
        assert result.factory_address == factory
        assert result.deployer == _deployer(deployment)
        assert result.pool_init_hash == _init_hash(deployment)

    @pytest.mark.parametrize(
        ("chain_id", "factory"),
        list(REGISTRATIONS.keys()),
        ids=[f"{c}-{f[:10]}…" for (c, f) in REGISTRATIONS],
    )
    def test_descriptor_factory(
        self, registry: PoolTypeRegistry, chain_id: int, factory: str
    ) -> None:
        """The descriptor's factory field matches the registration factory."""
        desc = registry.get_descriptor(chain_id, factory)
        assert desc is not None
        assert desc.factory == factory


class TestDeploymentDataMatchesPoolTypeRegistry:
    """
    Validate that pool_type_registry has correct deployment data.

    pool_type_registry is the sole source of truth — there is no longer
    a separate FACTORY_DEPLOYMENTS dict to cross-check against.
    """

    @pytest.fixture
    def registry(self) -> PoolTypeRegistry:
        reg = PoolTypeRegistry()
        for (chain_id, factory), (pool_class, deployment, _, _) in REGISTRATIONS.items():
            reg.register(
                pool_class,
                chain_id=chain_id,
                factory_address=factory,
                pool_init_hash=_init_hash(deployment),
                deployer=_deployer(deployment),
            )
        return reg

    def test_ethereum_mainnet_factories(self, registry: PoolTypeRegistry) -> None:
        """All ETH mainnet deployments are registered."""
        eth_entries = [(c, f) for c, f in REGISTRATIONS if c == 1]
        for chain_id, factory in eth_entries:
            assert registry.has_registration(chain_id, factory), (
                f"ETH mainnet factory {factory} not registered"
            )

    def test_base_factories(self, registry: PoolTypeRegistry) -> None:
        """All Base deployments are registered."""
        base_entries = [(c, f) for c, f in REGISTRATIONS if c == 8453]
        for chain_id, factory in base_entries:
            assert registry.has_registration(chain_id, factory), (
                f"Base factory {factory} not registered"
            )

    def test_arbitrum_factories(self, registry: PoolTypeRegistry) -> None:
        """All Arbitrum deployments are registered."""
        arb_entries = [(c, f) for c, f in REGISTRATIONS if c == 42161]
        for chain_id, factory in arb_entries:
            assert registry.has_registration(chain_id, factory), (
                f"Arbitrum factory {factory} not registered"
            )

    def test_init_hashes_match(self, registry: PoolTypeRegistry) -> None:
        """pool_init_hash in registry matches hardcoded deployment data."""
        for chain_id, factory in REGISTRATIONS:
            reg_deployment = registry.get_deployment(chain_id, factory)
            assert reg_deployment is not None
            expected_deployment = REGISTRATIONS[chain_id, factory][1]
            expected = _init_hash(expected_deployment)
            assert reg_deployment.pool_init_hash == expected


class TestAerodromeV2Registration:
    """AerodromeV2Pool is registered with the pool type registry."""

    def test_aerodrome_v2_in_registrations(self) -> None:
        """AerodromeV2Pool is registered in the registry."""
        key = (8453, BaseAerodromeV2.factory.address)
        assert key in REGISTRATIONS

    def test_aerodrome_v2_has_variant(self) -> None:
        """AerodromeV2Pool has a variant attribute used for kind derivation."""
        assert AerodromeV2Pool.variant == "aerodrome"


class TestV4DeploymentsExcluded:
    """V4 deployments use PoolManager, not factory — not covered by this registry."""

    def test_v4_not_in_registrations(self) -> None:
        """V4 deployments are not keyed by factory address."""
        for deployment in [EthereumMainnetUniswapV4, BaseUniswapV4]:
            assert not hasattr(deployment, "factory")


class TestDefaultFallback:
    """Unrecognized factories should fall back to UniswapV2/V3 defaults."""

    @pytest.fixture
    def registry(self) -> PoolTypeRegistry:
        reg = PoolTypeRegistry()
        reg.set_default_v2_class(UniswapV2Pool)
        reg.set_default_v3_class(UniswapV3Pool)
        for (chain_id, factory), (pool_class, deployment, _, _) in REGISTRATIONS.items():
            reg.register(
                pool_class,
                chain_id=chain_id,
                factory_address=factory,
                pool_init_hash=_init_hash(deployment),
                deployer=_deployer(deployment),
            )
        return reg

    def test_unknown_v2_factory_falls_back_to_uniswap(self, registry: PoolTypeRegistry) -> None:
        assert registry.get_v2_class(chain_id=1, factory_address="0x" + "0" * 40) is UniswapV2Pool

    def test_unknown_v3_factory_falls_back_to_uniswap(self, registry: PoolTypeRegistry) -> None:
        assert registry.get_v3_class(chain_id=1, factory_address="0x" + "0" * 40) is UniswapV3Pool

    def test_unknown_factory_has_no_descriptor(self, registry: PoolTypeRegistry) -> None:
        assert registry.get_descriptor(chain_id=1, factory_address="0x" + "0" * 40) is None

    def test_unknown_factory_has_no_deployment(self, registry: PoolTypeRegistry) -> None:
        assert registry.get_deployment(chain_id=1, factory_address="0x" + "0" * 40) is None
