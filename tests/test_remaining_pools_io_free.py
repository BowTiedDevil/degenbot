"""
Tests for Phase 6: I/O-free remaining pool families.

- CamelotLiquidityPool (extends UniswapV2Pool)
- AerodromeV2Pool (standalone class)
- AerodromeV3Pool (extends UniswapV3Pool, already I/O-free via parent)
- BalancerV2Pool (standalone class)
- CurveStableswapPool (standalone class, very complex)
"""

import pathlib
import pickle
from fractions import Fraction

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.checksum_cache import get_checksum_address
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.erc20.erc20 import Erc20Token
from degenbot.registry import pool_registry


def _make_test_config(tmp_path: pathlib.Path) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: "https://eth.llamarpc.com/"},
    )


def _make_weth() -> Erc20Token:
    return Erc20Token(
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        chain_id=1,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
    )


def _make_usdc() -> Erc20Token:
    return Erc20Token(
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        chain_id=1,
        name="USD Coin",
        symbol="USDC",
        decimals=6,
    )


# ============================================================
# Camelot
# ============================================================

CAMELOT_POOL_ADDRESS = "0x6dAcEc3254F3E8C832066d3D7a3AA387fECa0BD1"
CAMELOT_FACTORY = "0x6EcCab422D763aC031210895C81787E87B53A920"


class TestCamelotIOFree:
    """CamelotLiquidityPool I/O-free construction via pre-fetched data."""

    def test_io_free_constructor(self) -> None:
        """I/O-free CamelotPool constructed with pre-fetched data."""

        weth = _make_weth()
        usdc = _make_usdc()

        fee_token0 = 5
        fee_token1 = 10
        fee_denominator = 1000

        pool = CamelotLiquidityPool(
            address=CAMELOT_POOL_ADDRESS,
            chain_id=42161,  # Arbitrum
            token0=weth,
            token1=usdc,
            factory=CAMELOT_FACTORY,
            fee_token0=fee_token0,
            fee_token1=fee_token1,
            fee_denominator=fee_denominator,
            reserves_token0=1000000,
            reserves_token1=2000000000,
            stable_swap=False,
            state_block=18_000_000,
        )

        assert pool.address == get_checksum_address(CAMELOT_POOL_ADDRESS)
        assert pool.token0 == weth
        assert pool.token1 == usdc
        assert pool.factory == get_checksum_address(CAMELOT_FACTORY)
        assert pool.fee_token0 == Fraction(fee_token0, fee_denominator)
        assert pool.fee_token1 == Fraction(fee_token1, fee_denominator)
        assert pool.fee_denominator == fee_denominator
        assert pool.stable_swap is False
        assert not hasattr(pool, "_provider")

    def test_io_free_no_self_registration(self) -> None:
        """I/O-free CamelotPool should not self-register in pool_registry."""

        pool_registry._reset()

        weth = _make_weth()
        usdc = _make_usdc()

        _ = CamelotLiquidityPool(
            address=CAMELOT_POOL_ADDRESS,
            chain_id=42161,
            token0=weth,
            token1=usdc,
            factory=CAMELOT_FACTORY,
            fee_token0=5,
            fee_token1=10,
            fee_denominator=1000,
            reserves_token0=1000000,
            reserves_token1=2000000000,
            stable_swap=False,
            state_block=18_000_000,
        )

        found = pool_registry.get(
            pool_address=CAMELOT_POOL_ADDRESS,
            chain_id=42161,
        )
        assert found is None

    def test_io_free_pickle(self) -> None:
        """I/O-free CamelotPool can be pickled and unpickled."""

        weth = _make_weth()
        usdc = _make_usdc()

        pool = CamelotLiquidityPool(
            address=CAMELOT_POOL_ADDRESS,
            chain_id=42161,
            token0=weth,
            token1=usdc,
            factory=CAMELOT_FACTORY,
            fee_token0=5,
            fee_token1=10,
            fee_denominator=1000,
            reserves_token0=1000000,
            reserves_token1=2000000000,
            stable_swap=False,
            state_block=18_000_000,
        )

        data = pickle.dumps(pool)
        unpickled = pickle.loads(data)
        assert unpickled.address == pool.address
        assert unpickled.stable_swap == pool.stable_swap
        assert not hasattr(unpickled, "_provider")


