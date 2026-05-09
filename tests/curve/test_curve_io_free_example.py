"""Example: Testing Curve pool calculations with I/O-free architecture.

This demonstrates how the I/O-free pattern eliminates the need for mocks
when testing pool logic.
"""

import eth_abi.abi

from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.erc20 import Erc20Token


def test_curve_plain_pool_with_lambda_fetchers():
    """
    Test a plain Curve pool (no lending, no base pool) with I/O-free pattern.

    NO MOCKS NEEDED - just pass lambda fetchers for any on-chain data.
    """
    dai = Erc20Token(
        address="0x6B175474E89094C44Da98b954EedeAC495271d0F",
        name="DAI",
        symbol="DAI",
        decimals=18,
    )
    usdc = Erc20Token(
        address="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        name="USD Coin",
        symbol="USDC",
        decimals=6,
    )

    # I/O-free pool - inject fake data directly
    # All pools need timestamp_fetcher for A coefficient ramping calculations
    def fake_timestamp_fetcher(
        block,  # noqa: ARG001
    ):
        return 1700000000  # Fake timestamp

    pool = CurveStableswapPool(
        address="0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
        tokens=(dai, usdc),
        a_coefficient=2000,
        fee=4000000,  # 0.04%
        admin_fee=5000000000,
        balances=(
            10_000_000 * 10**18,  # 10M DAI
            10_000_000 * 10**6,  # 10M USDC
        ),
        # Set state block to match the block we'll use for calculations
        state_block=18_000_000,
        # Inject timestamp fetcher - needed for all pools
        timestamp_fetcher=fake_timestamp_fetcher,
    )

    # Test swap calculation - pass explicit block number (I/O-free)
    # No network, no mocks, just math
    amount_in = 1000 * 10**18  # 1000 DAI
    result = pool.get_dy(0, 1, amount_in, block_identifier=18_000_000)

    assert result > 0
    # Result is in USDC (6 decimals), input was DAI (18 decimals)
    # At equilibrium with equal balances, 1000 DAI -> ~999.6 USDC (after 0.04% fee)
    # Result in USDC units: ~999,600,000 (6 decimals)
    expected_approx = 999 * 10**6  # ~999 USDC
    assert result > expected_approx * 0.99  # Close to expected (stable swap)
    assert result < expected_approx * 1.01


