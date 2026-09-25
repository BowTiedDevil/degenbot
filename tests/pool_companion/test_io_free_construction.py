"""I/O-free companion construction - collapsed, registry-driven.

Holds the two invariant families plus the construction/build coverage that
lived in the per-class ``*_io_free*.py`` files:

* ``test_construction_touches_no_provider`` - invariant #2, parametrized over
  the companion registry: constructing any companion with injected data never
  touches the central provider (probed with a single recording ``MagicMock``
  provider handed to the high-level ``Bot``; no per-call patching).
* the per-kind I/O-free constructor / ``Bot.build_*`` coverage (V2/V3/V4,
  token, ether placeholder, Curve data-provider example).

``Bot.build_*`` choreography is alloy-only, so the build paths are driven from
one-block ``OfflineProvider`` casettes (see ``_helpers``). Per-kind injected
builders (``_v2_injected`` et al.) keep the setup in one place; each test keeps
its distinct id.
"""

from __future__ import annotations

from dataclasses import dataclass
from fractions import Fraction
from typing import TYPE_CHECKING, Any
from unittest.mock import MagicMock

import pytest

from degenbot._ffi import Bot as _Engine
from degenbot.abi import encode as abi_encode
from degenbot.bot import Bot
from degenbot.builders.request import BuildManagedPoolRequest
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.db import db_create_new_database
from degenbot.erc20.erc20 import Erc20Token
from degenbot.provider import OfflineProvider
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import UniswapV3PoolExternalUpdate
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool
from degenbot.uniswap.v4_types import UniswapV4PoolExternalUpdate, UniswapV4PoolKey
from degenbot.utils.bytes import to_bytes
from tests.fakes.curve_data_provider import FakeCurveDataProvider
from tests.helpers.balancer_pool_factory import make_balancer_stable_pool
from tests.helpers.curve_pool_factory import make_curve_pool
from tests.helpers.erc20_factory import make_erc20, make_ether_placeholder
from tests.helpers.v2_pool_factory import make_v2_pool
from tests.helpers.v3_pool_factory import make_v3_pool
from tests.helpers.v4_pool_factory import make_v4_pool
from tests.pool_companion._helpers import (
    PY_BOT,
    UNISWAP_V2_FACTORY,
    UNISWAP_V3_FACTORY,
    USDC_ADDR,
    USDC_WETH_V3_POOL,
    V3_FEE,
    V3_TICK_SPACING,
    V4_FEE,
    V4_HOOKS,
    V4_POOL_ID,
    V4_POOL_MANAGER,
    V4_STATE_VIEW,
    V4_TICK_SPACING,
    WETH_ADDR,
    WETH_USDC_V2_POOL,
    make_native_eth,
    make_test_config,
    make_usdc,
    make_weth,
    v2_offline_provider,
    v3_offline_provider,
    v4_offline_provider,
)

if TYPE_CHECKING:
    import pathlib
    from collections.abc import Callable

    from degenbot.erc20 import EtherPlaceholder

# ---------------------------------------------------------------------------
# Per-kind injected-data builders (I/O-free pool construction)
# ---------------------------------------------------------------------------


def _v2_injected(**overrides: Any) -> UniswapV2Pool:
    """I/O-free V2 pool with the reference reserves/fees; ``overrides`` win."""
    kwargs: dict[str, Any] = {
        "address": WETH_USDC_V2_POOL,
        "chain_id": 1,
        "token0": make_weth(),
        "token1": make_usdc(),
        "factory": UNISWAP_V2_FACTORY,
        "fee_token0": Fraction(3, 1000),
        "fee_token1": Fraction(3, 1000),
        "reserves_token0": 1000 * 10**18,
        "reserves_token1": 2_000_000 * 10**6,
        "state_block": 18_000_000,
    }
    kwargs.update(overrides)
    return make_v2_pool(**kwargs)


