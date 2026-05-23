"""Tests for AbstractSwapAmounts.input_amount() and output_amount() methods.

Validates that each SwapAmounts subclass correctly returns the input and
output amounts through the protocol methods.
"""

from eth_typing import ChecksumAddress

from degenbot.arbitrage.types import (
    CurveStableSwapPoolSwapAmounts,
    UniswapV2PoolSwapAmounts,
    UniswapV3PoolSwapAmounts,
    UniswapV4PoolSwapAmounts,
    V4PoolKey,
)
from tests.fakes.tokens import FakeToken


class TestUniswapV2InputOutputAmounts:
    def test_input_amount_zero_for_one(self):
        swap = UniswapV2PoolSwapAmounts(
            pool=ChecksumAddress("0x" + "a" * 40),
            amounts_in=(1000, 0),
            amounts_out=(0, 900),
        )
        assert swap.input_amount() == 1000

    def test_output_amount_zero_for_one(self):
        swap = UniswapV2PoolSwapAmounts(
            pool=ChecksumAddress("0x" + "a" * 40),
            amounts_in=(1000, 0),
            amounts_out=(0, 900),
        )
        assert swap.output_amount() == 900

    def test_input_amount_one_for_zero(self):
        swap = UniswapV2PoolSwapAmounts(
            pool=ChecksumAddress("0x" + "a" * 40),
            amounts_in=(0, 2000),
            amounts_out=(1800, 0),
        )
        assert swap.input_amount() == 2000

    def test_output_amount_one_for_zero(self):
        swap = UniswapV2PoolSwapAmounts(
            pool=ChecksumAddress("0x" + "a" * 40),
            amounts_in=(0, 2000),
            amounts_out=(1800, 0),
        )
        assert swap.output_amount() == 1800


class TestUniswapV3InputOutputAmounts:
    def test_input_amount(self):
        swap = UniswapV3PoolSwapAmounts(
            pool=ChecksumAddress("0x" + "a" * 40),
            amount_in=5000,
            amount_out=4500,
            amount_specified=-5000,
            zero_for_one=True,
            sqrt_price_limit_x96=4295128739,
        )
        assert swap.input_amount() == 5000

    def test_output_amount(self):
        swap = UniswapV3PoolSwapAmounts(
            pool=ChecksumAddress("0x" + "a" * 40),
            amount_in=5000,
            amount_out=4500,
            amount_specified=-5000,
            zero_for_one=True,
            sqrt_price_limit_x96=4295128739,
        )
        assert swap.output_amount() == 4500


class TestUniswapV4InputOutputAmounts:
    def test_input_amount(self):
        dummy_key = V4PoolKey(
            currency0=ChecksumAddress("0x" + "0" * 40),
            currency1=ChecksumAddress("0x" + "1" * 40),
            fee=3000,
            tick_spacing=60,
            hooks=ChecksumAddress("0x" + "0" * 40),
        )
        swap = UniswapV4PoolSwapAmounts(
            address=ChecksumAddress("0x" + "a" * 40),
            id=b"\x01" * 32,
            pool_key=dummy_key,
            amount_in=7000,
            amount_out=6500,
            amount_specified=-7000,
            zero_for_one=True,
            sqrt_price_limit_x96=4295128739,
        )
        assert swap.input_amount() == 7000

    def test_output_amount(self):
        dummy_key = V4PoolKey(
            currency0=ChecksumAddress("0x" + "0" * 40),
            currency1=ChecksumAddress("0x" + "1" * 40),
            fee=3000,
            tick_spacing=60,
            hooks=ChecksumAddress("0x" + "0" * 40),
        )
        swap = UniswapV4PoolSwapAmounts(
            address=ChecksumAddress("0x" + "a" * 40),
            id=b"\x01" * 32,
            pool_key=dummy_key,
            amount_in=7000,
            amount_out=6500,
            amount_specified=-7000,
            zero_for_one=True,
            sqrt_price_limit_x96=4295128739,
        )
        assert swap.output_amount() == 6500


class TestCurveInputOutputAmounts:
    def test_input_amount(self):
        token0 = FakeToken("0xt0")
        token1 = FakeToken("0xt1")
        swap = CurveStableSwapPoolSwapAmounts(
            pool=ChecksumAddress("0x" + "a" * 40),
            token_in=token0,
            token_in_index=0,
            token_out=token1,
            token_out_index=1,
            amount_in=3000,
            min_amount_out=2900,
            underlying=False,
        )
        assert swap.input_amount() == 3000

    def test_output_amount(self):
        token0 = FakeToken("0xt0")
        token1 = FakeToken("0xt1")
        swap = CurveStableSwapPoolSwapAmounts(
            pool=ChecksumAddress("0x" + "a" * 40),
            token_in=token0,
            token_in_index=0,
            token_out=token1,
            token_out_index=1,
            amount_in=3000,
            min_amount_out=2900,
            underlying=False,
        )
        assert swap.output_amount() == 2900
