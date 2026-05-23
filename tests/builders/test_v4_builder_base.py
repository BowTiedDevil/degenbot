"""Tests for V4BuilderBase shared pure-logic helpers.

Verifies that decode, extract, and snapshot-loading helpers produce
correct outputs from known inputs, independent of I/O.
"""

import eth_abi.abi

from degenbot.builders.v4_builder_base import (
    V4BuilderBase,
    V4DbValues,
    V4Slot0Data,
)
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick

# --- decode_slot0 ---


class TestDecodeSlot0:
    """V4BuilderBase.decode_slot0 decodes sqrt_price, tick, protocol fees, lp_fee."""

    def test_decodes_all_fields(self):
        sqrt_price_x96 = 79228162514264337593543950336
        tick = -100
        # Pack protocol fee: one_to_zero = 0xABC (2748), zero_to_one = 0x123 (291)
        protocol_fee = (0xABC << 12) | 0x123
        lp_fee = 500

        slot0_result = eth_abi.abi.encode(
            ["uint160", "int24", "uint24", "uint24"],
            [sqrt_price_x96, tick, protocol_fee, lp_fee],
        )

        data = V4BuilderBase.decode_slot0(slot0_result)

        assert isinstance(data, V4Slot0Data)
        assert data.sqrt_price_x96 == sqrt_price_x96
        assert data.tick == tick
        assert data.protocol_fee_one_to_zero == 0xABC
        assert data.protocol_fee_zero_to_one == 0x123
        assert data.lp_fee == lp_fee

    def test_zero_protocol_fees(self):
        slot0_result = eth_abi.abi.encode(
            ["uint160", "int24", "uint24", "uint24"],
            [12345, 0, 0, 0],
        )

        data = V4BuilderBase.decode_slot0(slot0_result)
        assert data.protocol_fee_one_to_zero == 0
        assert data.protocol_fee_zero_to_one == 0
        assert data.lp_fee == 0

    def test_max_packed_protocol_fee(self):
        """Max uint24 = 0xFFFFFF → one_to_zero = 0xFFF, zero_to_one = 0xFFF."""
        protocol_fee = 0xFFFFFF

        slot0_result = eth_abi.abi.encode(
            ["uint160", "int24", "uint24", "uint24"],
            [0, 0, protocol_fee, 0],
        )

        data = V4BuilderBase.decode_slot0(slot0_result)
        assert data.protocol_fee_one_to_zero == 0xFFF
        assert data.protocol_fee_zero_to_one == 0xFFF


# --- extract_db_values ---


class TestExtractDbValues:
    """V4BuilderBase.extract_db_values maps a V4 DB row to typed values."""

    def test_extracts_from_v4_table_row(self):
        class FakeManager:
            state_view = "0xStateView1234567890abcdef1234567890abcdef12"

        class FakeToken:
            address = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"

        class FakeToken2:
            address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

        class FakeDbRow:
            currency0 = FakeToken()
            currency1 = FakeToken2()
            hooks = "0x0000000000000000000000000000000000000000"
            tick_spacing = 10
            fee_currency0 = 500
            manager = FakeManager()

        result = V4BuilderBase.extract_db_values(FakeDbRow())

        assert isinstance(result, V4DbValues)
        assert result.tick_spacing == 10
        assert result.fee == 500
        assert result.state_view_address == "0xStateView1234567890abcdef1234567890abcdef12"


# --- load_tick_snapshot ---


class TestLoadTickSnapshot:
    """V4BuilderBase.load_tick_snapshot loads tick bitmap/data from DB relationships."""

    def test_returns_empty_and_not_loaded_when_no_data(self):
        class FakePoolWithData:
            initialization_maps = []
            liquidity_positions = []
            liquidity_update_block = 0

        bitmap, tick_data, loaded = V4BuilderBase.load_tick_snapshot(FakePoolWithData())
        assert bitmap == {}
        assert tick_data == {}
        assert loaded is False

    def test_loads_from_re_queried_row_with_data(self):
        class FakeInitMap:
            word = 3
            bitmap = 65535

        class FakeLiqPosition:
            tick = -887220
            liquidity_net = 5000
            liquidity_gross = 10000

        class FakePoolWithData:
            initialization_maps = [FakeInitMap()]
            liquidity_positions = [FakeLiqPosition()]
            liquidity_update_block = 100

        bitmap, tick_data, loaded = V4BuilderBase.load_tick_snapshot(FakePoolWithData())

        assert loaded is True
        assert 3 in bitmap
        assert isinstance(bitmap[3], BitmapAtWord)
        assert bitmap[3].bitmap == 65535
        assert bitmap[3].block == 100
        assert -887220 in tick_data
        assert isinstance(tick_data[-887220], LiquidityAtTick)
        assert tick_data[-887220].liquidity_net == 5000
        assert tick_data[-887220].liquidity_gross == 10000

    def test_returns_not_loaded_when_positions_empty(self):
        class FakeInitMap:
            word = 0
            bitmap = 1

        class FakePoolWithData:
            initialization_maps = [FakeInitMap()]
            liquidity_positions = []
            liquidity_update_block = 1

        _, _, loaded = V4BuilderBase.load_tick_snapshot(FakePoolWithData())
        assert loaded is False

    def test_liquidity_update_block_defaults_to_zero(self):
        class FakeInitMap:
            word = 0
            bitmap = 1

        class FakeLiqPosition:
            tick = 10
            liquidity_net = 100
            liquidity_gross = 200

        class FakePoolWithData:
            initialization_maps = [FakeInitMap()]
            liquidity_positions = [FakeLiqPosition()]
            liquidity_update_block = None

        bitmap, tick_data, loaded = V4BuilderBase.load_tick_snapshot(FakePoolWithData())

        assert loaded is True
        assert bitmap[0].block == 0
        assert tick_data[10].block == 0


# --- resolve_tick_data_args ---


class TestResolveTickDataArgs:
    """V4BuilderBase.resolve_tick_data_args decides tick data passing for pool constructor."""

    def test_returns_none_when_no_tick_data(self):
        tick_bitmap, tick_data_arg = V4BuilderBase.resolve_tick_data_args(
            working_tick_data={},
            working_tick_bitmap={0: BitmapAtWord(bitmap=1, block=1)},
        )
        assert tick_bitmap is None
        assert tick_data_arg is None

    def test_returns_values_when_tick_data_present(self):
        bitmap = {0: BitmapAtWord(bitmap=1, block=1)}
        ticks = {10: LiquidityAtTick(liquidity_net=1, liquidity_gross=2, block=1)}

        tick_bitmap, tick_data_arg = V4BuilderBase.resolve_tick_data_args(
            working_tick_data=ticks,
            working_tick_bitmap=bitmap,
        )
        assert tick_bitmap is bitmap
        assert tick_data_arg is ticks
