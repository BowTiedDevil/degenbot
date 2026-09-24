"""I/O-free companion behavior - collapsed from the per-class io_free files.

The remaining distinct (non-invariant) coverage from the per-class
``*_io_free*.py`` / ``*_construction_guard.py`` files: V2 calc delegation,
tracker wiring, token cache/RPC paths, Balancer rate-provider storage, and the
Curve single-arg seam details. Bodies are preserved verbatim (class + method
names unchanged) so failure attribution stays sharp; only the module location
and shared helpers changed.
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING
from unittest.mock import MagicMock

import pytest

from degenbot._ffi import Bot as _Engine
from degenbot._ffi.v2_math import calc_exact_in_v2, calc_exact_out_v2
from degenbot.abi import encode as abi_encode
from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.types import BasePoolPort, DyCalculationInputs
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import InvalidSwapInputAmount, LiquidityPoolError
from degenbot.provider import OfflineProvider
from degenbot.uniswap.trackers import UniswapV2PoolTracker, UniswapV3PoolTracker
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolState
from tests.helpers.balancer_pool_factory import make_balancer_stable_pool
from tests.helpers.curve_pool_factory import make_curve_pool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool
from tests.helpers.v3_pool_factory import make_v3_pool
from tests.pool_companion._helpers import (
    PY_BOT,
    UNISWAP_V2_FACTORY,
    UNISWAP_V3_FACTORY,
    USDC_WETH_V3_POOL,
    V3_FEE,
    V3_TICK_SPACING,
    WETH_USDC_V2_POOL,
    make_test_config,
    make_usdc,
    make_weth,
    v2_offline_provider,
)

if TYPE_CHECKING:
    import pathlib

    from degenbot.erc20.erc20 import Erc20Token


def _make_weth() -> Erc20Token:
    return make_weth()


def _make_usdc() -> Erc20Token:
    return make_usdc()


class _DelegateSpy:
    """Wraps a ``Pool`` to record ``calculate_tokens_out/in`` calls.

    ADR-005 slice 5 delegation-test for the V2 constant-product calc path:
    ``UniswapV2Pool.calculate_tokens_out_from_tokens_in`` (no override) routes
    through ``Pool.calculate_tokens_out``. Pass-through for all other
    handle methods via ``__getattr__``.
    """

    def __init__(self, real_pool):
        self._real = real_pool
        self.calc_out_calls: list[tuple[bool, int]] = []
        self.calc_in_calls: list[tuple[bool, int]] = []

    def calculate_tokens_out(self, *, zero_for_one, amount_in):
        self.calc_out_calls.append((zero_for_one, int(amount_in)))
        return self._real.calculate_tokens_out(zero_for_one=zero_for_one, amount_in=amount_in)

    def calculate_tokens_in(self, *, zero_for_one, amount_out):
        self.calc_in_calls.append((zero_for_one, int(amount_out)))
        return self._real.calculate_tokens_in(zero_for_one=zero_for_one, amount_out=amount_out)

    def __getattr__(self, name):
        return getattr(self._real, name)


class TestV2CalcDelegation:
    """ADR-005 slice 5 - V2 calc delegation to ``Pool``.

    The constant-product calc math delegates to Rust's
    ``Pool.calculate_tokens_out/in`` when no ``override_state`` is
    given (single read guard - no separate Python state read before the calc,
    so no pump-interleave risk). The override path calls the
    ``calc_exact_in/out_v2`` FFI seam against the override reserves so
    ``simulate_*`` can hold one snapshot Python-side for delta + final_state
    consistency (slice 4's fix).
    """

    @staticmethod
    def _make_pool() -> UniswapV2Pool:
        weth = make_weth()
        usdc = make_usdc()
        return make_v2_pool(
            address=WETH_USDC_V2_POOL,
            chain_id=1,
            token0=weth,
            token1=usdc,
            factory=UNISWAP_V2_FACTORY,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000 * 10**18,
            reserves_token1=2_000_000 * 10**6,
            state_block=18_000_000,
        )

    def test_calculate_tokens_out_delegates_to_rust_no_override(self) -> None:
        """No override_state: calc delegates to Pool.calculate_tokens_out."""
        pool = self._make_pool()
        weth = pool.token0
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        result = pool.calculate_tokens_out_from_tokens_in(token_in=weth, token_in_quantity=10**18)

        assert spy.calc_out_calls == [(True, 10**18)]
        assert spy.calc_in_calls == []
        assert result > 0

    def test_calculate_tokens_out_reverse_delegates_to_rust(self) -> None:
        """token1 in -> zero_for_one=False, still delegates to Rust."""
        pool = self._make_pool()
        usdc = pool.token1
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        result = pool.calculate_tokens_out_from_tokens_in(token_in=usdc, token_in_quantity=10**6)

        assert spy.calc_out_calls == [(False, 10**6)]
        assert result > 0

    def test_calculate_tokens_out_uses_python_path_with_override(self) -> None:
        """override_state: calc uses the FFI seam (calc_exact_in_v2)."""
        pool = self._make_pool()
        weth = pool.token0
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        override = UniswapV2PoolState(
            address=pool.address,
            block=None,
            reserves_token0=2000 * 10**18,
            reserves_token1=4_000_000 * 10**6,
        )

        result = pool.calculate_tokens_out_from_tokens_in(
            token_in=weth, token_in_quantity=10**18, override_state=override
        )

        assert spy.calc_out_calls == []
        assert spy.calc_in_calls == []
        assert result == calc_exact_in_v2(
            override.reserves_token0,
            override.reserves_token1,
            10**18,
            3,
            1000,
        )

    def test_calculate_tokens_in_delegates_to_rust_no_override(self) -> None:
        """No override_state: calc delegates to Pool.calculate_tokens_in."""
        pool = self._make_pool()
        usdc = pool.token1
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        result = pool.calculate_tokens_in_from_tokens_out(token_out_quantity=10**6, token_out=usdc)

        assert spy.calc_in_calls == [(True, 10**6)]
        assert spy.calc_out_calls == []
        assert result > 0

    def test_calculate_tokens_in_reverse_delegates_to_rust(self) -> None:
        """token0 out -> zero_for_one=False, still delegates to Rust."""
        pool = self._make_pool()
        weth = pool.token0
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        result = pool.calculate_tokens_in_from_tokens_out(token_out_quantity=10**18, token_out=weth)

        assert spy.calc_in_calls == [(False, 10**18)]
        assert result > 0

    def test_calculate_tokens_in_uses_python_path_with_override(self) -> None:
        """override_state: calc-in uses the FFI seam (calc_exact_out_v2)."""
        pool = self._make_pool()
        usdc = pool.token1
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        override = UniswapV2PoolState(
            address=pool.address,
            block=None,
            reserves_token0=2000 * 10**18,
            reserves_token1=4_000_000 * 10**6,
        )

        result = pool.calculate_tokens_in_from_tokens_out(
            token_out_quantity=10**6, token_out=usdc, override_state=override
        )

        assert spy.calc_in_calls == []
        assert spy.calc_out_calls == []
        assert result == calc_exact_out_v2(
            override.reserves_token0,
            override.reserves_token1,
            10**6,
            3,
            1000,
        )

    def test_calculate_tokens_in_raises_on_overdraw_via_rust(self) -> None:
        """Non-override overdraw: Rust returns 0, Python raises LiquidityPoolError."""
        pool = self._make_pool()
        usdc = pool.token1
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        with pytest.raises(LiquidityPoolError):
            pool.calculate_tokens_in_from_tokens_out(
                token_out_quantity=2_000_000_000_001,
                token_out=usdc,
            )

        assert spy.calc_in_calls == [(True, 2_000_000_000_001)]

    def test_calculate_tokens_out_rejects_zero_input(self) -> None:
        """InvalidSwapInputAmount still raised before delegation is attempted."""
        pool = self._make_pool()
        weth = pool.token0
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        with pytest.raises(InvalidSwapInputAmount):
            pool.calculate_tokens_out_from_tokens_in(token_in=weth, token_in_quantity=0)

        assert spy.calc_out_calls == []

    def test_calculate_tokens_out_rejects_unknown_token(self) -> None:
        """Unknown token_in raises DegenbotValueError before delegation."""
        pool = self._make_pool()
        bogus = make_erc20(
            PY_BOT,
            "0x0000000000000000000000000000000000000009",
            chain_id=1,
            name="Bogus",
            symbol="BOG",
            decimals=18,
        )
        spy = _DelegateSpy(pool._py_pool)
        pool._py_pool = spy

        with pytest.raises(DegenbotValueError):
            pool.calculate_tokens_out_from_tokens_in(token_in=bogus, token_in_quantity=10**18)

        assert spy.calc_out_calls == []


class TestV2PoolTrackerWithBot:
    """UniswapV2PoolTracker delegates to Bot when available."""

    def test_tracker_uses_bot_build_pool(self, tmp_path: pathlib.Path) -> None:
        """When a manager has a bot, get_pool delegates to bot.build_pool."""
        config = make_test_config(tmp_path)
        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        bot = Bot(config, provider=provider)

        factory = UNISWAP_V2_FACTORY
        manager = bot.add_tracker(UniswapV2PoolTracker, factory_address=factory)

        assert manager._bot is bot

        weth = make_weth()
        usdc = make_usdc()
        mock_pool = make_v2_pool(
            address=WETH_USDC_V2_POOL,
            chain_id=1,
            token0=weth,
            token1=usdc,
            factory=factory,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1000 * 10**18,
            reserves_token1=2_000_000 * 10**6,
            state_block=18_000_000,
        )

        bot.pools.add(pool_address=mock_pool.address, chain_id=1, pool=mock_pool)

        pool = manager.get_pool(WETH_USDC_V2_POOL)
        assert pool is mock_pool
        assert pool.address == get_checksum_address(WETH_USDC_V2_POOL)

    def test_manager_builds_pool_via_bot(self, tmp_path: pathlib.Path) -> None:
        """Manager builds a new pool via bot.build_pool when not in registry."""
        weth_addr = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
        usdc_addr = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
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

        manager = bot.add_tracker(UniswapV2PoolTracker, factory_address=factory_addr)

        pool = manager.get_pool(WETH_USDC_V2_POOL)
        assert isinstance(pool, UniswapV2Pool)
        assert pool.address == get_checksum_address(WETH_USDC_V2_POOL)
        assert pool.token0.address == get_checksum_address(weth_addr)
        assert pool.token1.address == get_checksum_address(usdc_addr)

        assert pool.address in manager._tracked_pools
        assert bot.pools.get(pool_address=pool.address, chain_id=1) is pool

        pool2 = manager.get_pool(WETH_USDC_V2_POOL)
        assert pool2 is pool


class TestV3PoolTrackerWithBot:
    """UniswapV3PoolTracker delegates to Bot when available."""

    def test_tracker_uses_bot_pools_registry(self, tmp_path: pathlib.Path) -> None:
        """When a manager has a bot, get_pool checks bot.pools first."""
        config = make_test_config(tmp_path)
        provider = MagicMock()
        provider.chain_id = 1
        provider.is_connected.return_value = True
        provider.get_block_number.return_value = 18_000_000
        bot = Bot(config, provider=provider)

        factory = UNISWAP_V3_FACTORY
        manager = bot.add_tracker(UniswapV3PoolTracker, factory_address=factory)

        assert manager._bot is bot

        weth = make_weth()
        usdc = make_usdc()
        mock_pool = make_v3_pool(
            address=USDC_WETH_V3_POOL,
            token0=weth,
            token1=usdc,
            factory=factory,
            fee=V3_FEE,
            tick_spacing=V3_TICK_SPACING,
            sqrt_price_x96=2198666895605149686863,
            tick=-76020,
            liquidity=1234567890,
            state_block=18_000_000,
        )
        bot.pools.add(pool_address=mock_pool.address, chain_id=1, pool=mock_pool)

        pool = manager.get_pool(USDC_WETH_V3_POOL)
        assert pool is mock_pool


class TestBotTokenIOMethods:
    """Bot.get_token_balance/approval/total_supply use cache + RPC."""

    def test_get_token_balance_cache_hit(self, tmp_path: pathlib.Path) -> None:
        """Balance returned from cache without RPC call."""
        token = make_erc20(
            PY_BOT,
            "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",
            chain_id=1,
            name="Wrapped BTC",
            symbol="WBTC",
            decimals=8,
        )
        holder = "0x" + "11" * 20
        token.set_cached_balance(holder, block_number=100, balance=10**18)

        config = make_test_config(tmp_path)
        provider = MagicMock()
        provider.chain_id = 1
        bot = Bot(config, provider=provider)
        provider.is_connected.return_value = True

        balance = bot.get_token_balance(token, holder, block_identifier=100)
        assert balance == 10**18
        provider.call.assert_not_called()

    def test_get_token_balance_cache_miss(self, tmp_path: pathlib.Path) -> None:
        """Balance fetched from chain (offline cassette) on cache miss, then cached."""
        token = make_erc20(
            PY_BOT,
            "0xDe30239bB7673E3021A8cF8c6B7Df7Af60e9C0d6",
            chain_id=1,
            name="Golem",
            symbol="GLM",
            decimals=18,
        )
        holder = "0x" + "11" * 20
        expected_balance = 5 * 10**18

        config = make_test_config(tmp_path)

        balance_of_calldata = (
            bytes.fromhex("70a08231") + abi_encode(types=["address"], args=[holder])
        ).hex()
        encoded_balance = abi_encode(types=["uint256"], args=[expected_balance])
        offline = OfflineProvider(
            chain_id=1,
            blocks={
                "200": {
                    "timestamp": 1776986723,
                    "calls": {
                        f"{token.address.lower()}:0x{balance_of_calldata}": (encoded_balance.hex()),
                    },
                    "code": {},
                }
            },
        )
        bot = Bot(config, provider=offline)

        balance = bot.get_token_balance(token, holder)
        assert balance == expected_balance
        assert token.get_cached_balance(holder, block_number=200) == expected_balance

    def test_get_token_approval_cache_hit(self, tmp_path: pathlib.Path) -> None:
        token = make_erc20(
            PY_BOT,
            "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984",
            chain_id=1,
            name="Uniswap",
            symbol="UNI",
            decimals=18,
        )
        owner = "0x" + "11" * 20
        spender = "0x" + "22" * 20
        token.set_cached_approval(block_number=100, owner=owner, spender=spender, amount=500)

        config = make_test_config(tmp_path)
        provider = MagicMock()
        provider.chain_id = 1
        bot = Bot(config, provider=provider)
        provider.is_connected.return_value = True

        approval = bot.get_token_approval(token, owner, spender, block_identifier=100)
        assert approval == 500
        provider.call.assert_not_called()

    def test_get_token_total_supply_cache_hit(self, tmp_path: pathlib.Path) -> None:
        token = make_erc20(
            PY_BOT,
            "0x514910771AF9Ca656af840dff83E8264EcF986CA",
            chain_id=1,
            name="Chainlink",
            symbol="LINK",
            decimals=18,
        )
        token.set_cached_total_supply(block_number=100, total_supply=10**27)

        config = make_test_config(tmp_path)
        provider = MagicMock()
        provider.chain_id = 1
        bot = Bot(config, provider=provider)
        provider.is_connected.return_value = True

        supply = bot.get_token_total_supply(token, block_identifier=100)
        assert supply == 10**27
        provider.call.assert_not_called()


class TestRateProviderIsStored:
    """The rate provider is the stored Rust trait object, read off the handle."""

    def test_static_pool_reports_static_provider(self) -> None:
        """A pool with no injected provider reports the static fallback."""
        bot = _Engine()
        t0 = make_erc20(bot, address="0x" + "d4" * 20, name="S0", symbol="S0", decimals=18)
        t1 = make_erc20(bot, address="0x" + "e5" * 20, name="S1", symbol="S1", decimals=18)
        pool = make_balancer_stable_pool(
            address="0x" + "f6" * 20,
            pool_id=bytes.fromhex("f6" * 20 + "0002" + "0" * 20),
            vault="0x" + "ba" * 20,
            tokens=[t0, t1],
            balances=[10**18, 2 * 10**18],
            fee=Fraction(3, 1000),
            amp=100_000,
            scaling_factors=[10**18, 10**18],
            py_bot=bot,
        )

        assert pool._rate_provider_is_static is True
        assert pool.rate_provider is None
        assert not pool.requires_io_at_calculation_time

    def test_dynamic_pool_reports_non_static(self) -> None:
        """A pool with an injected provider reports the stored (dynamic) provider."""

        class _CaptureProvider:
            def __init__(self, rates: tuple[int, ...]) -> None:
                self._rates = rates

            def get_rates(self, block_identifier=None):
                return self._rates

        bot = _Engine()
        t0 = make_erc20(bot, address="0x" + "d4" * 20, name="S0", symbol="S0", decimals=18)
        t1 = make_erc20(bot, address="0x" + "e5" * 20, name="S1", symbol="S1", decimals=18)
        rates = (10**18, 2 * 10**18, 3 * 10**18)
        pool = make_balancer_stable_pool(
            address="0x" + "07" * 20,
            pool_id=bytes.fromhex("07" * 20 + "0002" + "0" * 20),
            vault="0x" + "ba" * 20,
            tokens=[t0, t1],
            balances=[10**18, 2 * 10**18],
            fee=Fraction(3, 1000),
            amp=100_000,
            scaling_factors=[10**18, 2 * 10**18],
            bpt_idx=None,
            rate_provider=_CaptureProvider(rates),
            py_bot=bot,
        )

        assert pool._rate_provider_is_static is False
        assert pool.rate_provider is not None
        assert pool.rate_provider.get_rates(42) == rates


class TestSingleArgReadsIdentityOffHandle:
    """The companion reads every field off the handle (single-arg end state)."""

    def test_plain_pool_identity_round_trips(self) -> None:
        bot = _Engine()
        t0 = make_erc20(bot, address="0x" + "d4" * 20, name="DAI", symbol="DAI", decimals=18)
        t1 = make_erc20(bot, address="0x" + "e5" * 20, name="USDC", symbol="USDC", decimals=6)
        pool = make_curve_pool(
            "0x" + "f6" * 20,
            tokens=[t0, t1],
            a_coefficient=2000,
            fee=4_000_000,
            admin_fee=5_000_000_000,
            balances=[10**18, 2 * 10**18],
            py_bot=bot,
        )
        assert pool.address == get_checksum_address("0x" + "f6" * 20)
        assert pool.fee == 4_000_000
        assert pool.a_coefficient == 2000
        assert [t.address for t in pool.tokens] == [t0.address, t1.address]
        assert pool.rate_multipliers[1] == 10**30


class TestGoBetweenResolvesBasePool:
    """``curve_base_pool()`` + ``_LazyBasePool`` recover the base companion."""

    def test_metapool_resolves_base_pool_through_handle(self) -> None:
        bot = _Engine()
        bt0 = make_erc20(bot, address="0x" + "11" * 20, name="B0", symbol="B0", decimals=18)
        bt1 = make_erc20(bot, address="0x" + "22" * 20, name="B1", symbol="B1", decimals=18)
        blp = make_erc20(bot, address="0x" + "33" * 20, name="BLP", symbol="BLP", decimals=18)
        base_pool = make_curve_pool(
            "0x" + "44" * 20,
            tokens=[bt0, bt1],
            a_coefficient=100,
            fee=1_000_000,
            admin_fee=5_000_000_000,
            balances=[10**18, 2 * 10**18],
            lp_token=blp,
            py_bot=bot,
        )

        assert base_pool._py_pool.curve_base_pool() is None

        mt0 = make_erc20(bot, address="0x" + "55" * 20, name="M0", symbol="M0", decimals=18)
        mlp = make_erc20(bot, address="0x" + "66" * 20, name="MLP", symbol="MLP", decimals=18)
        meta = make_curve_pool(
            "0x" + "77" * 20,
            tokens=[mt0, mlp],
            a_coefficient=100,
            fee=1_000_000,
            admin_fee=5_000_000_000,
            balances=[10**18, 2 * 10**18],
            lp_token=mlp,
            base_pool=base_pool,
            py_bot=bot,
        )

        assert isinstance(meta.base_pool, BasePoolPort)
        assert meta.base_pool.balances == base_pool.balances
        assert meta.base_pool.fee == base_pool.fee


class TestStubBasePoolSatisfiesPort:
    """A canned ``StubBasePool`` satisfies ``BasePoolPort``."""

    def test_stub_is_a_base_pool_port(self) -> None:
        class StubBasePool:
            @property
            def tokens(self) -> tuple:
                return ()

            @property
            def balances(self) -> tuple[int, ...]:
                return (10**18, 10**18)

            @property
            def fee(self) -> int:
                return 1_000_000

            def calc_token_amount(self, *, amounts, deposit, **_):
                return sum(amounts)

            def get_dy(self, i, j, dx, **_):
                return dx

            def calc_withdraw_one_coin(self, _token_amount, i, **_):
                return (_token_amount,)

        stub = StubBasePool()
        assert isinstance(stub, BasePoolPort)
        inputs = DyCalculationInputs(
            PRECISION=10**18,
            FEE_DENOMINATOR=10**10,
            fee=stub.fee,
            n_coins=2,
            balances=stub.balances,
            rate_multipliers=(10**18, 10**18),
            precision_multipliers=(1, 1),
            offpeg_fee_multiplier=0,
            fee_gamma=0,
            mid_fee=0,
            out_fee=0,
            address="0x" + "0" * 40,
            resolved_rates=(10**18, 10**18),
            xp=(10**18, 10**18),
            block_number=0,
            block_timestamp=0,
            amp=100,
            base_pool=stub,
        )
        assert inputs.base_pool is stub