def test_curve_lending_pool_with_provider_call():
    """
    Test a lending pool (cToken/yToken) with fake provider_call callback.

    BEFORE: Would need to mock the provider object
    AFTER: Pass a lambda that mimics provider.call() for rate fetching.

    Note: Lending pools use provider_call (low-level) instead of typed fetchers
    because rate fetching requires pool-specific decoding logic.
    """
    cdai = Erc20Token(
        address="0x5d3a536E4D6DbD6114cc1Ead35777bAB948E3643",
        name="Compound DAI",
        symbol="cDAI",
        decimals=8,  # cTokens have different decimals
    )
    cusdc = Erc20Token(
        address="0x39AA39c021dfbaE8faC545936693aC917d5E7563",
        name="Compound USDC",
        symbol="cUSDC",
        decimals=8,
    )

    # Fake provider_call - mimics w3.eth.call() for rate fetching
    # The pool will call this with (to, data, block) for exchangeRateStored()
    PRECISION = 10**18

    def fake_provider_call(*, to: str, data: bytes, block: int) -> bytes:
        # Decode the function selector to know what's being called
        # For simplicity, just return fake encoded rates

        # Return rate = 1.02e18 for cDAI, 1.05e18 for cUSDC
        if to.lower() == cdai.address.lower():
            return eth_abi.abi.encode(["uint256"], [PRECISION * 102 // 100])
        if to.lower() == cusdc.address.lower():
            return eth_abi.abi.encode(["uint256"], [PRECISION * 105 // 100])
        return eth_abi.abi.encode(["uint256"], [PRECISION])

    # Fake timestamp fetcher (needed for all pools)
    def fake_timestamp_fetcher(
        block,  # noqa: ARG001
    ):
        return 1700000000

    pool = CurveStableswapPool(
        address="0x0000000000000000000000000000000000000001",  # Valid address
        tokens=(cdai, cusdc),
        a_coefficient=1000,
        fee=4000000,
        admin_fee=5000000000,
        balances=(
            1_000_000 * 10**8,  # 1M cDAI
            1_000_000 * 10**8,  # 1M cUSDC
        ),
        # Set state block to match the block we'll use for calculations
        state_block=18_000_000,
        # Mark tokens as lending
        use_lending=(True, True),
        # Inject fake fetchers - NO MOCKS!
        provider_call=fake_provider_call,
        timestamp_fetcher=fake_timestamp_fetcher,
    )

    # Test calculation with fake rates
    amount_in = 100 * 10**8  # 100 cDAI
    result = pool.get_dy(0, 1, amount_in, block_identifier=18_000_000)

    assert result > 0
    # Rate difference affects the output


def test_curve_metapool_with_virtual_price_fetcher():
    """
    Test a metapool with fake virtual price fetcher.

    BEFORE: Would need to mock provider.call() for base_pool.get_virtual_price()
    AFTER: Just pass a lambda that returns fake virtual price.
    """
    rai = Erc20Token(
        address="0x81ab848898b15A779B7cd0cB2cDd406c64EFc12c",
        name="Rai Reflex Index",
        symbol="RAI",
        decimals=18,
    )
    # 3Crv LP token (3Pool token)
    threecrv = Erc20Token(
        address="0x6c3F90f043a72FA612CbAC8115EEe7f52CdE6E49",  # Fixed address (even length)
        name="Curve 3Pool Token",
        symbol="3Crv",
        decimals=18,
    )

    # Fake virtual price fetcher - returns LP token price
    PRECISION = 10**18
    fake_vp_fetcher = lambda block: PRECISION * 102 // 100  # 1.02

    # Fake timestamp fetcher (needed for all pools)
    def fake_timestamp_fetcher(
        block,  # noqa: ARG001
    ):
        return 1700000000

    pool = CurveStableswapPool(
        address="0x618788357D0EBd8A37e763ADab3bc575D54c2C7d",
        tokens=(rai, threecrv),
        a_coefficient=400,
        fee=4000000,
        admin_fee=5000000000,
        balances=(
            5_000_000 * 10**18,  # 5M RAI
            10_000_000 * 10**18,  # 10M 3Crv LP tokens
        ),
        # Set state block to match the block we'll use for calculations
        state_block=18_000_000,
        # Inject fake fetchers - NO MOCKS!
        base_virtual_price_fetcher=fake_vp_fetcher,
        timestamp_fetcher=fake_timestamp_fetcher,
    )

    # Test calculation with fake virtual price
    amount_in = 1000 * 10**18  # 1000 RAI
    result = pool.get_dy(0, 1, amount_in, block_identifier=18_000_000)

    assert result > 0


def test_curve_crypto_pool_with_d_fetcher():
    """
    Test a crypto pool (volatile assets like Tricrypto) with fake D fetcher.

    Crypto pools need the on-chain invariant D value.
    BEFORE: Would need to mock provider.call() for D()
    AFTER: Just pass a lambda.
    """
    wbtc = Erc20Token(
        address="0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",
        name="Wrapped BTC",
        symbol="WBTC",
        decimals=8,
    )
    weth = Erc20Token(
        address="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
    )

    # Fake D fetcher - returns on-chain invariant
    def fake_d_fetcher(
        block,  # noqa: ARG001
    ):
        return 10**20  # Some large D value

    # Fake gamma fetcher
    def fake_gamma_fetcher(
        block,  # noqa: ARG001
    ):
        return 10**16  # Gamma parameter

    # Fake price scale fetcher (for multi-asset pools)
    def fake_price_scale_fetcher(
        block,  # noqa: ARG001
    ):
        return (10**18,)

    # Fake timestamp fetcher (needed for all pools)
    def fake_timestamp_fetcher(
        block,  # noqa: ARG001
    ):
        return 1700000000

    pool = CurveStableswapPool(
        address="0x0000000000000000000000000000000000000002",  # Valid address
        tokens=(wbtc, weth),
        a_coefficient=400,  # Crypto pools still have A parameter
        fee=10000000,  # Higher fee for crypto pool
        admin_fee=5000000000,
        balances=(
            100 * 10**8,  # 100 WBTC
            2000 * 10**18,  # 2000 WETH
        ),
        # Set state block to match the block we'll use for calculations
        state_block=18_000_000,
        # Crypto pool parameters (non-zero fee_gamma indicates crypto pool)
        fee_gamma=500000000000000,
        mid_fee=3000000,
        out_fee=30000000,
        gamma=10**16,  # Gamma parameter
        # Inject fake fetchers - NO MOCKS!
        D_fetcher=fake_d_fetcher,
        gamma_fetcher=fake_gamma_fetcher,
        price_scale_fetcher=fake_price_scale_fetcher,
        timestamp_fetcher=fake_timestamp_fetcher,
    )

    # Test calculation
    amount_in = 10**8  # 1 WBTC
    result = pool.get_dy(0, 1, amount_in, block_identifier=18_000_000)

    assert result > 0
