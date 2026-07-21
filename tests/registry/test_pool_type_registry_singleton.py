"""Tests that the module-level pool_type_registry singleton is correctly
populated by DEX module self-registration at import time.
"""

import pytest

from degenbot.aerodrome.pools import AerodromeV2Pool, AerodromeV3Pool
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import BalancerV2StablePool
from degenbot.pancakeswap.pools import PancakeswapV3Pool
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.deployments import BaseAerodromeV2
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

# (Expected (chain_id, factory) → (pool_class, expected_variant, expected_kind)
SINGLETON_REGISTRATIONS: dict[tuple[int, str], tuple[type, str | None, str]] = {
    # Ethereum Mainnet
    (1, "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"): (UniswapV2Pool, None, "uniswap_v2"),
    (1, "0x1F98431c8aD98523631AE4a59f267346ea31F984"): (UniswapV3Pool, None, "uniswap_v3"),
    (1, "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"): (
        UniswapV2Pool,
        "sushiswap",
        "sushiswap_v2",
    ),
    (1, "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"): (
        UniswapV3Pool,
        "sushiswap",
        "sushiswap_v3",
    ),
    (1, "0x1097053Fd2ea711dad45caCcc45EfF7548fCB362"): (
        UniswapV2Pool,
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
        UniswapV2Pool,
        "sushiswap",
        "sushiswap_v2",
    ),
    (8453, "0xc35DADB65012eC5796536bD9864eD8773aBc74C4"): (
        UniswapV3Pool,
        "sushiswap",
        "sushiswap_v3",
    ),
    (8453, "0x02a84c1b3BBD7401a5f7fa98a384EBC70bB5749E"): (
        UniswapV2Pool,
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
        UniswapV2Pool,
        "swapbased",
        "swapbased_v2",
    ),
    # Arbitrum
    (42161, "0x1F98431c8aD98523631AE4a59f267346ea31F984"): (UniswapV3Pool, None, "uniswap_v3"),
    (42161, "0xc35DADB65012eC5796536bD9864eD8773aBc74C4"): (
        UniswapV2Pool,
        "sushiswap",
        "sushiswap_v2",
    ),
    (42161, "0x1af415a1EbA07a4986a52B6f2e7dE7003D82231e"): (
        UniswapV3Pool,
        "sushiswap",
        "sushiswap_v3",
    ),
    (42161, "0x6EcCab422D763aC031210895C81787E87B43A652"): (
        UniswapV2Pool,
        "camelot",
        "camelot_v2",
    ),
    # Balancer V2 (Ethereum Mainnet) — all factory revisions. The rev tag
    # lives in the JSON ``name`` field only; ``variant`` stays null so all
    # Weighted revisions share the ``balancer_weighted`` kind and all
    # Stable/Composable revisions share ``balancer_stable`` (the revisions
    # simulate identically — the factory address is the discriminator).
    (1, "0x8E9aa87E45e92bad84D5F8DD1bff34Fb92637dE9"): (  # Weighted rev1
        BalancerV2Pool,
        "balancer_weighted",
        "balancer_weighted",
    ),
    (1, "0xcC508a455F5b0073973107Db6a878DdBDab957bC"): (  # Weighted rev2
        BalancerV2Pool,
        "balancer_weighted",
        "balancer_weighted",
    ),
    (1, "0x5Dd94Da3644DDD055fcf6B3E1aa310Bb7801EB8b"): (  # Weighted rev3
        BalancerV2Pool,
        "balancer_weighted",
        "balancer_weighted",
    ),
    (1, "0x897888115Ada5773E02aA29F775430BFB5F34c51"): (  # Weighted rev4
        BalancerV2Pool,
        "balancer_weighted",
        "balancer_weighted",
    ),
    (1, "0xA5bf2ddF098bb0Ef6d120C98217dD6B141c74EE0"): (  # Weighted 2 Tokens
        BalancerV2Pool,
        "balancer_weighted",
        "balancer_weighted",
    ),
    (1, "0xc66Ba2B6595D3613CCab350C886aCE23866EDe24"): (  # Stable rev1
        BalancerV2StablePool,
        "balancer_stable",
        "balancer_stable",
    ),
    (1, "0x8df6EfEc5547e31B0eb7d1291B511FF8a2bf987c"): (  # Stable rev2
        BalancerV2StablePool,
        "balancer_stable",
        "balancer_stable",
    ),
    (1, "0xf9ac7B9dF2b3454E841110CcE5550bD5AC6f875F"): (  # ComposableStable rev1
        BalancerV2StablePool,
        "balancer_stable",
        "balancer_stable",
    ),
    (1, "0x85a80afee867aDf27B50BdB7b76DA70f1E853062"): (  # ComposableStable rev2
        BalancerV2StablePool,
        "balancer_stable",
        "balancer_stable",
    ),
    (1, "0xdba127fBc23fb20F5929C546af220A991b5C6e01"): (  # ComposableStable rev3
        BalancerV2StablePool,
        "balancer_stable",
        "balancer_stable",
    ),
    (1, "0xfADa0f4547AB2de89D1304A668C39B3E09Aa7c76"): (  # ComposableStable rev4
        BalancerV2StablePool,
        "balancer_stable",
        "balancer_stable",
    ),
    (1, "0xDB8d758BCb971e482B2C45f7F8a7740283A1bd3A"): (  # ComposableStable rev5
        BalancerV2StablePool,
        "balancer_stable",
        "balancer_stable",
    ),
    (1, "0x5B42eC6D40f7B7965BE5308c70e2603c0281C1E9"): (  # ComposableStable rev6
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


class TestDeploymentDataIntegrity:
    """Validate registry deployment data directly.

    pool_type_registry is the sole source of truth — there is no longer
    a separate FACTORY_DEPLOYMENTS dict to cross-check against.
    """

    @pytest.mark.parametrize(
        ("chain_id", "factory"),
        list(SINGLETON_REGISTRATIONS.keys()),
        ids=[f"{c}-{f[:10]}…" for (c, f) in SINGLETON_REGISTRATIONS],
    )
    def test_init_hash_is_set(self, chain_id: int, factory: str) -> None:
        """pool_init_hash in registry is present (may be None for some pools)."""
        deployment = pool_type_registry.get_deployment(chain_id, factory)
        assert deployment is not None
        # pool_init_hash may be None (e.g. Sushiswap V3, Pancakeswap V3)
        # or empty string (Aerodrome) or a concrete hash
        assert deployment.pool_init_hash is None or isinstance(deployment.pool_init_hash, str)

    @pytest.mark.parametrize(
        ("chain_id", "factory"),
        list(SINGLETON_REGISTRATIONS.keys()),
        ids=[f"{c}-{f[:10]}…" for (c, f) in SINGLETON_REGISTRATIONS],
    )
    def test_deployer_is_set(self, chain_id: int, factory: str) -> None:
        """Deployer in registry is always set (may equal factory)."""
        deployment = pool_type_registry.get_deployment(chain_id, factory)
        assert deployment is not None
        assert deployment.deployer is not None
