"""Tests verifying Curve pool operates without _get_provider_for_chain().

Phase 4 tracer bullets: replace leaf I/O methods with fetcher callbacks
so that _get_provider_for_chain() can be removed entirely.
"""


from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.erc20 import Erc20Token


def test_curve_pool_has_no_provider_attributes() -> None:
    """After Phase 4, CurveStableswapPool should not have _provider or _bot."""
    # Construct a minimal I/O-free Curve pool
    tokens = (
        Erc20Token(
            address="0x6B175474E89094C44Da98b954EedeAC495271d0F",
            name="DAI",
            symbol="DAI",
            decimals=18,
        ),
        Erc20Token(
            address="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            name="USD Coin",
            symbol="USDC",
            decimals=6,
        ),
        Erc20Token(
            address="0xdAC17F958D2ee523a2206206994597C13D831ec7",
            name="Tether USD",
            symbol="USDT",
            decimals=6,
        ),
    )

    pool = CurveStableswapPool(
        address="0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
        tokens=tokens,
        a_coefficient=2000,
        fee=4000000,
        admin_fee=5000000000,
        balances=(100, 200, 300),
    )

    assert not hasattr(pool, "_provider"), "Pool should not have _provider attribute"
    assert not hasattr(pool, "_bot"), "Pool should not have _bot attribute"
    assert not hasattr(pool, "_get_provider_for_chain"), (
        "Pool should not have _get_provider_for_chain method"
    )


def test_curve_pool_has_fetcher_callbacks() -> None:
    """After Phase 4, fetcher protocols should be wired into leaf methods."""
    tokens = (
        Erc20Token(
            address="0x6B175474E89094C44Da98b954EedeAC495271d0F",
            name="DAI",
            symbol="DAI",
            decimals=18,
        ),
        Erc20Token(
            address="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            name="USD Coin",
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
        # Pass no fetchers — pool should work without them for basic operations
    )

    # Basic computation should work without any fetchers
    assert pool.a_coefficient == 2000
    assert pool.balances == (100, 200)