def _v3_injected(**overrides: Any) -> UniswapV3Pool:
    """I/O-free V3 pool with the reference scalars; ``overrides`` win."""
    kwargs: dict[str, Any] = {
        "address": USDC_WETH_V3_POOL,
        "token0": make_weth(),
        "token1": make_usdc(),
        "factory": UNISWAP_V3_FACTORY,
        "fee": V3_FEE,
        "tick_spacing": V3_TICK_SPACING,
        "sqrt_price_x96": 2198666895605149686863,
        "tick": -76020,
        "liquidity": 1234567890,
        "state_block": 18_000_000,
    }
    kwargs.update(overrides)
    return make_v3_pool(**kwargs)


def _v4_injected(**overrides: Any) -> UniswapV4Pool:
    """I/O-free V4 pool with the reference scalars; ``overrides`` win."""
    kwargs: dict[str, Any] = {
        "pool_id": V4_POOL_ID,
        "pool_manager_address": V4_POOL_MANAGER,
        "token0": make_native_eth(),
        "token1": make_usdc(),
        "fee": V4_FEE,
        "tick_spacing": V4_TICK_SPACING,
        "hook_address": V4_HOOKS,
        "sqrt_price_x96": 2198666895605149686863,
        "tick": -76020,
        "liquidity": 1234567890,
        "protocol_fee_zero_for_one": 0,
        "protocol_fee_one_for_zero": 0,
        "lp_fee": 500000,
        "state_block": 18_000_000,
    }
    kwargs.update(overrides)
    return make_v4_pool(**kwargs)


# ---------------------------------------------------------------------------
# Invariant #2: companion construction performs no I/O
# ---------------------------------------------------------------------------


def _build_erc20(bot: Bot) -> Erc20Token:
    return make_erc20(
        bot._py_bot,
        "0x" + "ab" * 20,
        chain_id=1,
        name="NoIO",
        symbol="NIO",
        decimals=18,
    )


def _build_ether_placeholder(bot: Bot) -> EtherPlaceholder:
    return make_ether_placeholder(bot._py_bot, ZERO_ADDRESS, chain_id=1)


def _build_v2(bot: Bot) -> UniswapV2Pool:
    weth = make_erc20(
        bot._py_bot, WETH_ADDR, chain_id=1, name="Wrapped Ether", symbol="WETH", decimals=18
    )
    usdc = make_erc20(
        bot._py_bot, USDC_ADDR, chain_id=1, name="USD Coin", symbol="USDC", decimals=6
    )
    return make_v2_pool(
        WETH_USDC_V2_POOL,
        chain_id=1,
        token0=weth,
        token1=usdc,
        factory=UNISWAP_V2_FACTORY,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=1000 * 10**18,
        reserves_token1=2_000_000 * 10**6,
        state_block=18_000_000,
        py_bot=bot._py_bot,
    )


def _build_v3(bot: Bot) -> UniswapV3Pool:
    weth = make_erc20(
        bot._py_bot, WETH_ADDR, chain_id=1, name="Wrapped Ether", symbol="WETH", decimals=18
    )
    usdc = make_erc20(
        bot._py_bot, USDC_ADDR, chain_id=1, name="USD Coin", symbol="USDC", decimals=6
    )
    return make_v3_pool(
        address=USDC_WETH_V3_POOL,
        token0=weth,
        token1=usdc,
        factory=UNISWAP_V3_FACTORY,
        fee=V3_FEE,
        tick_spacing=V3_TICK_SPACING,
        sqrt_price_x96=2198666895605149686863,
        tick=-76020,
        liquidity=1234567890,
        state_block=18_000_000,
        py_bot=bot._py_bot,
    )


def _build_v4(bot: Bot) -> UniswapV4Pool:
    native_eth = make_erc20(
        bot._py_bot, ZERO_ADDRESS, chain_id=1, name="Ether", symbol="ETH", decimals=18
    )
    usdc = make_erc20(
        bot._py_bot, USDC_ADDR, chain_id=1, name="USD Coin", symbol="USDC", decimals=6
    )
    return make_v4_pool(
        pool_id=V4_POOL_ID,
        pool_manager_address=V4_POOL_MANAGER,
        token0=native_eth,
        token1=usdc,
        fee=V4_FEE,
        tick_spacing=V4_TICK_SPACING,
        hook_address=V4_HOOKS,
        sqrt_price_x96=2198666895605149686863,
        tick=-76020,
        liquidity=1234567890,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=500000,
        state_block=18_000_000,
        py_bot=bot._py_bot,
    )


