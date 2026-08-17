"""Tests for the shared type resolution module.

Verifies the pure-logic ``pool_class_for_descriptor`` function and the
I/O-dependent ``resolve``/``fetch``/``probe`` functions in
``src/degenbot/builders/type_resolution.py``.

Post ADR-005 slice-14 collapse: the resolve/fetch/probe functions call
``io.fetch_X()`` / ``io.probe_pool_type()`` directly (the Python ``io.call()``
parity-gate fallback is retired). The fakes here are duck-typed objects
exposing those ``fetch_*`` methods — no ``RustBotIo`` subclass needed (Q4 alpha):
builders never ``isinstance(io, RustBotIo)``.
"""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

from degenbot.builders.type_resolution import (
    fetch_factory_from_chain,
    pool_class_for_descriptor,
    resolve_pool_type,
    resolve_pool_type_by_probing,
)
from degenbot.exceptions.base import DegenbotValueError
from degenbot.types.pool_type import PoolFamily, PoolTypeDescriptor
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

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


class FakePyBotIo:
    """Duck-typed RustBotIo stand-in for type-resolution tests.

    Each seam method returns a configurable canned value; theresolve/fetch/
    probe functions only ever call ``fetch_factory_address`` /
    ``probe_pool_type`` / ``fetch_pool_row`` / ``fetch_exchange``.
    """

    def __init__(
        self,
        *,
        factory_address: str | None = None,
        probe_result: str = "",
        pool_row: object | None = None,
        exchange_row: object | None = None,
    ) -> None:
        self._factory_address = factory_address
        self._probe_result = probe_result
        self._pool_row = pool_row
        self._exchange_row = exchange_row

    def get_block_number(self) -> int:
        return 18_000_000

    def fetch_factory_address(self, _address: str) -> str | None:
        return self._factory_address

    def probe_pool_type(self, _address: str) -> str:
        return self._probe_result

    def fetch_pool_row(self, chain_id: int, address: str) -> object | None:
        return self._pool_row

    def fetch_exchange(self, exchange_id: int) -> object | None:
        return self._exchange_row


class TestFetchFactoryFromChain:
    """Tests for the sync fetch_factory_from_chain."""

    def test_returns_decoded_factory_address(self) -> None:
        factory_address = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
        io = FakePyBotIo(factory_address=factory_address)
        result = fetch_factory_from_chain(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result is not None
        assert result.lower() == factory_address.lower()

    def test_returns_none_on_failure(self) -> None:
        io = FakePyBotIo(factory_address=None)
        result = fetch_factory_from_chain(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result is None


class TestResolvePoolTypeByProbing:
    """Tests for the sync resolve_pool_type_by_probing."""

    def test_v3_probe_returns_concentrated_liquidity(self) -> None:
        io = FakePyBotIo(probe_result="slot0")
        result = resolve_pool_type_by_probing(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            factory="0xFactory",  # type: ignore[arg-type]
            io=io,
        )
        assert result.family == PoolFamily.CONCENTRATED_LIQUIDITY

    def test_v2_probe_returns_constant_product(self) -> None:
        io = FakePyBotIo(probe_result="getReserves")
        result = resolve_pool_type_by_probing(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            factory="0xFactory",  # type: ignore[arg-type]
            io=io,
        )
        assert result.family == PoolFamily.CONSTANT_PRODUCT

    def test_fallback_returns_stableswap(self) -> None:
        # An unrecognized probe result falls through to the STABLESWAP default.
        io = FakePyBotIo(probe_result="")
        result = resolve_pool_type_by_probing(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            factory="0xFactory",  # type: ignore[arg-type]
            io=io,
        )
        assert result.family == PoolFamily.STABLESWAP


class TestResolvePoolType:
    """Tests for the full sync resolve_pool_type flow."""

    def test_raises_when_factory_fails_and_no_db_entry(self) -> None:
        io = FakePyBotIo(factory_address=None, pool_row=None)
        with pytest.raises(DegenbotValueError, match="Cannot resolve pool type"):
            resolve_pool_type(
                "0xPool",  # type: ignore[arg-type]
                chain_id=CHAIN_ID,
                io=io,
            )

    def test_probes_when_no_db_entry(self) -> None:
        factory_address = "0x1234567890AbCdEf1234567890aBcDeF12345678"
        io = FakePyBotIo(
            factory_address=factory_address,
            pool_row=None,
            probe_result="slot0",
        )
        result = resolve_pool_type(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result.family == PoolFamily.CONCENTRATED_LIQUIDITY

    def test_resolves_from_db_via_seam(self) -> None:
        """A DB hit returns through the seam without probing."""
        factory_address = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
        pool_row = MagicMock()
        pool_row.kind = "uniswap_v3"
        pool_row.exchange_id = 42
        exchange_row = MagicMock()
        exchange_row.factory = factory_address
        io = FakePyBotIo(
            factory_address=None,
            pool_row=pool_row,
            exchange_row=exchange_row,
        )

        result = resolve_pool_type(
            "0xPool",  # type: ignore[arg-type]
            chain_id=CHAIN_ID,
            io=io,
        )
        assert result.family == PoolFamily.CONCENTRATED_LIQUIDITY
        assert result.factory == factory_address
