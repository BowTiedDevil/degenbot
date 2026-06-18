"""Tests for FakeToken interoperability with Erc20Token and production pool constructors.

These tests validate:
1. FakeToken ↔ Erc20Token equality/hash/dict-key compatibility (AddressComparable semantics)
2. Canary tests: constructing each production pool class with FakeToken arguments,
   ensuring future attribute drift is caught
"""

from fractions import Fraction

import pytest

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.degenbot_rs import PyBot
from degenbot.types.address_comparable import AddressComparable
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from tests.fakes.tokens import FakeToken
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

_PY_BOT = PyBot()

# ── Addresses used across tests ──

USDC_ADDRESS = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
WBTC_ADDRESS = get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
DAI_ADDRESS = get_checksum_address("0x6B175474E89094C44Da98b954EedeAC495271d0F")


class TestFakeTokenErc20Interoperability:
    """Validate FakeToken ↔ Erc20Token equality, hashing, and dict-key interchangeability."""

    def test_fake_token_equals_fake_token_by_address(self) -> None:
        a = FakeToken(USDC_ADDRESS, symbol="USDC", decimals=6)
        b = FakeToken(USDC_ADDRESS, symbol="USDC", decimals=6)
        assert a == b

    def test_fake_token_not_equal_different_address(self) -> None:
        a = FakeToken(USDC_ADDRESS, symbol="USDC", decimals=6)
        b = FakeToken(WETH_ADDRESS, symbol="WETH", decimals=18)
        assert a != b

    def test_erc20_token_equals_fake_token_by_address(self) -> None:
        fake = FakeToken(USDC_ADDRESS, symbol="USDC", decimals=6)
        erc20 = make_erc20(
            _PY_BOT,
            USDC_ADDRESS,
            name="USD Coin",
            symbol="USDC",
            decimals=6,
            chain_id=1,
        )
        assert fake == erc20
        assert erc20 == fake

    def test_hash_compatibility_fake_token(self) -> None:
        a = FakeToken(USDC_ADDRESS, symbol="USDC", decimals=6)
        b = FakeToken(USDC_ADDRESS, symbol="USDC", decimals=6)
        assert hash(a) == hash(b)

    def test_hash_compatibility_erc20_token(self) -> None:
        fake = FakeToken(USDC_ADDRESS, symbol="USDC", decimals=6)
        erc20 = make_erc20(
            _PY_BOT,
            USDC_ADDRESS,
            name="USD Coin",
            symbol="USDC",
            decimals=6,
            chain_id=1,
        )
        assert hash(fake) == hash(erc20)

    def test_dict_key_interchangeability(self) -> None:
        """FakeToken and Erc20Token with the same address should map to the same dict key."""
        fake = FakeToken(USDC_ADDRESS, symbol="USDC", decimals=6)
        erc20 = make_erc20(
            _PY_BOT,
            USDC_ADDRESS,
            name="USD Coin",
            symbol="USDC",
            decimals=6,
            chain_id=1,
        )
        d: dict[object, str] = {fake: "via_fake"}
        # Erc20Token should overwrite the same key
        d[erc20] = "via_erc20"
        assert len(d) == 1
        assert d[fake] == "via_erc20"

    def test_set_membership(self) -> None:
        fake = FakeToken(USDC_ADDRESS, symbol="USDC", decimals=6)
        erc20 = make_erc20(
            _PY_BOT,
            USDC_ADDRESS,
            name="USD Coin",
            symbol="USDC",
            decimals=6,
            chain_id=1,
        )
        s = {fake}
        assert erc20 in s

    def test_ordering(self) -> None:
        """FakeToken inherits AddressComparable ordering."""
        a = FakeToken(USDC_ADDRESS, symbol="USDC", decimals=6)
        b = FakeToken(WETH_ADDRESS, symbol="WETH", decimals=18)
        # Checksummed addresses are lexicographically ordered
        assert (a < b) != (a > b)

    def test_name_attribute(self) -> None:
        token = FakeToken(USDC_ADDRESS, name="USD Coin", symbol="USDC", decimals=6)
        assert token.name == "USD Coin"

    def test_default_name(self) -> None:
        token = FakeToken(USDC_ADDRESS)
        assert token.name == "Token"

    def test_str_returns_symbol(self) -> None:
        """FakeToken.__str__ returns self.symbol, matching AbstractErc20Token.__str__."""
        token = FakeToken(USDC_ADDRESS, symbol="USDC", decimals=6)
        assert str(token) == "USDC"

    def test_isinstance_address_comparable(self) -> None:
        token = FakeToken(USDC_ADDRESS, symbol="USDC", decimals=6)
        assert isinstance(token, AddressComparable)

    def test_frozen(self) -> None:
        token = FakeToken(USDC_ADDRESS, symbol="USDC", decimals=6)
        with pytest.raises(AttributeError):
            token.symbol = "NEW"  # type: ignore[misc]