# ============================================================
# Aerodrome V2
# ============================================================

AERO_V2_POOL_ADDRESS = "0x4bdB2C3E46FD7E6Bb0d1D3F7D6c53cB1e9F05e19"
AERO_V2_FACTORY = "0x420DD381b31aEf6683db6B902084cB0FFecfc406"


class TestAerodromeV2IOFree:
    """AerodromeV2Pool I/O-free construction via pre-fetched data."""

    def test_io_free_constructor(self) -> None:
        """I/O-free AerodromeV2Pool constructed with pre-fetched data."""

        weth = _make_weth()
        usdc = _make_usdc()

        pool = AerodromeV2Pool(
            address=AERO_V2_POOL_ADDRESS,
            chain_id=10,  # Optimism
            token0=weth,
            token1=usdc,
            factory=AERO_V2_FACTORY,
            fee=Fraction(3, 1000),
            stable=False,
            reserves_token0=1000000,
            reserves_token1=2000000000,
            state_block=18_000_000,
        )

        assert pool.address == get_checksum_address(AERO_V2_POOL_ADDRESS)
        assert pool.token0 == weth
        assert pool.token1 == usdc
        assert pool.factory == get_checksum_address(AERO_V2_FACTORY)
        assert pool.fee == Fraction(3, 1000)
        assert pool.stable is False
        assert not hasattr(pool, "_provider")

    def test_io_free_no_self_registration(self) -> None:
        """I/O-free AerodromeV2Pool should not self-register in pool_registry."""

        pool_registry._reset()

        weth = _make_weth()
        usdc = _make_usdc()

        pool = AerodromeV2Pool(
            address=AERO_V2_POOL_ADDRESS,
            chain_id=10,
            token0=weth,
            token1=usdc,
            factory=AERO_V2_FACTORY,
            fee=Fraction(3, 1000),
            stable=False,
            reserves_token0=1000000,
            reserves_token1=2000000000,
            state_block=18_000_000,
        )

        found = pool_registry.get(
            pool_address=AERO_V2_POOL_ADDRESS,
            chain_id=10,
        )
        assert found is None

    def test_io_free_pickle(self) -> None:
        """I/O-free AerodromeV2Pool can be pickled and unpickled."""

        weth = _make_weth()
        usdc = _make_usdc()

        pool = AerodromeV2Pool(
            address=AERO_V2_POOL_ADDRESS,
            chain_id=10,
            token0=weth,
            token1=usdc,
            factory=AERO_V2_FACTORY,
            fee=Fraction(3, 1000),
            stable=False,
            reserves_token0=1000000,
            reserves_token1=2000000000,
            state_block=18_000_000,
        )

        data = pickle.dumps(pool)
        unpickled = pickle.loads(data)
        assert unpickled.address == pool.address
        assert unpickled.stable == pool.stable
        assert not hasattr(unpickled, "_provider")


# ============================================================
# Balancer V2
# ============================================================

BALANCER_POOL_ADDRESS = "0x5c6Ee304399DBdB9C8Ef030aB642B10820DB8F56"
BALANCER_VAULT = "0xBA12222222228d8Ba4219b601842Db6516a6B85A"


