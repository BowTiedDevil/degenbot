"""Tests for V3BuilderBase shared pure-logic helpers.

Verifies that decode, extract, and snapshot-loading helpers produce
correct outputs from known inputs, independent of I/O.
"""

from dataclasses import dataclass, field

import eth_abi.abi

from degenbot.builders.v3_builder_base import (
    V3BuilderBase,
    V3DbValues,
    V3ImmutableData,
    V3Slot0Data,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick

# --- decode_immutable_data ---


class TestDecodeImmutableData:
    """V3BuilderBase.decode_immutable_data decodes factory, token addresses, fee, tick_spacing."""

    def test_decodes_all_fields(self):
        factory = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
        token0 = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
        token1 = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        fee = 3000
        tick_spacing = 60

        factory_result = eth_abi.abi.encode(["address"], [factory])
        token0_result = eth_abi.abi.encode(["address"], [token0])
        token1_result = eth_abi.abi.encode(["address"], [token1])
        fee_result = eth_abi.abi.encode(["uint24"], [fee])
        tick_spacing_result = eth_abi.abi.encode(["int24"], [tick_spacing])

        result = V3BuilderBase.decode_immutable_data(
            factory_result=factory_result,
            token0_result=token0_result,
            token1_result=token1_result,
            fee_result=fee_result,
            tick_spacing_result=tick_spacing_result,
        )

        assert isinstance(result, V3ImmutableData)
        # get_checksum_address returns EIP-55 mixed-case checksums
        assert result.factory == get_checksum_address(factory)
        assert result.token0_address == get_checksum_address(token0)
        assert result.token1_address == get_checksum_address(token1)
        assert result.fee == fee
        assert result.tick_spacing == tick_spacing

    def test_negative_tick_spacing(self):
        """tick_spacing is int24 — can be negative in edge cases."""
        factory = "0x0000000000000000000000000000000000000001"
        token0 = "0x0000000000000000000000000000000000000002"
        token1 = "0x0000000000000000000000000000000000000003"

        factory_result = eth_abi.abi.encode(["address"], [factory])
        token0_result = eth_abi.abi.encode(["address"], [token0])
        token1_result = eth_abi.abi.encode(["address"], [token1])
        fee_result = eth_abi.abi.encode(["uint24"], [500])
        # int24 range: -8388608 to 8388607
        tick_spacing_result = eth_abi.abi.encode(["int24"], [-1])

        result = V3BuilderBase.decode_immutable_data(
            factory_result=factory_result,
            token0_result=token0_result,
            token1_result=token1_result,
            fee_result=fee_result,
            tick_spacing_result=tick_spacing_result,
        )

        assert result.tick_spacing == -1


# --- decode_slot0 ---


class TestDecodeSlot0:
    """V3BuilderBase.decode_slot0 decodes sqrt_price_x96 and tick from V3 slot0()."""

    def test_decodes_sqrt_price_and_tick(self):
        sqrt_price_x96 = 79228162514264337593543950336  # ~1:1 price
        tick = -100

        slot0_result = eth_abi.abi.encode(
            ["uint160", "int24", "uint16", "uint16", "uint16", "uint8", "bool"],
            [sqrt_price_x96, tick, 0, 0, 0, 0, False],
        )

        data = V3BuilderBase.decode_slot0(slot0_result)

        assert isinstance(data, V3Slot0Data)
        assert data.sqrt_price_x96 == sqrt_price_x96
        assert data.tick == tick

    def test_ignores_extra_fields(self):
        """slot0() returns 7 values; only sqrt_price_x96 and tick matter."""
        sqrt_price_x96 = 12345
        tick = 50

        slot0_result = eth_abi.abi.encode(
            ["uint160", "int24", "uint16", "uint16", "uint16", "uint8", "bool"],
            [sqrt_price_x96, tick, 123, 456, 789, 10, True],
        )

        data = V3BuilderBase.decode_slot0(slot0_result)
        assert data.sqrt_price_x96 == sqrt_price_x96
        assert data.tick == tick

    def test_zero_values(self):
        slot0_result = eth_abi.abi.encode(
            ["uint160", "int24", "uint16", "uint16", "uint16", "uint8", "bool"],
            [0, 0, 0, 0, 0, 0, False],
        )

        data = V3BuilderBase.decode_slot0(slot0_result)
        assert data.sqrt_price_x96 == 0
        assert data.tick == 0


# --- extract_db_values ---


class TestExtractDbVales:
    """V3BuilderBase.extract_db_values maps a DB row to typed values."""

    def test_extracts_from_v3_table_row(self):
        """Extract factory, tokens, fee, tick_spacing, deployer from a V3 DB row."""

        class FakeExchange:
            factory = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
            deployer = "0x1111111111111111111111111111111111111111"

        class FakeToken:
            address = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"

        class FakeToken2:
            address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

        class FakeDbRow:
            exchange = FakeExchange()
            token0 = FakeToken()
            token1 = FakeToken2()
            fee_token0 = 3000
            fee_token1 = 3000
            tick_spacing = 60

        result = V3BuilderBase.extract_db_values(FakeDbRow())

        assert isinstance(result, V3DbValues)
        assert result.fee == 3000
        assert result.tick_spacing == 60
        assert result.deployer_address == "0x1111111111111111111111111111111111111111"

    def test_deployer_is_none_when_not_set(self):
        class FakeExchange:
            factory = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
            deployer = None

        class FakeToken:
            address = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"

        class FakeToken2:
            address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

        class FakeDbRow:
            exchange = FakeExchange()
            token0 = FakeToken()
            token1 = FakeToken2()
            fee_token0 = 500
            fee_token1 = 500
            tick_spacing = 10

        result = V3BuilderBase.extract_db_values(FakeDbRow())
        assert result.deployer_address is None


# --- load_tick_snapshot ---


class TestLoadTickSnapshot:
    """V3BuilderBase.load_tick_snapshot loads tick bitmap/data from DB relationships."""

    def test_returns_empty_and_not_loaded_when_no_data(self):
        """If re-queried row has empty relationships, returns (empty, empty, False)."""

        @dataclass
        class FakePoolWithData:
            initialization_maps: list = field(default_factory=list)
            liquidity_positions: list = field(default_factory=list)
            liquidity_update_block: int = 0

        bitmap, tick_data, loaded = V3BuilderBase.load_tick_snapshot(FakePoolWithData())
        assert bitmap == {}
        assert tick_data == {}
        assert loaded is False

    def test_loads_from_re_queried_row_with_data(self):
        """Loads initialization_maps and liquidity_positions into typed dicts."""

        class FakeInitMap:
            word = 0
            bitmap = 255  # bits 0-7 set

        class FakeLiqPosition:
            tick = 60
            liquidity_net = -1000
            liquidity_gross = 2000

        @dataclass
        class FakePoolWithData:
            initialization_maps: list = field(default_factory=list)
            liquidity_positions: list = field(default_factory=list)
            liquidity_update_block: int = 42

        bitmap, tick_data, loaded = V3BuilderBase.load_tick_snapshot(
            FakePoolWithData(
                initialization_maps=[FakeInitMap()],
                liquidity_positions=[FakeLiqPosition()],
            )
        )

        assert loaded is True
        assert 0 in bitmap
        assert isinstance(bitmap[0], BitmapAtWord)
        assert bitmap[0].bitmap == 255
        assert bitmap[0].block == 42
        assert 60 in tick_data
        assert isinstance(tick_data[60], LiquidityAtTick)
        assert tick_data[60].liquidity_net == -1000
        assert tick_data[60].liquidity_gross == 2000
        assert tick_data[60].block == 42

    def test_returns_not_loaded_when_maps_exist_but_positions_empty(self):
        """init_maps exist but positions empty → not loaded."""

        class FakeInitMap:
            word = 0
            bitmap = 255

        @dataclass
        class FakePoolWithData:
            initialization_maps: list = field(default_factory=list)
            liquidity_positions: list = field(default_factory=list)
            liquidity_update_block: int = 1

        _, _, loaded = V3BuilderBase.load_tick_snapshot(
            FakePoolWithData(
                initialization_maps=[FakeInitMap()],
            )
        )
        assert loaded is False

    def test_returns_not_loaded_when_positions_exist_but_maps_empty(self):
        """Positions exist but init_maps empty → not loaded."""

        class FakeLiqPosition:
            tick = 60
            liquidity_net = -1000
            liquidity_gross = 2000

        @dataclass
        class FakePoolWithData:
            initialization_maps: list = field(default_factory=list)
            liquidity_positions: list = field(default_factory=list)
            liquidity_update_block: int = 1

        _, _, loaded = V3BuilderBase.load_tick_snapshot(
            FakePoolWithData(
                liquidity_positions=[FakeLiqPosition()],
            )
        )
        assert loaded is False

    def test_liquidity_update_block_defaults_to_zero(self):
        """When liquidity_update_block is None, block defaults to 0."""

        class FakeInitMap:
            word = 0
            bitmap = 1

        class FakeLiqPosition:
            tick = 10
            liquidity_net = 100
            liquidity_gross = 200

        @dataclass
        class FakePoolWithData:
            initialization_maps: list = field(default_factory=list)
            liquidity_positions: list = field(default_factory=list)
            liquidity_update_block: int | None = None

        bitmap, tick_data, loaded = V3BuilderBase.load_tick_snapshot(
            FakePoolWithData(
                initialization_maps=[FakeInitMap()],
                liquidity_positions=[FakeLiqPosition()],
            )
        )

        assert loaded is True
        assert bitmap[0].block == 0
        assert tick_data[10].block == 0


# --- resolve_tick_data_args ---


class TestResolveTickDataArgs:
    """
    V3BuilderBase.resolve_tick_data_args decides whether to pass tick data to pool constructor.
    """

    def test_returns_none_when_not_loaded(self):
        tick_bitmap, tick_data_arg = V3BuilderBase.resolve_tick_data_args(
            db_snapshot_loaded=False,
            working_tick_bitmap={0: BitmapAtWord(bitmap=1, block=1)},
            working_tick_data={10: LiquidityAtTick(liquidity_net=1, liquidity_gross=2, block=1)},
        )
        assert tick_bitmap is None
        assert tick_data_arg is None

    def test_returns_none_when_loaded_but_empty_data(self):
        tick_bitmap, tick_data_arg = V3BuilderBase.resolve_tick_data_args(
            db_snapshot_loaded=True,
            working_tick_bitmap={},
            working_tick_data={},
        )
        assert tick_bitmap is None
        assert tick_data_arg is None

    def test_returns_values_when_loaded_with_data(self):
        bitmap = {0: BitmapAtWord(bitmap=1, block=1)}
        ticks = {10: LiquidityAtTick(liquidity_net=1, liquidity_gross=2, block=1)}

        tick_bitmap, tick_data_arg = V3BuilderBase.resolve_tick_data_args(
            db_snapshot_loaded=True,
            working_tick_bitmap=bitmap,
            working_tick_data=ticks,
        )
        assert tick_bitmap is bitmap
        assert tick_data_arg is ticks
