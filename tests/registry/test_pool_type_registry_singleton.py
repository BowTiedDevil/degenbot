"""
Tests that the module-level pool_type_registry singleton is correctly
populated by DEX module self-registration at import time.
"""

import pytest

from degenbot.aerodrome.pools import AerodromeV2Pool, AerodromeV3Pool
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.pancakeswap.pools import PancakeswapV2Pool, PancakeswapV3Pool
from degenbot.registry.pool_type import pool_type_registry
from degenbot.sushiswap.pools import SushiswapV2Pool, SushiswapV3Pool
from degenbot.swapbased.pools import SwapbasedV2Pool
from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS, BaseAerodromeV2
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

# (Expected (chain_id, factory) → (pool_class, expected_variant, expected_kind)
SINGLETON_REGISTRATIONS: dict[tuple[int, str], tuple[type, str | None, str]] = {
    # Ethereum Mainnet
    (1, "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"): (UniswapV2Pool, None, "uniswap_v2"),
    (1, "0x1F98431c8aD98523631AE4a59f267346ea31F984"): (UniswapV3Pool, None, "uniswap_v3"),
    (1, "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"): (
        SushiswapV2Pool,
        "sushiswap",
        "sushiswap_v2",
    ),
    (1, "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"): (
        SushiswapV3Pool,
        "sushiswap",
        "sushiswap_v3",
    ),
    (1, "0x1097053Fd2ea711dad45caCcc45EfF7548fCB362"): (
        PancakeswapV2Pool,
        "pancakeswap",
        "pancakeswap_v2",
    ),
    (1, "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"): (
        PancakeswapV3Pool,
        "pancakeswap",
        "pancakeswap_v3",
    ),
    # Base
    (8453, "0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6"): (UniswapV2Pool, None, "uniswap_v2"),
    (8453, "0x33128a8fC17869897dcE68Ed026d694621f6FDfD"): (UniswapV3Pool, None, "uniswap_v3"),
    (8453, "0x71524B4f93c58fcbF659783284E38825f0622859"): (
        SushiswapV2Pool,
        "sushiswap",
        "sushiswap_v2",
    ),
    (8453, "0xc35DADB65012eC5796536bD9864eD8773aBc74C4"): (
        SushiswapV3Pool,
        "sushiswap",
        "sushiswap_v3",
    ),
    (8453, "0x02a84c1b3BBD7401a5f7fa98a384EBC70bB5749E"): (
        PancakeswapV2Pool,
        "pancakeswap",
        "pancakeswap_v2",
    ),
    (8453, "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"): (
        PancakeswapV3Pool,
        "pancakeswap",
        "pancakeswap_v3",
    ),
    (8453, "0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A"): (
        AerodromeV3Pool,
        "aerodrome",
        "aerodrome_v3",
    ),
    (8453, "0x420DD381b31aEf6683db6B902084cB0FFECe40Da"): (
        AerodromeV2Pool,
        "aerodrome",
        "aerodrome_v2",
    ),
    (8453, "0x04C9f118d21e8B767D2e50C946f0cC9F6C367300"): (
        SwapbasedV2Pool,
        "swapbased",
        "swapbased_v2",
    ),
    # Arbitrum
    (42161, "0x1F98431c8aD98523631AE4a59f267346ea31F984"): (UniswapV3Pool, None, "uniswap_v3"),
    (42161, "0xc35DADB65012eC5796536bD9864eD8773aBc74C4"): (
        SushiswapV2Pool,
        "sushiswap",
        "sushiswap_v2",
    ),
    (42161, "0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e"): (
        SushiswapV3Pool,
        "sushiswap",
        "sushiswap_v3",
    ),
    (42161, "0x6EcCab422D763aC031210895C81787E87B43A652"): (
        CamelotLiquidityPool,
        "camelot",
        "camelot_v2",
    ),
    # Balancer V2 (Ethereum Mainnet)
    (1, "0x8e9AA87E45e92BAD7D5F7F9dd794cEa12f21707B"): (
        BalancerV2Pool,
        "balancer_weighted",
        "balancer_weighted",
    ),
    (1, "0x8519f5a4A85678e0e03395586e2e223D70E9e09B"): (
        BalancerV2StablePool,
        "balancer_stable",
        "balancer_stable",
    ),
    (1, "0xA8936f4824B2E6407fC0e94133909aEf7D48E876"): (
        BalancerV2StablePool,
        "balancer_stable",
        "balancer_stable",
    ),
}


