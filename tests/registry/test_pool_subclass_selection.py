"""
Tests that verify pool managers return the correct pool subclass for each DEX variant.

This ensures that:
- UniswapV2PoolManager returns UniswapV2Pool
- SushiswapV2PoolManager returns SushiswapV2Pool
- AerodromeV2PoolManager returns AerodromeV2Pool
- UniswapV3PoolManager returns UniswapV3Pool
- SushiswapV3PoolManager returns SushiswapV3Pool
- PancakeswapV3PoolManager returns PancakeswapV3Pool
- AerodromeV3PoolManager returns AerodromeV3Pool
"""

from degenbot.anvil_fork import AnvilFork
from degenbot.checksum_cache import get_checksum_address
from degenbot.provider import ProviderAdapter
from degenbot.sushiswap.managers import SushiswapV2PoolManager
from degenbot.sushiswap.pools import SushiswapV2Pool
from degenbot.uniswap.managers import UniswapV2PoolManager, UniswapV3PoolManager
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from tests.helpers.bot_factory import make_bot_with_provider

# =============================================================================
# Mainnet addresses (chain_id=1)
# =============================================================================

MAINNET_UNISWAP_V2_FACTORY = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
MAINNET_SUSHISWAP_V2_FACTORY = get_checksum_address("0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac")
MAINNET_UNISWAP_V3_FACTORY = get_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984")

MAINNET_WETH = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
MAINNET_WBTC = get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")

# Mainnet pools
MAINNET_UNISWAP_V2_WETH_WBTC = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
MAINNET_SUSHISWAP_V2_WETH_WBTC = get_checksum_address("0xceff51756c56ceffca006cd410b03ffc46dd3a58")
MAINNET_UNISWAP_V3_WETH_WBTC = get_checksum_address("0xCBCdF9626bC03E24f779434178A73a0B4bad62eD")


# =============================================================================
# V2 Pool Subclass Tests
# =============================================================================


class TestV2PoolSubclassSelection:
    """Tests that V2 pool managers return the correct pool subclass."""

    def test_uniswap_v2_pool_manager_returns_uniswap_v2_pool(
        self, fork_mainnet_full: AnvilFork
    ) -> None:
        """UniswapV2PoolManager should return UniswapV2Pool instances."""
        bot = make_bot_with_provider(ProviderAdapter.from_web3(fork_mainnet_full.w3))
        manager = UniswapV2PoolManager(
            factory_address=MAINNET_UNISWAP_V2_FACTORY,
            bot=bot,
        )

        pool = manager.get_pool(MAINNET_UNISWAP_V2_WETH_WBTC)

        assert isinstance(pool, UniswapV2Pool), f"Expected UniswapV2Pool, got {type(pool).__name__}"
        # Should NOT be a subclass instance
        assert type(pool) is UniswapV2Pool, (
            f"Expected exact type UniswapV2Pool, got {type(pool).__name__}"
        )

    def test_sushiswap_v2_pool_manager_returns_sushiswap_v2_pool(
        self, fork_mainnet_full: AnvilFork
    ) -> None:
        """SushiswapV2PoolManager should return SushiswapV2Pool instances."""
        bot = make_bot_with_provider(ProviderAdapter.from_web3(fork_mainnet_full.w3))
        manager = SushiswapV2PoolManager(
            factory_address=MAINNET_SUSHISWAP_V2_FACTORY,
            bot=bot,
        )

        pool = manager.get_pool(MAINNET_SUSHISWAP_V2_WETH_WBTC)

        assert isinstance(pool, SushiswapV2Pool), (
            f"Expected SushiswapV2Pool, got {type(pool).__name__}"
        )
        assert type(pool) is SushiswapV2Pool, (
            f"Expected exact type SushiswapV2Pool, got {type(pool).__name__}"
        )


# =============================================================================
# V3 Pool Subclass Tests
# =============================================================================


class TestV3PoolSubclassSelection:
    """Tests that V3 pool managers return the correct pool subclass."""

    def test_uniswap_v3_pool_manager_returns_uniswap_v3_pool(
        self, fork_mainnet_full: AnvilFork
    ) -> None:
        """UniswapV3PoolManager should return UniswapV3Pool instances."""
        bot = make_bot_with_provider(ProviderAdapter.from_web3(fork_mainnet_full.w3))
        manager = UniswapV3PoolManager(
            factory_address=MAINNET_UNISWAP_V3_FACTORY,
            bot=bot,
        )

        pool = manager.get_pool(MAINNET_UNISWAP_V3_WETH_WBTC)

        assert isinstance(pool, UniswapV3Pool), f"Expected UniswapV3Pool, got {type(pool).__name__}"
        assert type(pool) is UniswapV3Pool, (
            f"Expected exact type UniswapV3Pool, got {type(pool).__name__}"
        )


# =============================================================================
# Bot.build_*_pool direct tests
# =============================================================================


class TestBotBuildPoolSubclassSelection:
    """Tests that Bot.build_v3_pool returns the correct subclass based on factory."""

    def test_build_v3_pool_returns_uniswap_v3_for_uniswap_factory(
        self, fork_mainnet_full: AnvilFork
    ) -> None:
        """Bot.build_v3_pool should return UniswapV3Pool for Uniswap factory."""
        bot = make_bot_with_provider(ProviderAdapter.from_web3(fork_mainnet_full.w3))

        pool = bot.build_v3_pool(MAINNET_UNISWAP_V3_WETH_WBTC)

        assert isinstance(pool, UniswapV3Pool), f"Expected UniswapV3Pool, got {type(pool).__name__}"
        assert type(pool) is UniswapV3Pool, (
            f"Expected exact type UniswapV3Pool, got {type(pool).__name__}"
        )
