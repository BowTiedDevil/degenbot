"""
Tests that every concrete pool type can be constructed I/O-free and returns
the correct concrete type.

Pool addresses and token data come from the project's reference database
(degenbot.db.bak). All pools are on Ethereum mainnet (chain_id=1).
"""

from fractions import Fraction

import pytest

from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20.erc20 import Erc20Token
from degenbot.pancakeswap.pools import PancakeswapV2Pool, PancakeswapV3Pool
from degenbot.sushiswap.pools import SushiswapV2Pool, SushiswapV3Pool
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.types.pool_type import PoolFamily
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

# ---------------------------------------------------------------------------
# Shared tokens (Ethereum mainnet)
# ---------------------------------------------------------------------------


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


def _make_usdt() -> Erc20Token:
    return Erc20Token(
        "0xdAC17F958D2ee523a2206206994597C13D831ec7",
        chain_id=1,
        name="Tether USD",
        symbol="USDT",
        decimals=6,
    )


def _make_sushi() -> Erc20Token:
    return Erc20Token(
        "0x6B3595068778DD592e39A122f4f5a5cF09C90fE2",
        chain_id=1,
        name="SushiSwap",
        symbol="SUSHI",
        decimals=18,
    )


def _make_uni() -> Erc20Token:
    return Erc20Token(
        "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984",
        chain_id=1,
        name="Uniswap",
        symbol="UNI",
        decimals=18,
    )


def _make_dai() -> Erc20Token:
    return Erc20Token(
        "0x6B175474E89094C44Da98b954EedeAC495271d0F",
        chain_id=1,
        name="Dai Stablecoin",
        symbol="DAI",
        decimals=18,
    )


# ---------------------------------------------------------------------------
# Factory addresses (Ethereum mainnet)
# ---------------------------------------------------------------------------

UNISWAP_V2_FACTORY = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
UNISWAP_V3_FACTORY = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
SUSHISWAP_V2_FACTORY = "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"
SUSHISWAP_V3_FACTORY = "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F"
PANCAKESWAP_V2_FACTORY = "0x1097053Fd2ea711dad45caCcc45EfF7548fCB362"
PANCAKESWAP_V3_FACTORY = "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"

V4_POOL_MANAGER = "0x000000000004444c5dc75cB358380D2e3dE08A90"


# ---------------------------------------------------------------------------
# Pool construction helpers
# ---------------------------------------------------------------------------


def _make_uniswap_v2_pool() -> UniswapV2Pool:
    weth = _make_weth()
    usdc = _make_usdc()

    return UniswapV2Pool(
        address="0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc",
        chain_id=1,
        token0=usdc,
        token1=weth,
        factory=UNISWAP_V2_FACTORY,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=2_000_000 * 10**6,
        reserves_token1=1000 * 10**18,
        state_block=18_000_000,
    )


def _make_sushiswap_v2_pool() -> SushiswapV2Pool:
    sushi = _make_sushi()
    usdt = _make_usdt()

    return SushiswapV2Pool(
        address="0x680A025Da7b1be2c204D7745e809919bCE074026",
        chain_id=1,
        token0=sushi,
        token1=usdt,
        factory=SUSHISWAP_V2_FACTORY,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=500_000 * 10**18,
        reserves_token1=1_000_000 * 10**6,
        state_block=18_000_000,
    )


def _make_pancakeswap_v2_pool() -> PancakeswapV2Pool:
    weth = _make_weth()
    usdc = _make_usdc()

    return PancakeswapV2Pool(
        address="0x2E8135bE71230c6B1B4045696d41C09Db0414226",
        chain_id=1,
        token0=usdc,
        token1=weth,
        factory=PANCAKESWAP_V2_FACTORY,
        fee_token0=Fraction(25, 10000),
        fee_token1=Fraction(25, 10000),
        reserves_token0=1_000_000 * 10**6,
        reserves_token1=500 * 10**18,
        state_block=18_000_000,
    )


def _make_uniswap_v3_pool() -> UniswapV3Pool:
    weth = _make_weth()
    uni = _make_uni()

    return UniswapV3Pool(
        address="0x1d42064Fc4Beb5F8aAF85F4617AE8b3b5B8Bd801",
        chain_id=1,
        token0=uni,
        token1=weth,
        factory=UNISWAP_V3_FACTORY,
        fee=3000,
        tick_spacing=60,
        sqrt_price_x96=5_187_054_295_086_245_983_372_581,
        tick=-76020,
        liquidity=1234567890,
        state_block=18_000_000,
    )


