"""Tests for Uniswap log decoders."""

from __future__ import annotations

import pytest
from eth_abi import encode as abi_encode
from hexbytes import HexBytes

from degenbot.uniswap.log_decoders import (
    V2_SYNC_TOPIC,
    V3_BURN_TOPIC,
    V3_MINT_TOPIC,
    V3_SWAP_TOPIC,
    V4_MODIFY_LIQUIDITY_TOPIC,
    V4_SWAP_TOPIC,
    decode_v2_sync,
    decode_v3_burn,
    decode_v3_mint,
    decode_v3_swap,
    decode_v4_modify_liquidity,
    decode_v4_swap,
    get_block_number,
    get_log_data_bytes,
)


def _make_log(
    topic0: str,
    data: bytes,
    block_number: int = 42,
    address: str = "0x" + "ab" * 20,
    extra_topics: list[bytes] | None = None,
) -> dict:
    """Build a synthetic log dict matching the Rust subscription format."""
    topics = [HexBytes(bytes.fromhex(topic0[2:]))]
    if extra_topics:
        topics.extend(HexBytes(t) for t in extra_topics)
    return {
        "address": address,
        "topics": topics,
        "data": HexBytes(data),
        "blockNumber": block_number,
        "logIndex": 0,
    }


class TestHelperFunctions:
    def test_get_block_number(self) -> None:
        log = {"blockNumber": 42}
        assert get_block_number(log) == 42

    def test_get_block_number_missing_raises(self) -> None:
        with pytest.raises(ValueError, match="blockNumber"):
            get_block_number({})

    def test_get_log_data_bytes(self) -> None:
        data = HexBytes(b"\x01\x02")
        log = {"data": data}
        assert get_log_data_bytes(log) == bytes(data)

    def test_get_log_data_bytes_missing_raises(self) -> None:
        with pytest.raises(ValueError, match="data"):
            get_log_data_bytes({})


class TestV2SyncDecoder:
    def test_decode_returns_callable(self) -> None:
        data = abi_encode(["uint112", "uint112"], [1000, 2000])
        log = _make_log(V2_SYNC_TOPIC, data)
        result = decode_v2_sync(log)
        assert callable(result)

    def test_decode_extracts_reserves(self) -> None:
        reserve0 = 1_000_000
        reserve1 = 2_000_000
        data = abi_encode(["uint112", "uint112"], [reserve0, reserve1])
        log = _make_log(V2_SYNC_TOPIC, data, block_number=100)

        apply_fn = decode_v2_sync(log)

        # The closure should call pool.external_update with the decoded values
        # We can't easily test this without a pool instance, but we can inspect
        # the closure's captured variables
        # Instead, use a fake pool
        from tests.fakes.pools import FakeV2Pool  # noqa: PLC0415

        pool = FakeV2Pool()
        apply_fn(pool)
        assert pool.last_update is not None
        assert pool.last_update.reserves_token0 == reserve0
        assert pool.last_update.reserves_token1 == reserve1
        assert pool.last_update.block_number == 100


class TestV3SwapDecoder:
    def test_decode_returns_callable(self) -> None:
        data = abi_encode(
            ["int256", "int256", "uint160", "uint128", "int24"],
            [-100, 200, 1 << 96, 1000, 10],
        )
        log = _make_log(V3_SWAP_TOPIC, data)
        result = decode_v3_swap(log)
        assert callable(result)

    def test_decode_extracts_swap_data(self) -> None:
        sqrt_price = 1 << 96
        liquidity = 5000
        tick = -100
        data = abi_encode(
            ["int256", "int256", "uint160", "uint128", "int24"],
            [-1000, 500, sqrt_price, liquidity, tick],
        )
        log = _make_log(V3_SWAP_TOPIC, data, block_number=200)

        apply_fn = decode_v3_swap(log)
        from tests.fakes.pools import FakeV3Pool  # noqa: PLC0415

        pool = FakeV3Pool()
        apply_fn(pool)
        assert pool.last_update is not None
        assert pool.last_update.sqrt_price_x96 == sqrt_price
        assert pool.last_update.liquidity == liquidity
        assert pool.last_update.tick == tick
        assert pool.last_update.block_number == 200


