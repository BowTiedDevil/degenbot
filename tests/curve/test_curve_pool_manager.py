"""Tests for CurveStableswapPoolManager.

Tracer bullet: a Curve pool manager that tracks pools,
delegates construction to Bot, and supports registry-based discovery.
"""

import pytest
from unittest.mock import MagicMock

from degenbot.anvil_fork import AnvilFork
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.erc20 import Erc20Token
from degenbot.provider import ProviderAdapter
from tests.helpers.bot_factory import make_bot_with_provider


def test_curve_pool_manager_exists() -> None:
    """CurveStableswapPoolManager is importable."""
    from degenbot.curve.managers import CurveStableswapPoolManager


def test_curve_pool_manager_requires_bot() -> None:
    """CurveStableswapPoolManager requires a Bot instance."""
    from degenbot.curve.managers import CurveStableswapPoolManager

    with pytest.raises(TypeError):
        CurveStableswapPoolManager()  # type: ignore[call-arg]


def test_curve_pool_manager_has_get_pool() -> None:
    """CurveStableswapPoolManager has a get_pool method."""
    from degenbot.curve.managers import CurveStableswapPoolManager

    assert hasattr(CurveStableswapPoolManager, "get_pool")


def test_curve_pool_manager_tracks_pools() -> None:
    """After getting a pool, it's tracked in the manager's _tracked_pools."""
    from degenbot.curve.managers import CurveStableswapPoolManager

    tokens = (
        Erc20Token(
            address="0x6B175474E89094C44Da98b954EedeAC495271d0F",
            name="DAI",
            symbol="DAI",
            decimals=18,
        ),
        Erc20Token(
            address="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            name="USDC",
            symbol="USDC",
            decimals=6,
        ),
    )
    pool = CurveStableswapPool(
        address="0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
        tokens=tokens,
        a_coefficient=2000,
        fee=4000000,
        admin_fee=5000000000,
        balances=(100, 200),
    )

    bot = MagicMock()
    bot.build_curve_pool.return_value = pool
    bot.pools.get.return_value = None
    bot.connections.default_chain_id = 1

    manager = CurveStableswapPoolManager(
        bot=bot,
        chain_id=1,
    )

    result = manager.get_pool("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")
    assert result is pool
    assert "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7" in manager._tracked_pools


def test_curve_pool_manager_returns_cached_pool() -> None:
    """Second call to get_pool returns the same tracked instance."""
    from degenbot.curve.managers import CurveStableswapPoolManager

    tokens = (
        Erc20Token(
            address="0x6B175474E89094C44Da98b954EedeAC495271d0F",
            name="DAI",
            symbol="DAI",
            decimals=18,
        ),
    )
    pool = CurveStableswapPool(
        address="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        tokens=tokens,
        a_coefficient=2000,
        fee=4000000,
        admin_fee=5000000000,
        balances=(100,),
    )

    bot = MagicMock()
    bot.build_curve_pool.return_value = pool
    bot.pools.get.return_value = None
    bot.connections.default_chain_id = 1

    manager = CurveStableswapPoolManager(bot=bot, chain_id=1)
    first = manager.get_pool("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
    second = manager.get_pool("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
    assert first is second
    # build_curve_pool should only be called once
    bot.build_curve_pool.assert_called_once()


def test_curve_pool_manager_fork_tripool(fork_mainnet_full: AnvilFork) -> None:
    """Integration test: build tripool via manager on a forked network."""
    from degenbot.curve.managers import CurveStableswapPoolManager

    bot = make_bot_with_provider(ProviderAdapter.from_web3(fork_mainnet_full.w3))
    manager = CurveStableswapPoolManager(bot=bot, chain_id=1)

    # 3Crv tripool
    pool = manager.get_pool("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")
    assert isinstance(pool, CurveStableswapPool)
    assert pool.address == "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"
    assert len(pool.tokens) == 3

    # Second call returns same instance
    same_pool = manager.get_pool("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")
    assert same_pool is pool


def test_curve_pool_manager_fork_metapool(fork_mainnet_full: AnvilFork) -> None:
    """Integration test: build metapool via manager on a forked network."""
    from degenbot.curve.managers import CurveStableswapPoolManager
    from degenbot.checksum_cache import get_checksum_address

    bot = make_bot_with_provider(ProviderAdapter.from_web3(fork_mainnet_full.w3))
    manager = CurveStableswapPoolManager(bot=bot, chain_id=1)

    # RAI-3Crv metapool
    pool = manager.get_pool("0x618788357D0EBd8A37e763ADab3bc575D54c2C7d")
    assert isinstance(pool, CurveStableswapPool)
    assert pool.address == get_checksum_address("0x618788357D0EBd8A37e763ADab3bc575D54c2C7d")
    assert pool.base_pool is not None


def test_curve_pool_manager_get_pools_for_token(fork_mainnet_full: AnvilFork) -> None:
    """get_pools_for_token returns pools containing a given token."""
    from degenbot.curve.managers import CurveStableswapPoolManager
    from degenbot.checksum_cache import get_checksum_address

    bot = make_bot_with_provider(ProviderAdapter.from_web3(fork_mainnet_full.w3))
    manager = CurveStableswapPoolManager(bot=bot, chain_id=1)

    # Build tripool
    manager.get_pool("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")

    # Find pools containing DAI
    dai_address = get_checksum_address("0x6B175474E89094C44Da98b954EedeAC495271d0F")
    pools_with_dai = manager.get_pools_for_token(dai_address)
    assert len(pools_with_dai) >= 1
    assert any(p.address == "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7" for p in pools_with_dai)

    # No pools with a random token
    random_address = get_checksum_address("0x0000000000000000000000000000000000000001")
    assert manager.get_pools_for_token(random_address) == []