class TestPoolTypeRegistrySingleton:
    """Validate that the module-level singleton is populated correctly."""

    @pytest.mark.parametrize(
        ("chain_id", "factory"),
        list(SINGLETON_REGISTRATIONS.keys()),
        ids=[f"{c}-{f[:10]}…" for (c, f) in SINGLETON_REGISTRATIONS],
    )
    def test_has_registration(self, chain_id: int, factory: str) -> None:
        """Every built-in deployment should be registered in the singleton."""
        assert pool_type_registry.has_registration(chain_id, factory), (
            f"Missing singleton registration for chain={chain_id} factory={factory}"
        )

    @pytest.mark.parametrize(
        ("chain_id", "factory"),
        list(SINGLETON_REGISTRATIONS.keys()),
        ids=[f"{c}-{f[:10]}…" for (c, f) in SINGLETON_REGISTRATIONS],
    )
    def test_pool_class(self, chain_id: int, factory: str) -> None:
        """get_class returns the correct pool class."""
        expected_class = SINGLETON_REGISTRATIONS[chain_id, factory][0]
        assert pool_type_registry.get_class(chain_id, factory) is expected_class

    @pytest.mark.parametrize(
        ("chain_id", "factory"),
        list(SINGLETON_REGISTRATIONS.keys()),
        ids=[f"{c}-{f[:10]}…" for (c, f) in SINGLETON_REGISTRATIONS],
    )
    def test_variant(self, chain_id: int, factory: str) -> None:
        """The descriptor variant matches the expected value."""
        expected_variant = SINGLETON_REGISTRATIONS[chain_id, factory][1]
        desc = pool_type_registry.get_descriptor(chain_id, factory)
        assert desc is not None
        assert desc.variant == expected_variant

    @pytest.mark.parametrize(
        ("chain_id", "factory"),
        list(SINGLETON_REGISTRATIONS.keys()),
        ids=[f"{c}-{f[:10]}…" for (c, f) in SINGLETON_REGISTRATIONS],
    )
    def test_kind(self, chain_id: int, factory: str) -> None:
        """The descriptor kind matches the expected value."""
        expected_kind = SINGLETON_REGISTRATIONS[chain_id, factory][2]
        desc = pool_type_registry.get_descriptor(chain_id, factory)
        assert desc is not None
        assert desc.kind == expected_kind

    @pytest.mark.parametrize(
        ("chain_id", "factory"),
        list(SINGLETON_REGISTRATIONS.keys()),
        ids=[f"{c}-{f[:10]}…" for (c, f) in SINGLETON_REGISTRATIONS],
    )
    def test_deployment_data_present(self, chain_id: int, factory: str) -> None:
        """Deployment data exists for every registered factory."""
        deployment = pool_type_registry.get_deployment(chain_id, factory)
        assert deployment is not None
        assert deployment.factory_address == factory

    def test_total_count(self) -> None:
        """The singleton should contain exactly the built-in registrations."""
        assert len(pool_type_registry.registrations) == len(SINGLETON_REGISTRATIONS)

    def test_default_v2_class(self) -> None:
        """UniswapV2Pool is set as the default V2 class."""
        assert pool_type_registry.get_v2_class(999, "0x" + "0" * 40) is UniswapV2Pool

    def test_default_v3_class(self) -> None:
        """UniswapV3Pool is set as the default V3 class."""
        assert pool_type_registry.get_v3_class(999, "0x" + "0" * 40) is UniswapV3Pool

    def test_aerodrome_v2_registered(self) -> None:
        """AerodromeV2Pool is registered with the pool type registry."""
        assert pool_type_registry.has_registration(8453, BaseAerodromeV2.factory.address)

    def test_aerodrome_v2_variant(self) -> None:
        """AerodromeV2Pool variant is 'aerodrome' (bare DEX name, no suffix)."""
        assert AerodromeV2Pool.variant == "aerodrome"


class TestSingletonKindReverseLookup:
    """Validate get_descriptor_by_kind on the module-level singleton."""

    @pytest.mark.parametrize(
        "kind",
        [
            "uniswap_v2",
            "uniswap_v3",
            "sushiswap_v2",
            "sushiswap_v3",
            "pancakeswap_v2",
            "pancakeswap_v3",
            "camelot_v2",
            "aerodrome_v2",
            "aerodrome_v3",
            "swapbased_v2",
        ],
    )
    def test_all_db_kinds_resolvable(self, kind: str) -> None:
        """Every DB kind value should resolve to a descriptor."""
        desc = pool_type_registry.get_descriptor_by_kind(kind)
        assert desc is not None, f"No descriptor for kind={kind}"
        assert desc.kind == kind

    def test_aerodrome_v2_kind_resolvable(self) -> None:
        """AerodromeV2 is registered, so 'aerodrome_v2' has a descriptor."""
        desc = pool_type_registry.get_descriptor_by_kind("aerodrome_v2")
        assert desc is not None
        assert desc.kind == "aerodrome_v2"


class TestDeploymentDataMatchesFactoriesModule:
    """Cross-check registry deployment data against FACTORY_DEPLOYMENTS."""

    @pytest.mark.parametrize(
        ("chain_id", "factory"),
        list(SINGLETON_REGISTRATIONS.keys()),
        ids=[f"{c}-{f[:10]}…" for (c, f) in SINGLETON_REGISTRATIONS],
    )
    def test_init_hash_matches_factories_module(self, chain_id: int, factory: str) -> None:
        """pool_init_hash in registry matches FACTORY_DEPLOYMENTS."""
        deployment = pool_type_registry.get_deployment(chain_id, factory)
        assert deployment is not None

        if chain_id in FACTORY_DEPLOYMENTS and factory in FACTORY_DEPLOYMENTS[chain_id]:
            fact_init_hash = FACTORY_DEPLOYMENTS[chain_id][factory].pool_init_hash
            expected = fact_init_hash or None
            assert deployment.pool_init_hash == expected

    @pytest.mark.parametrize(
        ("chain_id", "factory"),
        list(SINGLETON_REGISTRATIONS.keys()),
        ids=[f"{c}-{f[:10]}…" for (c, f) in SINGLETON_REGISTRATIONS],
    )
    def test_deployer_matches_factories_module(self, chain_id: int, factory: str) -> None:
        """deployer in registry matches FACTORY_DEPLOYMENTS."""
        deployment = pool_type_registry.get_deployment(chain_id, factory)
        assert deployment is not None

        if chain_id in FACTORY_DEPLOYMENTS and factory in FACTORY_DEPLOYMENTS[chain_id]:
            fact_deployment = FACTORY_DEPLOYMENTS[chain_id][factory]
            expected_deployer = (
                fact_deployment.deployer if fact_deployment.deployer is not None else factory
            )
            assert deployment.deployer == expected_deployer