class TestV3MintDecoder:
    def test_decode_returns_callable(self) -> None:
        data = abi_encode(
            ["int24", "int24", "uint128", "uint256", "uint256"],
            [-100, 100, 500, 1000, 2000],
        )
        log = _make_log(V3_MINT_TOPIC, data)
        result = decode_v3_mint(log)
        assert callable(result)

    def test_decode_extracts_mint_liquidity(self) -> None:
        tick_lower = -200
        tick_upper = 200
        amount = 1000
        data = abi_encode(
            ["int24", "int24", "uint128", "uint256", "uint256"],
            [tick_lower, tick_upper, amount, 100, 200],
        )
        log = _make_log(V3_MINT_TOPIC, data, block_number=300)

        apply_fn = decode_v3_mint(log)
        from tests.fakes.pools import FakeV3Pool  # noqa: PLC0415

        pool = FakeV3Pool()
        apply_fn(pool)
        assert pool.last_liquidity_update is not None
        assert pool.last_liquidity_update.liquidity == amount  # Positive for mint
        assert pool.last_liquidity_update.tick_lower == tick_lower
        assert pool.last_liquidity_update.tick_upper == tick_upper


class TestV3BurnDecoder:
    def test_decode_returns_callable(self) -> None:
        data = abi_encode(
            ["int24", "int24", "uint128", "uint256", "uint256"],
            [-100, 100, 500, 1000, 2000],
        )
        log = _make_log(V3_BURN_TOPIC, data)
        result = decode_v3_burn(log)
        assert callable(result)

    def test_decode_extracts_burn_liquidity_negative(self) -> None:
        tick_lower = -200
        tick_upper = 200
        amount = 1000
        data = abi_encode(
            ["int24", "int24", "uint128", "uint256", "uint256"],
            [tick_lower, tick_upper, amount, 100, 200],
        )
        log = _make_log(V3_BURN_TOPIC, data, block_number=400)

        apply_fn = decode_v3_burn(log)
        from tests.fakes.pools import FakeV3Pool  # noqa: PLC0415

        pool = FakeV3Pool()
        apply_fn(pool)
        assert pool.last_liquidity_update is not None
        assert pool.last_liquidity_update.liquidity == -amount  # Negative for burn
        assert pool.last_liquidity_update.tick_lower == tick_lower
        assert pool.last_liquidity_update.tick_upper == tick_upper


class TestV4SwapDecoder:
    def test_decode_returns_callable(self) -> None:
        data = abi_encode(
            ["int128", "int128", "uint160", "uint128", "int24", "uint24"],
            [-100, 200, 1 << 96, 1000, 10, 3000],
        )
        # V4 events have pool ID as topic[1]
        pool_id = b"\x01" * 32
        log = _make_log(V4_SWAP_TOPIC, data, extra_topics=[pool_id])
        result = decode_v4_swap(log)
        assert callable(result)


class TestV4ModifyLiquidityDecoder:
    def test_decode_returns_callable(self) -> None:
        data = abi_encode(
            ["int24", "int24", "int256", "bytes32"],
            [-100, 100, 500, b"\x00" * 32],
        )
        pool_id = b"\x01" * 32
        log = _make_log(V4_MODIFY_LIQUIDITY_TOPIC, data, extra_topics=[pool_id])
        result = decode_v4_modify_liquidity(log)
        assert callable(result)


class TestTopicConstants:
    """Verify topic constants match known keccak256 hashes."""

    def test_v2_sync_topic(self) -> None:
        from web3 import Web3  # noqa: PLC0415

        expected = Web3.keccak(text="Sync(uint112,uint112)").hex()
        assert f"0x{expected}" == V2_SYNC_TOPIC

    def test_v3_swap_topic(self) -> None:
        from web3 import Web3  # noqa: PLC0415

        expected = Web3.keccak(
            text="Swap(address,address,int256,int256,uint160,uint128,int24)"
        ).hex()
        assert f"0x{expected}" == V3_SWAP_TOPIC

    def test_v3_mint_topic(self) -> None:
        from web3 import Web3  # noqa: PLC0415

        expected = Web3.keccak(
            text="Mint(address,address,int24,int24,uint128,uint256,uint256)"
        ).hex()
        assert f"0x{expected}" == V3_MINT_TOPIC

    def test_v3_burn_topic(self) -> None:
        from web3 import Web3  # noqa: PLC0415

        expected = Web3.keccak(
            text="Burn(address,int24,int24,uint128,uint256,uint256)"
        ).hex()
        assert f"0x{expected}" == V3_BURN_TOPIC

    def test_v4_swap_topic(self) -> None:
        from web3 import Web3  # noqa: PLC0415

        expected = Web3.keccak(
            text="Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)"
        ).hex()
        assert f"0x{expected}" == V4_SWAP_TOPIC

    def test_v4_modify_liquidity_topic(self) -> None:
        from web3 import Web3  # noqa: PLC0415

        expected = Web3.keccak(
            text="ModifyLiquidity(bytes32,address,int24,int24,int256,bytes32)"
        ).hex()
        assert f"0x{expected}" == V4_MODIFY_LIQUIDITY_TOPIC
