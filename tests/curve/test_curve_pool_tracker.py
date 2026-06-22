"""Tests for CurveStableswapPoolTracker.

Tracer bullet: a Curve pool tracker that tracks pools,
delegates construction to Bot, and supports registry-based discovery.
"""

from unittest.mock import MagicMock

import pytest

from degenbot.anvil_fork import AnvilFork
from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.curve.trackers import CurveStableswapPoolTracker
from degenbot.degenbot_rs import PyBot
from degenbot.provider import ProviderAdapter
from tests.helpers.bot_factory import make_bot_with_provider
from tests.helpers.curve_pool_factory import make_curve_pool
from tests.helpers.erc20_factory import make_erc20

_PY_BOT = PyBot()


def test_curve_pool_tracker_exists() -> None:
    """CurveStableswapPoolTracker is importable."""


def test_curve_pool_tracker_requires_bot() -> None:
    """CurveStableswapPoolTracker requires a Bot instance."""
    with pytest.raises(TypeError):
        CurveStableswapPoolTracker()


def test_curve_pool_tracker_has_get_pool() -> None:
    """CurveStableswapPoolTracker has a get_pool method."""
    assert hasattr(CurveStableswapPoolTracker, "get_pool")


def test_curve_pool_tracker_tracks_pools() -> None:
    """After getting a pool, it's tracked in the tracker's _tracked_pools."""
    tokens = (
        make_erc20(
            _PY_BOT,
            "0x6B175474E89094C44Da98b954EedeAC495271d0F",
            name="DAI",
            symbol="DAI",
            decimals=18,
        ),
        make_erc20(
            _PY_BOT,
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            name="USDC",
            symbol="USDC",
            decimals=6,
        ),
    )
    pool = make_curve_pool(
        address="0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
        tokens=tokens,
        a_coefficient=2000,
        fee=4000000,
        admin_fee=5000000000,
        balances=(100, 200),
    )

    bot = MagicMock()
    bot.build_pool.return_value = pool
    bot.pools.get.return_value = None
    bot.chain_id = 1

    tracker = CurveStableswapPoolTracker(
        bot=bot,
        chain_id=1,
    )

    result = tracker.get_pool("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")
    assert result is pool
    assert "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7" in tracker._tracked_pools


def test_curve_pool_tracker_returns_cached_pool() -> None:
    """Second call to get_pool returns the same tracked instance."""
    tokens = (
        make_erc20(
            _PY_BOT,
            "0x6B175474E89094C44Da98b954EedeAC495271d0F",
            name="DAI",
            symbol="DAI",
            decimals=18,
        ),
    )
    pool = make_curve_pool(
        address="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        tokens=tokens,
        a_coefficient=2000,
        fee=4000000,
        admin_fee=5000000000,
        balances=(100,),
    )

    bot = MagicMock()
    bot.build_pool.return_value = pool
    bot.pools.get.return_value = None
    bot.chain_id = 1

    tracker = CurveStableswapPoolTracker(bot=bot, chain_id=1)
    first = tracker.get_pool("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
    second = tracker.get_pool("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
    assert first is second
    # build_pool should only be called once
    bot.build_pool.assert_called_once()


def test_curve_pool_tracker_fork_tripool(fork_mainnet_full: AnvilFork) -> None:
    """Integration test: build tripool via tracker on a forked network."""
    bot = make_bot_with_provider(ProviderAdapter.from_web3(fork_mainnet_full.w3))
    tracker = CurveStableswapPoolTracker(bot=bot, chain_id=1)

    # 3Crv tripool
    pool = tracker.get_pool("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")
    assert isinstance(pool, CurveStableswapPool)
    assert pool.address == "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"
    assert len(pool.tokens) == 3

    # Second call returns same instance
    same_pool = tracker.get_pool("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")
    assert same_pool is pool


def test_curve_pool_tracker_fork_metapool(fork_mainnet_full: AnvilFork) -> None:
    """Integration test: build metapool via tracker on a forked network."""
    bot = make_bot_with_provider(ProviderAdapter.from_web3(fork_mainnet_full.w3))
    tracker = CurveStableswapPoolTracker(bot=bot, chain_id=1)

    # RAI-3Crv metapool
    pool = tracker.get_pool("0x618788357D0EBd8A37e763ADab3bc575D54c2C7d")
    assert isinstance(pool, CurveStableswapPool)
    assert pool.address == get_checksum_address("0x618788357D0EBd8A37e763ADab3bc575D54c2C7d")
    assert pool.base_pool is not None


def test_curve_pool_tracker_get_pools_for_token(fork_mainnet_full: AnvilFork) -> None:
    """get_pools_for_token returns pools containing a given token."""
    bot = make_bot_with_provider(ProviderAdapter.from_web3(fork_mainnet_full.w3))
    tracker = CurveStableswapPoolTracker(bot=bot, chain_id=1)

    # Build tripool
    tracker.get_pool("0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7")

    # Find pools containing DAI
    dai_address = get_checksum_address("0x6B175474E89094C44Da98b954EedeAC495271d0F")
    pools_with_dai = tracker.get_pools_for_token(dai_address)
    assert len(pools_with_dai) >= 1
    assert any(p.address == "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7" for p in pools_with_dai)

    # No pools with a random token
    random_address = get_checksum_address("0x0000000000000000000000000000000000000001")
    assert tracker.get_pools_for_token(random_address) == []