class TestBalancerV2IOFree:
    """BalancerV2Pool I/O-free construction via pre-fetched data."""

    def test_io_free_constructor(self) -> None:
        """I/O-free BalancerV2Pool constructed with pre-fetched data."""

        weth = _make_weth()
        usdc = _make_usdc()

        pool = BalancerV2Pool(
            address=BALANCER_POOL_ADDRESS,
            chain_id=1,
            pool_id=bytes.fromhex(
                "5c6ee304399dbdb9c8ef030ab642b10820db8f56000000000000000000000000"
            ),
            vault=BALANCER_VAULT,
            tokens=(weth, usdc),
            balances=(1000000000000000000, 2000000000),
            fee=Fraction(3, 1000),
            weights=(500000000000000000, 500000000000000000),  # 50/50
            state_block=18_000_000,
        )

        assert pool.address == get_checksum_address(BALANCER_POOL_ADDRESS)
        assert pool.tokens == (weth, usdc)
        assert pool.balances == (1000000000000000000, 2000000000)
        assert pool.fee == Fraction(3, 1000)
        assert not hasattr(pool, "_provider")

    def test_io_free_pickle(self) -> None:
        """I/O-free BalancerV2Pool can be pickled and unpickled."""

        weth = _make_weth()
        usdc = _make_usdc()

        pool = BalancerV2Pool(
            address=BALANCER_POOL_ADDRESS,
            chain_id=1,
            pool_id=bytes.fromhex(
                "5c6ee304399dbdb9c8ef030ab642b10820db8f56000000000000000000000000"
            ),
            vault=BALANCER_VAULT,
            tokens=(weth, usdc),
            balances=(1000000000000000000, 2000000000),
            fee=Fraction(3, 1000),
            weights=(500000000000000000, 500000000000000000),
            state_block=18_000_000,
        )

        data = pickle.dumps(pool)
        unpickled = pickle.loads(data)
        assert unpickled.address == pool.address
        assert not hasattr(unpickled, "_provider")


# ============================================================
# Curve Stableswap
# ============================================================

CURVE_POOL_ADDRESS = "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"


class TestCurveIOFree:
    """CurveStableswapPool I/O-free construction via pre-fetched data."""

    def test_io_free_constructor(self) -> None:
        """I/O-free CurveStableswapPool constructed with pre-fetched data."""

        dai = Erc20Token(
            "0x6B175474E89094C44Da98b954EedeAC495271d0F",
            chain_id=1,
            name="Dai Stablecoin",
            symbol="DAI",
            decimals=18,
        )
        usdc = Erc20Token(
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            chain_id=1,
            name="USD Coin",
            symbol="USDC",
            decimals=6,
        )
        usdt = Erc20Token(
            "0xdAC17F958D2ee523a2206206994597C13D831ec7",
            chain_id=1,
            name="Tether USD",
            symbol="USDT",
            decimals=6,
        )

        pool = CurveStableswapPool(
            address=CURVE_POOL_ADDRESS,
            chain_id=1,
            tokens=(dai, usdc, usdt),
            a_coefficient=2000,
            fee=4000000,
            admin_fee=5000000000,
            balances=(1000000000000000000, 1000000000, 1000000000),
            state_block=18_000_000,
        )

        assert pool.address == get_checksum_address(CURVE_POOL_ADDRESS)
        assert len(pool.tokens) == 3
        assert not hasattr(pool, "_provider")

    def test_io_free_pickle(self) -> None:
        """I/O-free CurveStableswapPool can be pickled and unpickled."""

        dai = Erc20Token(
            "0x6B175474E89094C44Da98b954EedeAC495271d0F",
            chain_id=1,
            name="Dai Stablecoin",
            symbol="DAI",
            decimals=18,
        )
        usdc = Erc20Token(
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            chain_id=1,
            name="USD Coin",
            symbol="USDC",
            decimals=6,
        )
        usdt = Erc20Token(
            "0xdAC17F958D2ee523a2206206994597C13D831ec7",
            chain_id=1,
            name="Tether USD",
            symbol="USDT",
            decimals=6,
        )

        pool = CurveStableswapPool(
            address=CURVE_POOL_ADDRESS,
            chain_id=1,
            tokens=(dai, usdc, usdt),
            a_coefficient=2000,
            fee=4000000,
            admin_fee=5000000000,
            balances=(1000000000000000000, 1000000000, 1000000000),
            state_block=18_000_000,
        )

        data = pickle.dumps(pool)
        unpickled = pickle.loads(data)
        assert unpickled.address == pool.address
        # Pickle reconstructs _provider=None from _pickle_reconstructs,
        # but it should be None (not a real provider)
        assert unpickled._provider is None