def _make_sushiswap_v3_pool() -> SushiswapV3Pool:
    weth = _make_weth()
    usdc = _make_usdc()

    return SushiswapV3Pool(
        address="0x35644Fb61aFBc458bf92B15AdD6ABc1996Be5014",
        chain_id=1,
        token0=usdc,
        token1=weth,
        factory=SUSHISWAP_V3_FACTORY,
        fee=500,
        tick_spacing=10,
        sqrt_price_x96=2_198_666_895_605_149_686_863,
        tick=-76020,
        liquidity=9876543210,
        state_block=18_000_000,
    )


def _make_pancakeswap_v3_pool() -> PancakeswapV3Pool:
    usdc = _make_usdc()
    dai = _make_dai()

    return PancakeswapV3Pool(
        address="0xD9e497BD8f491fE163b42A62c296FB54CaEA74B7",
        chain_id=1,
        token0=dai,
        token1=usdc,
        factory=PANCAKESWAP_V3_FACTORY,
        fee=100,
        tick_spacing=1,
        sqrt_price_x96=79_228_162_514_264_337_593_543_950_336,
        tick=0,
        liquidity=5_000_000_000_000,
        state_block=18_000_000,
    )


def _make_uniswap_v4_pool() -> UniswapV4Pool:
    weth = _make_weth()
    usdc = _make_usdc()

    return UniswapV4Pool(
        pool_id="0x4f88f7c99022eace4740c6898f59ce6a2e798a1e64ce54589720b7153eb224a7",
        pool_manager_address=V4_POOL_MANAGER,
        token0=usdc,
        token1=weth,
        fee=500,
        tick_spacing=10,
        hook_address=ZERO_ADDRESS,
        sqrt_price_x96=2_198_666_895_605_149_686_863,
        tick=-76020,
        liquidity=9876543210,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=500000,
        state_block=18_000_000,
    )


# ---------------------------------------------------------------------------
# Parametrized pool list: (constructor, expected_type, pool_family)
# ---------------------------------------------------------------------------

MAINNET_POOLS: list[tuple[callable, type, PoolFamily]] = [
    (_make_uniswap_v2_pool, UniswapV2Pool, PoolFamily.CONSTANT_PRODUCT),
    (_make_sushiswap_v2_pool, SushiswapV2Pool, PoolFamily.CONSTANT_PRODUCT),
    (_make_pancakeswap_v2_pool, PancakeswapV2Pool, PoolFamily.CONSTANT_PRODUCT),
    (_make_uniswap_v3_pool, UniswapV3Pool, PoolFamily.CONCENTRATED_LIQUIDITY),
    (_make_sushiswap_v3_pool, SushiswapV3Pool, PoolFamily.CONCENTRATED_LIQUIDITY),
    (_make_pancakeswap_v3_pool, PancakeswapV3Pool, PoolFamily.CONCENTRATED_LIQUIDITY),
    (_make_uniswap_v4_pool, UniswapV4Pool, PoolFamily.CONCENTRATED_LIQUIDITY),
]

POOL_IDS = [expected_type.__name__ for _, expected_type, _ in MAINNET_POOLS]


@pytest.fixture(params=MAINNET_POOLS, ids=POOL_IDS)
def pool_case(request):
    """A (constructor, expected_type, pool_family) tuple for a mainnet pool."""
    return request.param


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


class TestConcretePoolConstruction:
    """Every concrete pool type can be constructed I/O-free and is the correct type."""

    def test_returns_correct_concrete_type(self, pool_case):
        constructor, expected_type, _ = pool_case
        pool = constructor()
        assert type(pool) is expected_type

    def test_is_abstract_liquidity_pool(self, pool_case):
        constructor, _, _ = pool_case
        pool = constructor()
        assert isinstance(pool, AbstractLiquidityPool)

    def test_chain_id_is_mainnet(self, pool_case):
        constructor, _, _ = pool_case
        pool = constructor()
        assert pool.chain_id == 1

    def test_tokens_are_set(self, pool_case):
        constructor, _, _ = pool_case
        pool = constructor()
        assert pool.token0 is not None
        assert pool.token1 is not None
        assert pool.token0.address != pool.token1.address

    def test_name_is_set(self, pool_case):
        constructor, _, _ = pool_case
        pool = constructor()
        assert isinstance(pool.name, str)
        assert pool.name


