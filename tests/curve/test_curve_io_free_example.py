"""Example: Testing Curve pool calculations with I/O-free architecture.

This demonstrates how the I/O-free pattern eliminates the need for mocks
when testing pool logic. All on-chain data is injected via a CurveDataProvider.
"""

from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.erc20 import Erc20Token
from tests.fakes.curve_data_provider import FakeCurveDataProvider


def test_curve_plain_pool_with_data_provider():
    """Test a plain Curve pool (no lending, no base pool) with I/O-free pattern.

    NO MOCKS NEEDED - just pass a data_provider with fake on-chain data.
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

    provider = FakeCurveDataProvider(
        block_timestamp=1_700_000_000,
    )

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
        state_block=18_000_000,
        data_provider=provider,
    )

    # Test swap calculation - pass explicit block number (I/O-free)
    amount_in = 1000 * 10**18  # 1000 DAI
    result = pool.get_dy(0, 1, amount_in, block_identifier=18_000_000)

    assert result > 0
    expected_approx = 999 * 10**6  # ~999 USDC
    assert result > expected_approx * 0.99
    assert result < expected_approx * 1.01


def test_curve_lending_pool_with_data_provider():
    """Test a lending pool (cToken/yToken) with fake lending rates via data_provider.

    The data_provider bundles all on-chain data access behind a single seam.
    """
    cdai = Erc20Token(
        address="0x5d3a536E4D6DbD6114cc1Ead35777bAB948E3643",
        name="Compound DAI",
        symbol="cDAI",
        decimals=8,
    )
    cusdc = Erc20Token(
        address="0x39AA39c021dfbaE8faC545936693aC917d5E7563",
        name="Compound USDC",
        symbol="cUSDC",
        decimals=8,
    )

    PRECISION = 10**18

    provider = FakeCurveDataProvider(
        block_timestamp=1_700_000_000,
        lending_rates=(PRECISION * 102 // 100, PRECISION * 105 // 100),
    )

    pool = CurveStableswapPool(
        address="0x0000000000000000000000000000000000000001",
        tokens=(cdai, cusdc),
        a_coefficient=1000,
        fee=4000000,
        admin_fee=5000000000,
        balances=(
            1_000_000 * 10**8,  # 1M cDAI
            1_000_000 * 10**8,  # 1M cUSDC
        ),
        state_block=18_000_000,
        use_lending=(True, True),
        data_provider=provider,
    )

    amount_in = 100 * 10**8  # 100 cDAI
    result = pool.get_dy(0, 1, amount_in, block_identifier=18_000_000)

    assert result > 0


def test_curve_metapool_with_data_provider():
    """Test a metapool with fake virtual price via data_provider.

    Just configure the provider with a base_virtual_price value.
    """
    rai = Erc20Token(
        address="0x81ab848898b15A779B7cd0cB2cDd406c64EFc12c",
        name="Rai Reflex Index",
        symbol="RAI",
        decimals=18,
    )
    threecrv = Erc20Token(
        address="0x6c3F90f043a72FA612CbAC8115EEe7f52CdE6E49",
        name="Curve 3Pool Token",
        symbol="3Crv",
        decimals=18,
    )

    PRECISION = 10**18

    provider = FakeCurveDataProvider(
        block_timestamp=1_700_000_000,
        base_virtual_price=PRECISION * 102 // 100,  # 1.02
    )

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
        state_block=18_000_000,
        data_provider=provider,
    )

    amount_in = 1000 * 10**18  # 1000 RAI
    result = pool.get_dy(0, 1, amount_in, block_identifier=18_000_000)

    assert result > 0


def test_curve_crypto_pool_with_data_provider():
    """Test a crypto pool (volatile assets like Tricrypto) with fake D, gamma, price_scale.

    Crypto pools need the on-chain invariant D value, gamma, and price_scale.
    All provided through the single data_provider seam.
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

    provider = FakeCurveDataProvider(
        block_timestamp=1_700_000_000,
        D=10**20,
        gamma=10**16,
        price_scale=(10**18,),
    )

    pool = CurveStableswapPool(
        address="0x0000000000000000000000000000000000000002",
        tokens=(wbtc, weth),
        a_coefficient=400,
        fee=10000000,
        admin_fee=5000000000,
        balances=(
            100 * 10**8,  # 100 WBTC
            2000 * 10**18,  # 2000 WETH
        ),
        state_block=18_000_000,
        fee_gamma=500000000000000,
        mid_fee=3000000,
        out_fee=30000000,
        gamma=10**16,
        data_provider=provider,
    )

    amount_in = 10**8  # 1 WBTC
    result = pool.get_dy(0, 1, amount_in, block_identifier=18_000_000)

    assert result > 0