def _build_curve(bot: Bot) -> Any:
    dai = make_erc20(
        bot._py_bot,
        "0x6B175474E89094C44Da98b954EedeAC495271d0F",
        chain_id=1,
        name="DAI",
        symbol="DAI",
        decimals=18,
    )
    usdc = make_erc20(
        bot._py_bot, USDC_ADDR, chain_id=1, name="USD Coin", symbol="USDC", decimals=6
    )
    return make_curve_pool(
        "0x" + "ac" * 20,
        tokens=[dai, usdc],
        a_coefficient=2000,
        fee=4_000_000,
        admin_fee=5_000_000_000,
        balances=[10**18, 2 * 10**18],
        state_block=18_000_000,
        py_bot=bot._py_bot,
    )


def _build_balancer_stable(bot: Bot) -> Any:
    t0 = make_erc20(bot._py_bot, "0x" + "d4" * 20, chain_id=1, name="S0", symbol="S0", decimals=18)
    t1 = make_erc20(bot._py_bot, "0x" + "e5" * 20, chain_id=1, name="S1", symbol="S1", decimals=18)
    return make_balancer_stable_pool(
        address="0x" + "07" * 20,
        pool_id=bytes.fromhex("07" * 20 + "0002" + "0" * 20),
        vault="0x" + "ba" * 20,
        tokens=[t0, t1],
        balances=[10**18, 2 * 10**18],
        fee=Fraction(3, 1000),
        amp=100_000,
        scaling_factors=[10**18, 10**18],
        py_bot=bot._py_bot,
    )


@dataclass(frozen=True)
class NoIOCase:
    """A companion builder keyed by pool kind, for the no-I/O invariant."""

    id: str
    pool_kind: str
    build: Callable[[Bot], Any]


NO_IO_REGISTRY: tuple[NoIOCase, ...] = (
    NoIOCase(id="erc20", pool_kind="token", build=_build_erc20),
    NoIOCase(id="ether-placeholder", pool_kind="token", build=_build_ether_placeholder),
    NoIOCase(id="v2", pool_kind="v2", build=_build_v2),
    NoIOCase(id="v3", pool_kind="v3", build=_build_v3),
    NoIOCase(id="v4", pool_kind="v4", build=_build_v4),
    NoIOCase(id="curve", pool_kind="curve", build=_build_curve),
    NoIOCase(id="balancer-stable", pool_kind="balancer-stable", build=_build_balancer_stable),
)


@pytest.mark.parametrize("case", NO_IO_REGISTRY, ids=[c.id for c in NO_IO_REGISTRY])
def test_construction_touches_no_provider(case: NoIOCase, tmp_path: pathlib.Path) -> None:
    """Constructing a companion never touches the Bot's central provider/DB.

    The provider is a single recording ``MagicMock`` handed to the high-level
    ``Bot`` (the central I/O boundary, whose ``ConstructionIo`` is attached to
    the engine). Injected-data construction must not issue any RPC.
    """
    provider = MagicMock()
    provider.chain_id = 1
    provider.is_connected.return_value = True
    bot = Bot(make_test_config(tmp_path), provider=provider)

    companion = case.build(bot)

    assert companion is not None
    provider.call.assert_not_called()
    provider.get_block_number.assert_not_called()


# ---------------------------------------------------------------------------
# Per-kind I/O-free constructors
# ---------------------------------------------------------------------------


