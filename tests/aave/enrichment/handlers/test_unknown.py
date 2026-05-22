"""Tests for UNKNOWN operation handler."""

from dataclasses import dataclass
from typing import TYPE_CHECKING
from unittest.mock import MagicMock

import pytest
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.aave.enrichment.context import EnrichmentContext
from degenbot.aave.enrichment.handlers.base import OperationHandler
from degenbot.aave.enrichment.handlers.unknown import UnknownHandler
from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.models import EnrichmentError
from degenbot.aave.operation_types import OperationType

if TYPE_CHECKING:
    from degenbot.aave.operations import Operation, ScaledTokenEvent


class TestUnknownHandler:
    """Tests for UnknownHandler."""

    @pytest.fixture
    def handler(self) -> UnknownHandler:
        return UnknownHandler()

    def test_handler_supports_unknown_operation(self, handler: UnknownHandler) -> None:
        """Handler supports UNKNOWN operation type."""
        assert OperationType.UNKNOWN in handler.operation_types

    def test_handler_is_operation_handler_protocol(self, handler: UnknownHandler) -> None:
        """Handler implements OperationHandler protocol."""
        assert isinstance(handler, OperationHandler)

    def test_unknown_raises_enrichment_error(self, handler: UnknownHandler) -> None:
        """
        UNKNOWN operation raises EnrichmentError.

        When an operation cannot be classified, enrichment should fail
        rather than produce incorrect results.
        """
        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            amount=1000,
        )

        operation = _create_mock_operation(OperationType.UNKNOWN)
        mock_session = MagicMock()
        context = EnrichmentContext(
            pool_revision=1,
            token_revisions={},
            session=mock_session,
        )

        with pytest.raises(EnrichmentError, match="Cannot enrich UNKNOWN operation"):
            handler.handle(scaled_event, operation, context)


# Helper functions to create mock objects


def _create_mock_scaled_event(
    event_type: ScaledTokenEventType,
    amount: int,
    user_address: ChecksumAddress = ChecksumAddress("0x" + "1" * 40),
) -> "ScaledTokenEvent":
    """Create a minimal mock ScaledTokenEvent."""

    @dataclass
    class MockScaledEvent:
        event: LogReceipt
        event_type: ScaledTokenEventType
        amount: int
        index: int | None
        balance_increase: int | None
        user_address: ChecksumAddress
        caller_address: ChecksumAddress | None = None
        from_address: ChecksumAddress | None = None
        target_address: ChecksumAddress | None = None

    return MockScaledEvent(
        event=LogReceipt({
            "address": "0x" + "a" * 40,
            "topics": [HexBytes(b"\x00" * 32)],
            "data": b"",
            "blockNumber": 1,
            "transactionHash": HexBytes(b"\x00" * 32),
            "transactionIndex": 0,
            "blockHash": HexBytes(b"\x00" * 32),
            "logIndex": 0,
        }),
        event_type=event_type,
        amount=amount,
        index=1_000_000_000_000_000_0000,
        balance_increase=None,
        user_address=user_address,
    )


def _create_mock_operation(operation_type: OperationType) -> "Operation":
    """Create a minimal mock Operation."""

    @dataclass
    class MockOperation:
        operation_type: OperationType
        pool_event: LogReceipt | None = None

    return MockOperation(operation_type=operation_type, pool_event=None)