class TestV2PoolInterface:
    """V2-style pools expose the constant-product interface."""

    @pytest.fixture(
        params=[c for c, t, f in MAINNET_POOLS if f == PoolFamily.CONSTANT_PRODUCT],
        ids=[t.__name__ for c, t, f in MAINNET_POOLS if f == PoolFamily.CONSTANT_PRODUCT],
    )
    def v2_pool(self, request):
        return request.param()

    def test_reserves(self, v2_pool):
        assert v2_pool.reserves_token0 > 0
        assert v2_pool.reserves_token1 > 0

    def test_fees(self, v2_pool):
        assert isinstance(v2_pool.fee_token0, Fraction)
        assert isinstance(v2_pool.fee_token1, Fraction)

    def test_factory(self, v2_pool):
        assert v2_pool.factory.startswith("0x")


class TestV3PoolInterface:
    """V3-style pools expose the concentrated-liquidity interface."""

    @pytest.fixture(
        params=[
            c
            for c, t, f in MAINNET_POOLS
            if f == PoolFamily.CONCENTRATED_LIQUIDITY and t is not UniswapV4Pool
        ],
        ids=[
            t.__name__
            for c, t, f in MAINNET_POOLS
            if f == PoolFamily.CONCENTRATED_LIQUIDITY and t is not UniswapV4Pool
        ],
    )
    def v3_pool(self, request):
        return request.param()

    def test_concentrated_state(self, v3_pool):
        assert v3_pool.sqrt_price_x96 > 0
        assert isinstance(v3_pool.tick, int)
        assert v3_pool.liquidity >= 0

    def test_fee_and_tick_spacing(self, v3_pool):
        assert v3_pool.fee > 0
        assert v3_pool.tick_spacing > 0

    def test_factory(self, v3_pool):
        assert v3_pool.factory.startswith("0x")

    def test_sparse_without_tick_data(self, v3_pool):
        assert v3_pool.sparse_liquidity_map is True


class TestV4PoolInterface:
    """UniswapV4Pool exposes V4-specific attributes."""

    def test_pool_id(self):
        pool = _make_uniswap_v4_pool()
        assert pool.pool_id == bytes.fromhex(
            "4f88f7c99022eace4740c6898f59ce6a2e798a1e64ce54589720b7153eb224a7",
        )

    def test_address_is_pool_manager(self):
        pool = _make_uniswap_v4_pool()
        assert pool.address.lower() == V4_POOL_MANAGER.lower()

    def test_protocol_fee(self):
        pool = _make_uniswap_v4_pool()
        assert pool.protocol_fee.zero_for_one == 0
        assert pool.protocol_fee.one_for_zero == 0

    def test_lp_fee(self):
        pool = _make_uniswap_v4_pool()
        assert pool.lp_fee == 500000

    def test_sparse_without_tick_data(self):
        pool = _make_uniswap_v4_pool()
        assert pool.sparse_liquidity_map is True


class TestInheritance:
    """Variant pools inherit from the correct base class."""

    def test_sushiswap_v2_inherits_uniswap_v2(self):
        assert issubclass(SushiswapV2Pool, UniswapV2Pool)

    def test_pancakeswap_v2_inherits_uniswap_v2(self):
        assert issubclass(PancakeswapV2Pool, UniswapV2Pool)

    def test_sushiswap_v3_inherits_uniswap_v3(self):
        assert issubclass(SushiswapV3Pool, UniswapV3Pool)

    def test_pancakeswap_v3_inherits_uniswap_v3(self):
        assert issubclass(PancakeswapV3Pool, UniswapV3Pool)

    def test_all_inherit_abstract_liquidity_pool(self):
        for cls in [
            UniswapV2Pool,
            SushiswapV2Pool,
            PancakeswapV2Pool,
            UniswapV3Pool,
            SushiswapV3Pool,
            PancakeswapV3Pool,
            UniswapV4Pool,
        ]:
            assert issubclass(cls, AbstractLiquidityPool)