class TestV2PoolIOFreeConstructor:
    """UniswapV2Pool can be constructed with pre-fetched data only."""

    def test_io_free_constructor_basic(self) -> None:
        pool = _v2_injected()

        assert pool.address == WETH_USDC_V2_POOL
        assert pool.token0.address == make_weth().address
        assert pool.token1.address == make_usdc().address
        assert pool.factory == get_checksum_address(UNISWAP_V2_FACTORY)
        assert pool.fee_token0 == Fraction(3, 1000)
        assert pool.fee_token1 == Fraction(3, 1000)
        assert pool.reserves_token0 == 1000 * 10**18
        assert pool.reserves_token1 == 2_000_000 * 10**6
        assert pool.update_block == 18_000_000

    def test_io_free_pool_computation_works(self) -> None:
        pool = _v2_injected()

        pool.external_update(
            UniswapV2PoolExternalUpdate(
                block_number=18_000_001,
                reserves_token0=1100 * 10**18,
                reserves_token1=1_800_000 * 10**6,
            )
        )
        assert pool.reserves_token0 == 1100 * 10**18

    def test_io_free_constructor_with_split_fees(self) -> None:
        pool = _v2_injected(fee_token1=Fraction(2, 1000))

        assert pool.fee_token0 == Fraction(3, 1000)
        assert pool.fee_token1 == Fraction(2, 1000)


class TestBotBuildV2Pool:
    """Bot.build_pool() constructs I/O-free pools from on-chain data."""

    def test_build_pool_with_mock_provider(self, tmp_path: pathlib.Path) -> None:
        weth_addr = WETH_ADDR
        usdc_addr = USDC_ADDR
        factory_addr = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"

        config = make_test_config(tmp_path)
        bot = Bot(
            config,
            provider=v2_offline_provider(
                weth_addr=weth_addr,
                usdc_addr=usdc_addr,
                factory_addr=factory_addr,
                pool_addr=WETH_USDC_V2_POOL,
            ),
        )

        weth = make_erc20(
            bot._py_bot, weth_addr, chain_id=1, name="Wrapped Ether", symbol="WETH", decimals=18
        )
        usdc = make_erc20(
            bot._py_bot, usdc_addr, chain_id=1, name="USD Coin", symbol="USDC", decimals=6
        )
        bot.tokens.add(token_address=weth_addr, chain_id=1, token=weth)
        bot.tokens.add(token_address=usdc_addr, chain_id=1, token=usdc)

        pool = bot.build_pool(WETH_USDC_V2_POOL)

        assert isinstance(pool, UniswapV2Pool)
        assert pool.address == get_checksum_address(WETH_USDC_V2_POOL)
        assert pool.token0.address == get_checksum_address(weth_addr)
        assert pool.token1.address == get_checksum_address(usdc_addr)
        assert pool.factory == get_checksum_address(factory_addr)
        assert pool.reserves_token0 == 1000 * 10**18
        assert pool.reserves_token1 == 2_000_000 * 10**6
        assert bot.pools.get(pool_address=pool.address, chain_id=1) is pool


class TestV3PoolIOFreeConstructor:
    """UniswapV3Pool can be constructed with pre-fetched data only."""

    def test_io_free_constructor_basic(self) -> None:
        pool = _v3_injected()

        assert pool.address == get_checksum_address(USDC_WETH_V3_POOL)
        assert pool.token0.address == make_weth().address
        assert pool.token1.address == make_usdc().address
        assert pool.factory == get_checksum_address(UNISWAP_V3_FACTORY)
        assert pool.fee == V3_FEE
        assert pool.tick_spacing == V3_TICK_SPACING
        assert pool.sqrt_price_x96 == 2198666895605149686863
        assert pool.tick == -76020
        assert pool.liquidity == 1234567890
        assert pool.update_block == 18_000_000

    def test_io_free_constructor_with_tick_data(self) -> None:
        tick_data = {
            -76080: LiquidityAtTick(liquidity_gross=1000, liquidity_net=-500, block=18_000_000),
            -75960: LiquidityAtTick(liquidity_gross=2000, liquidity_net=300, block=18_000_000),
        }
        tick_bitmap = {-297: BitmapAtWord(bitmap=3, block=18_000_000)}

        pool = _v3_injected(tick_bitmap=tick_bitmap, tick_data=tick_data)

        assert pool.sparse_liquidity_map is False
        assert -76080 in pool.tick_data
        assert -75960 in pool.tick_data

    def test_io_free_pool_external_update(self) -> None:
        pool = _v3_injected()

        updated = pool.external_update(
            UniswapV3PoolExternalUpdate(
                block_number=18_000_001,
                sqrt_price_x96=2200000000000000000000,
                tick=-75900,
                liquidity=9999999999,
            )
        )
        assert updated is True
        assert pool.tick == -75900
        assert pool.liquidity == 9999999999


