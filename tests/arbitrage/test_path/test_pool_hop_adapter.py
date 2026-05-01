"""Tests for pool_hop_adapter — pure-function extraction of hop state and fee.

Uses lightweight fake pools to avoid RPC dependencies. The adapter
supports duck-typing (calls pool.to_hop_state / extract_fee directly if
present) and named dispatch for concrete classes.
"""

from fractions import Fraction

import pytest

from degenbot.arbitrage.path.pool_hop_adapter import extract_fee, to_hop_state
from degenbot.types.hop_types import ConstantProductHop

from tests.arbitrage.test_path.conftest import (
    FakeAerodromeV2Pool,
    FakeConcentratedLiquidityPool,
    FakeToken,
    FakeUniswapV2Pool,
    FakeV2PoolState,
)


class _NoAttrsPool:
    """Pool with no to_hop_state, no extract_fee."""

    def __init__(self) -> None:
        self.state = object()


def _make_token(address: str, decimals: int = 18) -> FakeToken:
    return FakeToken(address, decimals)


class TestExtractFee:
    def test_v2_zero_for_one(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeUniswapV2Pool(t0, t1, fee=Fraction(7, 1000))
        assert extract_fee(pool, zero_for_one=True) == Fraction(7, 1000)

    def test_v2_one_for_zero(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeUniswapV2Pool(t0, t1, fee=Fraction(7, 1000))
        assert extract_fee(pool, zero_for_one=False) == Fraction(7, 1000)

    def test_cl(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeConcentratedLiquidityPool(t0, t1, fee=3000)
        assert extract_fee(pool, zero_for_one=True) == Fraction(3000, 1_000_000)

    def test_aerodrome(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeAerodromeV2Pool(t0, t1, fee=Fraction(5, 1000))
        assert extract_fee(pool, zero_for_one=True) == Fraction(5, 1000)

    def test_unknown_type_raises_typeerror(self):
        pool = _NoAttrsPool()
        with pytest.raises(TypeError):
            extract_fee(pool, zero_for_one=True)


class TestToHopState:
    def test_v2_zero_for_one(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeUniswapV2Pool(t0, t1, reserve0=1000, reserve1=2000)
        hop = to_hop_state(pool, zero_for_one=True)
        assert isinstance(hop, ConstantProductHop)
        assert hop.reserve_in == 1000
        assert hop.reserve_out == 2000

    def test_v2_one_for_zero(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeUniswapV2Pool(t0, t1, reserve0=1000, reserve1=2000)
        hop = to_hop_state(pool, zero_for_one=False)
        assert isinstance(hop, ConstantProductHop)
        assert hop.reserve_in == 2000
        assert hop.reserve_out == 1000

    def test_v2_with_state_override(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeUniswapV2Pool(t0, t1, reserve0=1000, reserve1=2000)
        override = FakeV2PoolState(
            address=pool.address,
            block=None,
            reserves_token0=500,
            reserves_token1=1500,
        )
        hop = to_hop_state(pool, zero_for_one=True, state_override=override)
        assert hop.reserve_in == 500
        assert hop.reserve_out == 1500

    def test_unknown_type_raises_typeerror(self):
        pool = _NoAttrsPool()
        with pytest.raises(TypeError):
            to_hop_state(pool, zero_for_one=True)

    def test_duck_typing_calls_pool_method(self):
        """When a pool already exposes to_hop_state, the adapter delegates."""
        called_with: dict[str, object] = {}

        class DuckPool:
            def __init__(self) -> None:
                pass

            def to_hop_state(
                self,
                zero_for_one: bool,  # noqa: FBT001
                state_override: object = None,
            ) -> ConstantProductHop:
                called_with["zfo"] = zero_for_one
                called_with["state"] = state_override
                return ConstantProductHop(
                    reserve_in=999,
                    reserve_out=888,
                    fee=Fraction(1, 100),
                )

        pool = DuckPool()
        hop = to_hop_state(pool, zero_for_one=True, state_override="ovr")
        assert called_with["zfo"] is True
        assert called_with["state"] == "ovr"
        assert hop.reserve_in == 999

    def test_duck_typing_extract_fee_delegates(self):
        called_with: dict[str, object] = {}

        class FeePool:
            def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001
                called_with["zfo"] = zero_for_one
                return Fraction(42, 1000)

        assert extract_fee(FeePool(), zero_for_one=False) == Fraction(42, 1000)
        assert called_with["zfo"] is False

    def test_aerodrome_stable_incompatible(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeAerodromeV2Pool(t0, t1, stable=True)
        from degenbot.exceptions.arbitrage import IncompatiblePoolInvariant

        with pytest.raises(IncompatiblePoolInvariant):
            to_hop_state(pool, zero_for_one=True)
