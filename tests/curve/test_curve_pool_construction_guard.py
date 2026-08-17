"""CurveStableswapPool sealed _from_py_pool seam (ADR-005 BQM2OA).

``_from_py_pool(cls, py_pool) -> Self`` takes ONLY the Rust handle. Every
identity field + the stored data-provider trait object + the cross-pool
base-pool reference are recovered off the handle — the Polars ``_from_pydf``
end state for Curve, completing the seam epic for every pool family.

The base pool is recovered via the Rust go-between ``curve_base_pool()``
(same shared ``BotState``, no Python registry), wrapped in a ``_LazyBasePool``
that memoises construction. The calculator depends on it through the named
``BasePoolPort`` interface (six members), which a ``StubBasePool`` also
satisfies — unlocking metapool math unit tests that previously couldn't
call ``.calculate()`` without standing up a full pool.
"""

from __future__ import annotations

from fractions import Fraction
from unittest.mock import MagicMock

import pytest

from degenbot._ffi import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.curve.types import BasePoolPort, DyCalculationInputs
from degenbot.exceptions import DegenbotValueError
from tests.helpers.curve_pool_factory import make_curve_pool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool


class TestDirectConstructionForbidden:
    """``CurveStableswapPool(...)`` raises ``TypeError`` with a helpful message."""

    def test_no_args_raises_type_error(self) -> None:
        with pytest.raises(TypeError) as exc_info:
            CurveStableswapPool()
        message = str(exc_info.value)
        assert "Bot.build_pool" in message
        assert "make_curve_pool" in message

    def test_old_kwargs_raise_type_error(self) -> None:
        """The pre-seam ``(handle, *, address=..., tokens=..., …)`` shape is rejected."""
        fake_handle = MagicMock()
        with pytest.raises(TypeError) as exc_info:
            CurveStableswapPool(
                fake_handle,
                address="0x" + "0" * 40,
                tokens=[MagicMock()],
                a_coefficient=100,
                fee=4_000_000,
                admin_fee=0,
            )
        assert "make_curve_pool" in str(exc_info.value)


class TestWrongFamilyHandle:
    """``_from_py_pool`` rejects a non-Curve handle."""

    def test_v2_handle_raises_degenbot_value_error(self) -> None:
        bot = Bot()
        t0 = make_erc20(bot, address="0x" + "a1" * 20, name="T0", symbol="T0", decimals=18)
        t1 = make_erc20(bot, address="0x" + "b2" * 20, name="T1", symbol="T1", decimals=18)
        v2_pool = make_v2_pool(
            "0x" + "c3" * 20,
            token0=t0,
            token1=t1,
            factory="0x" + "ff" * 20,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=10**18,
            reserves_token1=10**18,
            py_bot=bot,
        )
        with pytest.raises(DegenbotValueError) as exc_info:
            CurveStableswapPool._from_py_pool(v2_pool._py_pool)
        assert "Curve stableswap" in str(exc_info.value)


class TestSingleArgReadsIdentityOffHandle:
    """The companion reads every field off the handle (single-arg end state)."""

    def test_plain_pool_identity_round_trips(self) -> None:
        bot = Bot()
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
        # rate_multipliers derived from token decimals (18, 6) → (1e18, 1e30).
        assert pool.rate_multipliers[1] == 10**30


class TestGoBetweenResolvesBasePool:
    """``curve_base_pool()`` + ``_LazyBasePool`` recover the base companion."""

    def test_metapool_resolves_base_pool_through_handle(self) -> None:
        # The base + metapool share a bot; the metapool's handle resolves the
        # base pool's address → pool_id → base handle (Rust go-between), and
        # _LazyBasePool memoises the wrapped companion.
        bot = Bot()
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

        # The handle's go-between resolves the base by address.
        # base_pool._py_pool.curve_base_pool()  is None for a plain pool.
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

        # base_pool is a BasePoolPort (the metapool's lazy base wrapper).
        assert isinstance(meta.base_pool, BasePoolPort)
        # The wrapper delegates to the recovered base companion; balances match.
        assert meta.base_pool.balances == base_pool.balances
        assert meta.base_pool.fee == base_pool.fee


class TestStubBasePoolSatisfiesPort:
    """A canned ``StubBasePool`` satisfies ``BasePoolPort`` — the calculator
    can be unit-tested without standing up a full pool (the seam this names).
    """

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
        # A DyCalculationInputs carrying the stub type-checks structurally.
        _inputs = DyCalculationInputs(
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
        assert _inputs.base_pool is stub