class TestBotBuildV3Pool:
    """Bot.build_pool() constructs I/O-free V3 pools from on-chain data."""

    def test_build_pool_with_mock_provider(self, tmp_path: pathlib.Path) -> None:
        weth_addr = WETH_ADDR
        usdc_addr = USDC_ADDR
        factory_addr = UNISWAP_V3_FACTORY

        sqrt_price = 2198666895605149686863
        tick = -76020
        liquidity = 1234567890

        config = make_test_config(tmp_path)
        bot = Bot(
            config,
            provider=v3_offline_provider(
                weth_addr=weth_addr,
                usdc_addr=usdc_addr,
                factory_addr=factory_addr,
                pool_addr=USDC_WETH_V3_POOL,
                sqrt_price=sqrt_price,
                tick=tick,
                liquidity=liquidity,
            ),
        )

        weth = make_weth()
        usdc = make_usdc()
        for tok in (weth, usdc):
            if bot._py_bot.get_token(tok.address) is None:
                bot._py_bot.register_token(tok.address, tok.name, tok.symbol, tok.decimals, 1)
        bot.tokens.add(token_address=weth_addr, chain_id=1, token=weth)
        bot.tokens.add(token_address=usdc_addr, chain_id=1, token=usdc)

        pool = bot.build_pool(USDC_WETH_V3_POOL)

        assert isinstance(pool, UniswapV3Pool)
        assert pool.address == get_checksum_address(USDC_WETH_V3_POOL)
        assert pool.token0.address == get_checksum_address(weth_addr)
        assert pool.token1.address == get_checksum_address(usdc_addr)
        assert pool.factory == get_checksum_address(factory_addr)
        assert pool.fee == V3_FEE
        assert pool.tick_spacing == V3_TICK_SPACING
        assert pool.sqrt_price_x96 == sqrt_price
        assert pool.tick == tick
        assert pool.liquidity == liquidity
        assert bot.pools.get(pool_address=pool.address, chain_id=1) is pool


class TestV4PoolIOFreeConstructor:
    """UniswapV4Pool can be constructed with pre-fetched data only."""

    def test_io_free_constructor_basic(self) -> None:
        sqrt_price = 2198666895605149686863
        tick = -76020
        liquidity = 1234567890

        pool = _v4_injected(
            protocol_fee_zero_for_one=100,
            protocol_fee_one_for_zero=200,
        )

        assert pool.address == get_checksum_address(V4_POOL_MANAGER)
        assert pool.pool_id == to_bytes(V4_POOL_ID)
        assert pool.token0 == make_native_eth()
        assert pool.token1 == make_usdc()
        assert pool.fee == V4_FEE
        assert pool.tick_spacing == V4_TICK_SPACING
        assert pool.sqrt_price_x96 == sqrt_price
        assert pool.tick == tick
        assert pool.liquidity == liquidity
        assert pool.protocol_fee.zero_for_one == 100
        assert pool.protocol_fee.one_for_zero == 200
        assert pool.lp_fee == 500000

    def test_io_free_with_tick_data(self) -> None:
        tick_data = {
            120: LiquidityAtTick(liquidity_net=100, liquidity_gross=200, block=18_000_000),
            -10: LiquidityAtTick(liquidity_net=100, liquidity_gross=200, block=18_000_000),
        }

        pool = _v4_injected(tick_data=tick_data)

        assert pool.sparse_liquidity_map is False
        assert pool.tick_bitmap[0].bitmap == (1 << 12)
        assert pool.tick_bitmap[-1].bitmap == (1 << 255)
        assert pool.tick_data[120].liquidity_net == 100
        assert pool.tick_data[-10].liquidity_net == 100

    def test_io_free_external_update(self) -> None:
        pool = _v4_injected()

        update = UniswapV4PoolExternalUpdate(
            block_number=18_000_001,
            sqrt_price_x96=2198666895605150000000,
            tick=-76010,
            liquidity=1234567999,
        )
        assert pool.external_update(update) is True
        assert pool.tick == -76010
        assert pool.liquidity == 1234567999

    def test_io_free_pool_key_and_hooks(self) -> None:
        pool = _v4_injected()

        assert pool.pool_key == UniswapV4PoolKey(
            currency0=make_native_eth().address,
            currency1=make_usdc().address,
            fee=V4_FEE,
            tick_spacing=V4_TICK_SPACING,
            hooks=V4_HOOKS,
        )
        assert pool.hook_address == ZERO_ADDRESS
        assert pool.active_hooks == frozenset()

    def test_io_free_state_view_address(self) -> None:
        state_view = get_checksum_address(V4_STATE_VIEW)

        pool = _v4_injected(state_view_address=state_view)

        assert pool._state_view_address == state_view


