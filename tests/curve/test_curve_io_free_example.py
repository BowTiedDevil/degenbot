"""Example: Testing Curve pool calculations with I/O-free architecture.

This demonstrates how the I/O-free pattern eliminates the need for mocks
when testing pool logic. All on-chain data is injected via a CurveDataProvider.
"""

from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.erc20 import Erc20Token


class FakeCurveDataProvider:
    """A fake CurveDataProvider for testing that returns pre-programmed values."""

    def __init__(
        self,
        *,
        virtual_price: int | None = None,
        base_virtual_price: int | None = None,
        base_cache_updated: int | None = None,
        block_timestamp: int = 1_700_000_000,
        redemption_price: int | None = None,
        admin_balances: tuple[int, ...] | None = None,
        D: int | None = None,
        gamma: int | None = None,
        price_scale: tuple[int, ...] | None = None,
        lending_rates: tuple[int, ...] | None = None,
    ) -> None:
        self._virtual_price = virtual_price
        self._base_virtual_price = base_virtual_price
        self._base_cache_updated = base_cache_updated
        self._block_timestamp = block_timestamp
        self._redemption_price = redemption_price
        self._admin_balances = admin_balances
        self._D = D
        self._gamma = gamma
        self._price_scale = price_scale
        self._lending_rates = lending_rates

    def virtual_price(self, block_number: int) -> int:  # noqa: ARG002
        if self._virtual_price is None:
            msg = "virtual_price not configured"
            raise ValueError(msg)
        return self._virtual_price

    def base_virtual_price(self, block_number: int) -> int:  # noqa: ARG002
        if self._base_virtual_price is None:
            msg = "base_virtual_price not configured"
            raise ValueError(msg)
        return self._base_virtual_price

    def base_cache_updated(self, block_number: int) -> int:  # noqa: ARG002
        if self._base_cache_updated is None:
            msg = "base_cache_updated not configured"
            raise ValueError(msg)
        return self._base_cache_updated

    def block_timestamp(self, block_number: int) -> int:  # noqa: ARG002
        return self._block_timestamp

    def block_number(self) -> int:
        return 18_000_000

    def token_balance(self, token_address: str, holder_address: str, block_number: int) -> int:  # noqa: ARG002
        msg = "token_balance not configured"
        raise ValueError(msg)

    def token_total_supply(self, token_address: str, block_number: int) -> int:  # noqa: ARG002
        msg = "token_total_supply not configured"
        raise ValueError(msg)

    def lending_rates(self, block_number: int) -> tuple[int, ...]:  # noqa: ARG002
        if self._lending_rates is None:
            msg = "lending_rates not configured"
            raise ValueError(msg)
        return self._lending_rates

    def redemption_price(self, block_number: int) -> int:  # noqa: ARG002
        if self._redemption_price is None:
            msg = "redemption_price not configured"
            raise ValueError(msg)
        return self._redemption_price

    def admin_balances(self, block_number: int) -> tuple[int, ...]:  # noqa: ARG002
        if self._admin_balances is None:
            msg = "admin_balances not configured"
            raise ValueError(msg)
        return self._admin_balances

    def D(self, block_number: int) -> int:  # noqa: ARG002
        if self._D is None:
            msg = "D not configured"
            raise ValueError(msg)
        return self._D

    def gamma(self, block_number: int) -> int:  # noqa: ARG002
        if self._gamma is None:
            msg = "gamma not configured"
            raise ValueError(msg)
        return self._gamma

    def price_scale(self, block_number: int) -> tuple[int, ...]:  # noqa: ARG002
        if self._price_scale is None:
            msg = "price_scale not configured"
            raise ValueError(msg)
        return self._price_scale


def test_curve_plain_pool_with_data_provider():
    """
    Test a plain Curve pool (no lending, no base pool) with I/O-free pattern.

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
    """
    Test a lending pool (cToken/yToken) with fake lending rates via data_provider.

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
    """
    Test a metapool with fake virtual price via data_provider.

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
    """
    Test a crypto pool (volatile assets like Tricrypto) with fake D, gamma, price_scale.

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