class TestCanaryV2PoolWithFakeToken:
    """Canary: LiquidityPool construction with FakeToken."""

    def test_construct_v2_pool(self) -> None:  # type: ignore[arg-type]
        token0 = FakeToken(WBTC_ADDRESS, name="Wrapped BTC", symbol="WBTC", decimals=8)
        token1 = FakeToken(WETH_ADDRESS, name="Wrapped Ether", symbol="WETH", decimals=18)
        pool = make_v2_pool(
            address="0xBb2b8038a1640196FbE3e38816F3e67Cba72D940",
            token0=token0,  # type: ignore[arg-type]
            token1=token1,  # type: ignore[arg-type]
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000000,
            reserves_token1=500000000000000000,
        )
        assert pool.token0 == token0
        assert pool.token1 == token1


class TestCanaryV3PoolWithFakeToken:
    """Canary: UniswapV3Pool construction with FakeToken."""

    def test_construct_v3_pool(self) -> None:
        token0 = FakeToken(WBTC_ADDRESS, name="Wrapped BTC", symbol="WBTC", decimals=8)
        token1 = FakeToken(WETH_ADDRESS, name="Wrapped Ether", symbol="WETH", decimals=18)
        pool = UniswapV3Pool(
            address="0xCBCdF9626bC03E24f779434178A73a0B4bad62eD",
            token0=token0,  # type: ignore[arg-type]
            token1=token1,  # type: ignore[arg-type]
            factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
            fee=3000,
            tick_spacing=60,
            sqrt_price_x96=0xFFFD8963F8FC4E06,
            tick=0,
            liquidity=0,
        )
        assert pool.token0 == token0
        assert pool.token1 == token1


class TestCanaryAerodromeV2PoolWithFakeToken:
    """Canary: AerodromeV2Pool construction with FakeToken."""

    def test_construct_aerodrome_pool(self) -> None:
        token0 = FakeToken(WBTC_ADDRESS, name="Wrapped BTC", symbol="WBTC", decimals=8)
        token1 = FakeToken(WETH_ADDRESS, name="Wrapped Ether", symbol="WETH", decimals=18)
        pool = AerodromeV2Pool(
            address="0x0000000000000000000000000000000000000001",
            token0=token0,  # type: ignore[arg-type]
            token1=token1,  # type: ignore[arg-type]
            factory="0x0000000000000000000000000000000000000002",
            fee=Fraction(3, 1000),
            stable=False,
            reserves_token0=1000000,
            reserves_token1=500000000000000000,
        )
        assert pool.token0 == token0
        assert pool.token1 == token1


class TestCanaryCurveStableswapPoolWithFakeToken:
    """Canary: CurveStableswapPool construction with FakeToken."""

    def test_construct_curve_pool(self) -> None:
        dai = FakeToken(DAI_ADDRESS, name="Dai Stablecoin", symbol="DAI", decimals=18)
        usdc = FakeToken(USDC_ADDRESS, name="USD Coin", symbol="USDC", decimals=6)
        pool = CurveStableswapPool(
            address="0xbEbc44782C7db0a1A60Cb6fe97d0b483032FF1C7",
            tokens=[dai, usdc],  # type: ignore[arg-type]
            a_coefficient=1000,
            fee=4_000_000,
            admin_fee=5_000_000_000,
            balances=(10_000_000 * 10**18, 10_000_000 * 10**6),
        )
        assert pool.tokens[0] == dai
        assert pool.tokens[1] == usdc
