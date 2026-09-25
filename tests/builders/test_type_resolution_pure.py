"""Tests for the pure probing descriptor logic extracted from type_resolution.py."""

import pytest

from degenbot.builders.type_resolution import _descriptor_from_probing_result
from degenbot.exceptions.base import DegenbotValueError
from degenbot.types.pool_type import PoolFamily, PoolProbe

CHAIN_ID = 1
UNKNOWN_FACTORY = "0x0000000000000000000000000000000000000001"


class TestDescriptorFromProbingResult:
    """_descriptor_from_probing_result maps 'which probe succeeded' to a descriptor."""

    def test_v3_succeeds_yields_concentrated_liquidity(self) -> None:
        """slot0() call succeeding → V3 probe → CONCENTRATED_LIQUIDITY."""
        result = _descriptor_from_probing_result(
            succeeded=PoolProbe.V3,
            chain_id=CHAIN_ID,
            factory=UNKNOWN_FACTORY,  # type: ignore[arg-type]
        )
        assert result.family == PoolFamily.CONCENTRATED_LIQUIDITY

    def test_v2_succeeds_yields_constant_product(self) -> None:
        """getReserves() call succeeding → V2 probe → CONSTANT_PRODUCT."""
        result = _descriptor_from_probing_result(
            succeeded=PoolProbe.V2,
            chain_id=CHAIN_ID,
            factory=UNKNOWN_FACTORY,  # type: ignore[arg-type]
        )
        assert result.family == PoolFamily.CONSTANT_PRODUCT

    def test_no_probe_succeeds_raises(self) -> None:
        """No probe answered → unverified, not STABLESWAP: raise."""
        with pytest.raises(DegenbotValueError, match="No pool probe succeeded"):
            _descriptor_from_probing_result(
                succeeded=None,
                chain_id=CHAIN_ID,
                factory=UNKNOWN_FACTORY,  # type: ignore[arg-type]
            )

    def test_registered_factory_prefers_registry_descriptor(self) -> None:
        """When the factory is registered, registry descriptor takes precedence."""
        # Uniswap V2 factory on chain 1 is registered at import time
        factory = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
        result = _descriptor_from_probing_result(
            succeeded=PoolProbe.V3,  # would default to CONCENTRATED_LIQUIDITY
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
            succeeded=PoolProbe.V2,
            chain_id=CHAIN_ID,
            factory=UNKNOWN_FACTORY,  # type: ignore[arg-type]
        )
        assert result.factory == UNKNOWN_FACTORY

    def test_none_probe_carries_no_descriptor(self) -> None:
        """``None`` is an unverified identity: it must never produce a descriptor."""
        with pytest.raises(DegenbotValueError, match="No pool probe succeeded"):
            _descriptor_from_probing_result(
                succeeded=None,
                chain_id=CHAIN_ID,
                factory=UNKNOWN_FACTORY,  # type: ignore[arg-type]
            )