class TestBotBuildV4Pool:
    """Bot.build_pool() constructs I/O-free V4 pools from on-chain data."""

    def test_build_pool_with_mock_provider(self, tmp_path: pathlib.Path) -> None:
        sqrt_price = 2198666895605149686863
        tick = -76020
        protocol_fee = 0
        lp_fee = 500000
        liquidity = 1234567890

        config = make_test_config(tmp_path)
        bot = Bot(
            config,
            provider=v4_offline_provider(
                pool_manager=V4_POOL_MANAGER,
                state_view=V4_STATE_VIEW,
                pool_id=V4_POOL_ID,
                sqrt_price=sqrt_price,
                tick=tick,
                protocol_fee=protocol_fee,
                lp_fee=lp_fee,
                liquidity=liquidity,
            ),
        )

        native_eth = make_native_eth()
        usdc = make_usdc()
        for tok in (native_eth, usdc):
            if bot._py_bot.get_token(tok.address) is None:
                bot._py_bot.register_token(tok.address, tok.name, tok.symbol, tok.decimals, 1)
        bot.tokens.add(token_address=ZERO_ADDRESS, chain_id=1, token=native_eth)
        bot.tokens.add(token_address=get_checksum_address(USDC_ADDR), chain_id=1, token=usdc)

        pool = bot.build_managed_pool(
            V4_POOL_MANAGER,
            BuildManagedPoolRequest(
                pool_id=V4_POOL_ID,
                state_view_address=V4_STATE_VIEW,
                fee=V4_FEE,
                tick_spacing=V4_TICK_SPACING,
                hook_address=V4_HOOKS,
                tokens=[ZERO_ADDRESS, USDC_ADDR],
            ),
        )

        assert isinstance(pool, UniswapV4Pool)
        assert pool.pool_id == to_bytes(V4_POOL_ID)
        assert pool.address == get_checksum_address(V4_POOL_MANAGER)
        assert pool.fee == V4_FEE
        assert pool.tick_spacing == V4_TICK_SPACING
        assert pool.sqrt_price_x96 == sqrt_price
        assert pool.tick == tick
        assert pool.liquidity == liquidity
        assert pool.lp_fee == lp_fee

        found = bot.managed_pools.get(
            chain_id=1,
            pool_manager_address=V4_POOL_MANAGER,
            pool_id=to_bytes(V4_POOL_ID),
        )
        assert found is pool


# ---------------------------------------------------------------------------
# Token / ether-placeholder data-only construction
# ---------------------------------------------------------------------------


