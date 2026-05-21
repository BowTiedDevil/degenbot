"""Tests for the pure functions extracted from type_resolution.py.

Plan 066, Slice 1: _build_descriptor_from_db_result and
_descriptor_from_probing_result are the first pure-logic extractions.
These tests verify behavior through the public interface of each function.
"""

from unittest.mock import MagicMock

import pytest

from degenbot.builders.type_resolution import (
    _build_descriptor_from_db_result,
    _descriptor_from_probing_result,
)
from degenbot.types.pool_type import PoolFamily


CHAIN_ID = 1
UNKNOWN_FACTORY = "0x0000000000000000000000000000000000000001"


class TestBuildDescriptorFromDbResult:
    """_build_descriptor_from_db_result maps a DB row to a PoolTypeDescriptor."""

    def test_known_kind_returns_descriptor(self) -> None:
        """A registered kind maps to a PoolTypeDescriptor with the right family."""
        mock_row = MagicMock()
        mock_row.kind = "uniswap_v2"
        mock_row.exchange.factory = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
        result = _build_descriptor_from_db_result(mock_row)
        assert result is not None
        assert result.family == PoolFamily.CONSTANT_PRODUCT
        assert result.kind == "uniswap_v2"

    def test_unknown_kind_returns_none(self) -> None:
        """An unregistered kind returns None — caller must continue resolution."""
        mock_row = MagicMock()
        mock_row.kind = "nonexistent_kind"
        mock_row.exchange.factory = "0xABCD"
        result = _build_descriptor_from_db_result(mock_row)
        assert result is None

    def test_returns_factory_from_exchange(self) -> None:
        """The descriptor carries the factory from the DB exchange row."""
        factory = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
        mock_row = MagicMock()
        mock_row.kind = "uniswap_v2"
        mock_row.exchange.factory = factory
        result = _build_descriptor_from_db_result(mock_row)
        assert result is not None
        assert result.factory == factory

    def test_concentrated_liquidity_kind(self) -> None:
        """A V3 kind maps to CONCENTRATED_LIQUIDITY."""
        mock_row = MagicMock()
        mock_row.kind = "uniswap_v3"
        mock_row.exchange.factory = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
        result = _build_descriptor_from_db_result(mock_row)
        assert result is not None
        assert result.family == PoolFamily.CONCENTRATED_LIQUIDITY


class TestDescriptorFromProbingResult:
    """_descriptor_from_probing_result maps 'which method succeeded' to a descriptor."""

    def test_slot0_succeeds_yields_concentrated_liquidity(self) -> None:
        """slot0() call succeeding → CONCENTRATED_LIQUIDITY."""
        result = _descriptor_from_probing_result(
            succeeded_method="slot0",
            chain_id=CHAIN_ID,
            factory=UNKNOWN_FACTORY,  # type: ignore[arg-type]
        )
        assert result.family == PoolFamily.CONCENTRATED_LIQUIDITY

    def test_getreserves_succeeds_yields_constant_product(self) -> None:
        """getReserves() call succeeding → CONSTANT_PRODUCT."""
        result = _descriptor_from_probing_result(
            succeeded_method="getReserves",
            chain_id=CHAIN_ID,
            factory=UNKNOWN_FACTORY,  # type: ignore[arg-type]
        )
        assert result.family == PoolFamily.CONSTANT_PRODUCT

    def test_no_method_succeeds_yields_stableswap(self) -> None:
        """All probing methods fail → STABLESWAP fallback."""
        result = _descriptor_from_probing_result(
            succeeded_method=None,
            chain_id=CHAIN_ID,
            factory=UNKNOWN_FACTORY,  # type: ignore[arg-type]
        )
        assert result.family == PoolFamily.STABLESWAP

    def test_registered_factory_prefers_registry_descriptor(self) -> None:
        """When the factory is registered, registry descriptor takes precedence."""
        # Uniswap V2 factory on chain 1 is registered at import time
        factory = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
        result = _descriptor_from_probing_result(
            succeeded_method="slot0",  # would default to CONCENTRATED_LIQUIDITY
            chain_id=CHAIN_ID,
            factory=factory,  # type: ignore[arg-type]
        )
        # Registry says CONSTANT_PRODUCT for this factory
        assert result.family == PoolFamily.CONSTANT_PRODUCT
        # Should have the registry's variant, not a default None
        assert result.kind == "uniswap_v2"

    def test_descriptor_carries_factory(self) -> None:
        """The descriptor always carries the factory address."""
        result = _descriptor_from_probing_result(
            succeeded_method="getReserves",
            chain_id=CHAIN_ID,
            factory=UNKNOWN_FACTORY,  # type: ignore[arg-type]
        )
        assert result.factory == UNKNOWN_FACTORY

    def test_stableswap_fallback_carries_factory(self) -> None:
        """The STABLESWAP fallback descriptor also carries the factory."""
        result = _descriptor_from_probing_result(
            succeeded_method=None,
            chain_id=CHAIN_ID,
            factory=UNKNOWN_FACTORY,  # type: ignore[arg-type]
        )
        assert result.factory == UNKNOWN_FACTORY
        assert result.family == PoolFamily.STABLESWAP
