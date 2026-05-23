"""Tests for the shared type resolution module.

These verify the functions extracted from Bot/AsyncBot into
src/degenbot/builders/type_resolution.py — a pure-logic function
(pool_class_for_descriptor) that both sync and async paths share,
and the I/O-dependent resolve/fetch/probe functions that accept
PoolIO or AsyncPoolIO.
"""

from __future__ import annotations

from typing import TYPE_CHECKING
from unittest.mock import MagicMock

import eth_abi.abi
import pytest
from hexbytes import HexBytes
from web3.exceptions import Web3Exception

from degenbot.builders.pool_io import AsyncPoolIO, SyncPoolIO
from degenbot.builders.type_resolution import (
    fetch_factory_from_chain,
    fetch_factory_from_chain_async,
    pool_class_for_descriptor,
    resolve_pool_type,
    resolve_pool_type_async,
    resolve_pool_type_by_probing,
    resolve_pool_type_by_probing_async,
)
from degenbot.exceptions.base import DegenbotValueError
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.types.pool_type import PoolFamily, PoolTypeDescriptor
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from web3.types import TxParams


CHAIN_ID = 1


class TestPoolClassForDescriptor:
    """Tests for the pure pool_class_for_descriptor function."""

    def test_constant_product_returns_v2_class(self) -> None:
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.CONSTANT_PRODUCT,
            variant=None,
            kind="uniswap_v2",
            factory=None,
        )
        result = pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)
        assert result is UniswapV2Pool

    def test_concentrated_liquidity_returns_v3_class(self) -> None:
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.CONCENTRATED_LIQUIDITY,
            variant=None,
            kind="uniswap_v3",
            factory=None,
        )
        result = pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)
        assert result is UniswapV3Pool

    def test_unknown_family_raises(self) -> None:
        # PoolFamily.WEIGHTED has no default class registered
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.WEIGHTED,
            variant=None,
            kind="weighted",
            factory=None,
        )
        with pytest.raises(DegenbotValueError, match="No pool class for WEIGHTED"):
            pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)

    def test_registered_factory_returns_registered_class(self) -> None:
        """When the descriptor has a registered factory, returns the registered class."""
        factory = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
        descriptor = PoolTypeDescriptor(
            family=PoolFamily.CONSTANT_PRODUCT,
            variant=None,
            kind="uniswap_v2",
            factory=factory,  # type: ignore[arg-type]
        )
        result = pool_class_for_descriptor(descriptor, chain_id=CHAIN_ID)
        assert issubclass(result, UniswapV2Pool)


class FakeSyncProvider:
    """Minimal fake provider for SyncPoolIO delegation."""

    def __init__(self) -> None:
        self._responses: dict[str, HexBytes] = {}
        self.call_count = 0

    def set_response(self, to: str, data: str, response: HexBytes) -> None:
        self._responses[to, data] = response

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.call_count += 1
        key = (to, data.hex())
        if key in self._responses:
            return self._responses[key]
        msg = "No mock response"
        raise Web3Exception(msg)

    def call_raw(self, tx: TxParams, block: object = None) -> HexBytes:
        return HexBytes(b"\x00" * 32)

    def get_block_number(self) -> int:
        return 18_000_000

    def get_block(self, block_identifier: object = None) -> object:
        return {"number": 18_000_000}


class FakeAsyncProvider:
    """Minimal fake async provider for AsyncPoolIO delegation."""

    def __init__(self) -> None:
        self._responses: dict[str, HexBytes] = {}
        self.call_count = 0

    def set_response(self, to: str, data: str, response: HexBytes) -> None:
        self._responses[to, data] = response

    async def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.call_count += 1
        key = (to, data.hex())
        if key in self._responses:
            return self._responses[key]
        msg = "No mock response"
        raise Web3Exception(msg)

    async def call_raw(self, tx: TxParams, block: object = None) -> HexBytes:
        return HexBytes(b"\x00" * 32)

    async def get_block_number(self) -> int:
        return 18_000_000

    async def get_block(self, block_identifier: object = None) -> object:
        return {"number": 18_000_000}