class TestErc20TokenDataOnlyConstructor:
    """Erc20Token companion reads metadata through the Erc20Token handle."""

    def test_constructor_with_data(self) -> None:
        token = make_erc20(
            PY_BOT, WETH_ADDR, chain_id=1, name="Wrapped Ether", symbol="WETH", decimals=18
        )
        assert token.name == "Wrapped Ether"
        assert token.symbol == "WETH"
        assert token.decimals == 18
        assert token.chain_id == 1
        assert token.address == WETH_ADDR

    def test_constructor_normalizes_address(self) -> None:
        token = make_erc20(
            PY_BOT,
            "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
            chain_id=1,
            name="Wrapped Ether",
            symbol="WETH",
            decimals=18,
        )
        assert token.address == WETH_ADDR

    def test_constructor_defaults_chain_id(self) -> None:
        token = make_erc20(
            PY_BOT,
            "0x6810e776880C02933D47DB1b9fc05908e5386b96",
            name="Gnosis",
            symbol="GNO",
            decimals=18,
        )
        assert token.chain_id == 1

    def test_cache_accessors_balance(self) -> None:
        token = make_erc20(
            PY_BOT,
            "0x6B175474E89094C44Da98b954EedeAC495271d0F",
            chain_id=1,
            name="DAI",
            symbol="DAI",
            decimals=18,
        )
        assert token.get_cached_balance("0x" + "11" * 20, block_number=100) is None
        token.set_cached_balance("0x" + "11" * 20, block_number=100, balance=10**18)
        assert token.get_cached_balance("0x" + "11" * 20, block_number=100) == 10**18

    def test_cache_accessors_approval(self) -> None:
        token = make_erc20(
            PY_BOT, USDC_ADDR, chain_id=1, name="USD Coin", symbol="USDC", decimals=6
        )
        owner = "0x" + "11" * 20
        spender = "0x" + "22" * 20

        assert token.get_cached_approval(block_number=100, owner=owner, spender=spender) is None
        token.set_cached_approval(block_number=100, owner=owner, spender=spender, amount=500)
        assert token.get_cached_approval(block_number=100, owner=owner, spender=spender) == 500

    def test_cache_accessors_total_supply(self) -> None:
        token = make_erc20(
            PY_BOT,
            "0xdAC17F958D2ee523a2206206994597C13D831ec7",
            chain_id=1,
            name="Tether USD",
            symbol="USDT",
            decimals=6,
        )
        assert token.get_cached_total_supply(block_number=100) is None
        token.set_cached_total_supply(block_number=100, total_supply=10**27)
        assert token.get_cached_total_supply(block_number=100) == 10**27


class TestBotBuildErc20Token:
    """Bot.build_erc20token() fetches metadata and constructs the companion."""

    def test_build_token_from_chain(self, tmp_path: pathlib.Path) -> None:
        config = make_test_config(tmp_path)
        db_create_new_database(str(config.database.path))
        token_address = WETH_ADDR
        offline = OfflineProvider(
            chain_id=1,
            blocks={
                "100": {
                    "timestamp": 1700000000,
                    "calls": {
                        f"{token_address.lower()}:0x06fdde03": (
                            abi_encode(["string"], ["Wrapped Ether"]).hex()
                        ),
                        f"{token_address.lower()}:0x95d89b41": (
                            abi_encode(["string"], ["WETH"]).hex()
                        ),
                        f"{token_address.lower()}:0x313ce567": (
                            abi_encode(["uint256"], [18]).hex()
                        ),
                    },
                    "code": {token_address.lower(): "01"},
                }
            },
        )
        bot = Bot(config, provider=offline)

        token = bot.build_erc20token(token_address)
        assert isinstance(token, Erc20Token)
        assert token.chain_id == 1
        assert bot.tokens.get(token_address=token.address, chain_id=1) is token


class TestEtherPlaceholderDataOnly:
    """EtherPlaceholder delegates metadata through the Erc20Token handle."""

    def test_constructor_with_data(self) -> None:
        placeholder = make_ether_placeholder(
            PY_BOT,
            "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE",
            chain_id=1,
        )
        assert placeholder.chain_id == 1
        assert placeholder.symbol == "ETH"
        assert placeholder.name == "Ether Placeholder"
        assert placeholder.decimals == 18


# ---------------------------------------------------------------------------
# Curve I/O-free data-provider example (one body, four pool-kind cases)
# ---------------------------------------------------------------------------

_CURVE_BOT = _Engine()


@dataclass(frozen=True)
class CurveIoFreeCase:
    """A Curve pool kind built entirely from an injected data provider."""

    id: str
    address: str
    tokens: tuple[tuple[str, str, str, int], ...]  # (address, name, symbol, decimals)
    provider_kwargs: dict[str, Any]
    pool_kwargs: dict[str, Any]
    amount_in: int
    approx: tuple[int, float] | None = None


_CURVE_IO_FREE: tuple[CurveIoFreeCase, ...] = (
    CurveIoFreeCase(
        id="plain",
        address="0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7",
        tokens=(
            ("0x6B175474E89094C44Da98b954EedeAC495271d0F", "DAI", "DAI", 18),
            (USDC_ADDR, "USD Coin", "USDC", 6),
        ),
        provider_kwargs={},
        pool_kwargs={
            "a_coefficient": 2000,
            "fee": 4000000,
            "admin_fee": 5000000000,
            "balances": (10_000_000 * 10**18, 10_000_000 * 10**6),
        },
        amount_in=1000 * 10**18,
        approx=(999 * 10**6, 0.01),
    ),
    CurveIoFreeCase(
        id="lending",
        address="0x0000000000000000000000000000000000000001",
        tokens=(
            ("0x5d3a536E4D6DbD6114cc1Ead35777bAB948E3643", "Compound DAI", "cDAI", 8),
            ("0x39AA39c021dfbaE8faC545936693aC917d5E7563", "Compound USDC", "cUSDC", 8),
        ),
        provider_kwargs={"lending_rates": (10**18 * 102 // 100, 10**18 * 105 // 100)},
        pool_kwargs={
            "a_coefficient": 1000,
            "fee": 4000000,
            "admin_fee": 5000000000,
            "balances": (1_000_000 * 10**8, 1_000_000 * 10**8),
            "use_lending": (True, True),
        },
        amount_in=100 * 10**8,
    ),
    CurveIoFreeCase(
        id="metapool",
        address="0x618788357D0EBd8A37e763ADab3bc575D54c2C7d",
        tokens=(
            ("0x81ab848898b15A779B7cd0cB2cDd406c64EFc12c", "Rai Reflex Index", "RAI", 18),
            ("0x6c3F90f043a72FA612CbAC8115EEe7f52CdE6E49", "Curve 3Pool Token", "3Crv", 18),
        ),
        provider_kwargs={"base_virtual_price": 10**18 * 102 // 100},
        pool_kwargs={
            "a_coefficient": 400,
            "fee": 4000000,
            "admin_fee": 5000000000,
            "balances": (5_000_000 * 10**18, 10_000_000 * 10**18),
        },
        amount_in=1000 * 10**18,
    ),
    CurveIoFreeCase(
        id="crypto",
        address="0x0000000000000000000000000000000000000002",
        tokens=(
            ("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599", "Wrapped BTC", "WBTC", 8),
            (WETH_ADDR, "Wrapped Ether", "WETH", 18),
        ),
        provider_kwargs={"d": 10**20, "gamma": 10**16, "price_scale": (10**18,)},
        pool_kwargs={
            "a_coefficient": 400,
            "fee": 10000000,
            "admin_fee": 5000000000,
            "balances": (100 * 10**8, 2000 * 10**18),
            "fee_gamma": 500000000000000,
            "mid_fee": 3000000,
            "out_fee": 30000000,
            "gamma": 10**16,
        },
        amount_in=10**8,
    ),
)


@pytest.mark.parametrize("case", _CURVE_IO_FREE, ids=[c.id for c in _CURVE_IO_FREE])
def test_curve_pool_with_data_provider(case: CurveIoFreeCase) -> None:
    """A Curve pool kind computes ``get_dy`` from an injected data provider."""
    tokens = [
        make_erc20(_CURVE_BOT, addr, name=name, symbol=symbol, decimals=decimals)
        for addr, name, symbol, decimals in case.tokens
    ]
    provider = FakeCurveDataProvider(block_timestamp=1_700_000_000, **case.provider_kwargs)

    pool = make_curve_pool(
        address=case.address,
        tokens=tokens,
        state_block=18_000_000,
        data_provider=provider,
        **case.pool_kwargs,
    )

    result = pool.get_dy(0, 1, case.amount_in, block_identifier=18_000_000)

    assert result > 0
    if case.approx is not None:
        expected, tolerance = case.approx
        assert result > expected * (1 - tolerance)
        assert result < expected * (1 + tolerance)