class TestFetchFactoryFromChain:
    """Tests for the sync fetch_factory_from_chain."""

    def test_returns_decoded_factory_address(self) -> None:
        provider = FakeSyncProvider()
        factory_address = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"

        factory_data = encode_function_calldata("factory()", None)
        provider.set_response(
            to="0xPool",
            data=factory_data.hex(),
            response=HexBytes(
                eth_abi.abi.encode(types=["address"], args=[factory_address])
            ),
        )
        io = SyncPoolIO(provider)
        result = fetch_factory_from_chain(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result is not None
        assert result.lower() == factory_address.lower()

    def test_returns_none_on_failure(self) -> None:
        provider = FakeSyncProvider()
        io = SyncPoolIO(provider)
        result = fetch_factory_from_chain(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result is None


class TestFetchFactoryFromChainAsync:
    """Tests for the async fetch_factory_from_chain_async."""

    async def test_returns_decoded_factory_address(self) -> None:
        provider = FakeAsyncProvider()
        factory_address = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"

        factory_data = encode_function_calldata("factory()", None)
        provider.set_response(
            to="0xPool",
            data=factory_data.hex(),
            response=HexBytes(
                eth_abi.abi.encode(types=["address"], args=[factory_address])
            ),
        )
        io = AsyncPoolIO(provider)
        result = await fetch_factory_from_chain_async(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result is not None
        assert result.lower() == factory_address.lower()

    async def test_returns_none_on_failure(self) -> None:
        provider = FakeAsyncProvider()
        io = AsyncPoolIO(provider)
        result = await fetch_factory_from_chain_async(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result is None


class TestResolvePoolTypeByProbing:
    """Tests for the sync resolve_pool_type_by_probing."""

    def test_v3_probe_returns_concentrated_liquidity(self) -> None:
        provider = FakeSyncProvider()
        slot0_data = encode_function_calldata("slot0()", None)
        provider.set_response(
            to="0xPool",
            data=slot0_data.hex(),
            response=HexBytes(b"\x00" * 32),
        )
        io = SyncPoolIO(provider)
        result = resolve_pool_type_by_probing(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            factory="0xFactory",  # type: ignore[arg-type]
            io=io,
        )
        assert result.family == PoolFamily.CONCENTRATED_LIQUIDITY

    def test_v2_probe_returns_constant_product(self) -> None:
        provider = FakeSyncProvider()
        reserves_data = encode_function_calldata("getReserves()", None)
        provider.set_response(
            to="0xPool",
            data=reserves_data.hex(),
            response=HexBytes(b"\x00" * 32),
        )
        io = SyncPoolIO(provider)
        result = resolve_pool_type_by_probing(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            factory="0xFactory",  # type: ignore[arg-type]
            io=io,
        )
        assert result.family == PoolFamily.CONSTANT_PRODUCT

    def test_fallback_returns_stableswap(self) -> None:
        provider = FakeSyncProvider()
        io = SyncPoolIO(provider)
        result = resolve_pool_type_by_probing(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            factory="0xFactory",  # type: ignore[arg-type]
            io=io,
        )
        assert result.family == PoolFamily.STABLESWAP


class TestResolvePoolTypeByProbingAsync:
    """Tests for the async resolve_pool_type_by_probing_async."""

    async def test_v3_probe_returns_concentrated_liquidity(self) -> None:
        provider = FakeAsyncProvider()
        slot0_data = encode_function_calldata("slot0()", None)
        provider.set_response(
            to="0xPool",
            data=slot0_data.hex(),
            response=HexBytes(b"\x00" * 32),
        )
        io = AsyncPoolIO(provider)
        result = await resolve_pool_type_by_probing_async(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            factory="0xFactory",  # type: ignore[arg-type]
            io=io,
        )
        assert result.family == PoolFamily.CONCENTRATED_LIQUIDITY

    async def test_v2_probe_returns_constant_product(self) -> None:
        provider = FakeAsyncProvider()
        reserves_data = encode_function_calldata("getReserves()", None)
        provider.set_response(
            to="0xPool",
            data=reserves_data.hex(),
            response=HexBytes(b"\x00" * 32),
        )
        io = AsyncPoolIO(provider)
        result = await resolve_pool_type_by_probing_async(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            factory="0xFactory",  # type: ignore[arg-type]
            io=io,
        )
        assert result.family == PoolFamily.CONSTANT_PRODUCT

    async def test_fallback_returns_stableswap(self) -> None:
        provider = FakeAsyncProvider()
        io = AsyncPoolIO(provider)
        result = await resolve_pool_type_by_probing_async(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            factory="0xFactory",  # type: ignore[arg-type]
            io=io,
        )
        assert result.family == PoolFamily.STABLESWAP


class TestResolvePoolType:
    """Tests for the full sync resolve_pool_type flow."""

    def test_raises_when_factory_fails_and_no_db_entry(self) -> None:
        provider = FakeSyncProvider()
        io = SyncPoolIO(provider)
        fake_db = MagicMock()
        session = MagicMock()
        session.scalar.return_value = None
        fake_db.return_value.__enter__ = MagicMock(return_value=session)
        fake_db.return_value.__exit__ = MagicMock(return_value=False)

        with pytest.raises(DegenbotValueError, match="Cannot resolve pool type"):
            resolve_pool_type(
                "0xPool",  # type: ignore[arg-type]
                chain_id=CHAIN_ID,
                io=io,
                db=fake_db,
            )

    def test_probes_when_no_db_entry(self) -> None:
        provider = FakeSyncProvider()
        factory_address = "0x1234567890AbCdEf1234567890aBcDeF12345678"

        factory_data = encode_function_calldata("factory()", None)
        provider.set_response(
            to="0xPool",
            data=factory_data.hex(),
            response=HexBytes(
                eth_abi.abi.encode(types=["address"], args=[factory_address])
            ),
        )
        slot0_data = encode_function_calldata("slot0()", None)
        provider.set_response(
            to="0xPool",
            data=slot0_data.hex(),
            response=HexBytes(b"\x00" * 32),
        )
        io = SyncPoolIO(provider)
        fake_db = MagicMock()
        session = MagicMock()
        session.scalar.return_value = None
        fake_db.return_value.__enter__ = MagicMock(return_value=session)
        fake_db.return_value.__exit__ = MagicMock(return_value=False)

        result = resolve_pool_type(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
            db=fake_db,
        )
        assert result.family == PoolFamily.CONCENTRATED_LIQUIDITY


class TestResolvePoolTypeAsync:
    """Tests for the full async resolve_pool_type_async flow."""

    async def test_raises_when_factory_fails_and_no_db_entry(self) -> None:
        provider = FakeAsyncProvider()
        io = AsyncPoolIO(provider)
        fake_db = MagicMock()
        session = MagicMock()
        session.scalar.return_value = None
        fake_db.return_value.__enter__ = MagicMock(return_value=session)
        fake_db.return_value.__exit__ = MagicMock(return_value=False)

        with pytest.raises(DegenbotValueError, match="Cannot resolve pool type"):
            await resolve_pool_type_async(
                "0xPool",  # type: ignore[arg-type]
                chain_id=CHAIN_ID,
                io=io,
                db=fake_db,
            )

    async def test_probes_when_no_db_entry(self) -> None:
        provider = FakeAsyncProvider()
        factory_address = "0x1234567890AbCdEf1234567890aBcDeF12345678"

        factory_data = encode_function_calldata("factory()", None)
        provider.set_response(
            to="0xPool",
            data=factory_data.hex(),
            response=HexBytes(
                eth_abi.abi.encode(types=["address"], args=[factory_address])
            ),
        )
        slot0_data = encode_function_calldata("slot0()", None)
        provider.set_response(
            to="0xPool",
            data=slot0_data.hex(),
            response=HexBytes(b"\x00" * 32),
        )
        io = AsyncPoolIO(provider)
        fake_db = MagicMock()
        session = MagicMock()
        session.scalar.return_value = None
        fake_db.return_value.__enter__ = MagicMock(return_value=session)
        fake_db.return_value.__exit__ = MagicMock(return_value=False)

        result = await resolve_pool_type_async(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
            db=fake_db,
        )
        assert result.family == PoolFamily.CONCENTRATED_LIQUIDITY
