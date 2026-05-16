"""Tests for MINT_TO_TREASURY operation handler."""

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any
from unittest.mock import MagicMock

import pytest
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.aave.enrichment.handlers.base import OperationHandler
from degenbot.aave.enrichment.handlers.mint_to_treasury import MintToTreasuryHandler
from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.models import EnrichedScaledTokenEvent
from degenbot.aave.operation_types import OperationType
from degenbot.aave.enrichment.context import EnrichmentContext

if TYPE_CHECKING:
    from degenbot.cli.aave_transaction_operations import Operation, ScaledTokenEvent


class TestMintToTreasuryHandler:
    """Tests for MintToTreasuryHandler."""

    @pytest.fixture
    def handler(self) -> MintToTreasuryHandler:
        return MintToTreasuryHandler()

    def test_handler_supports_mint_to_treasury_operation(
        self, handler: MintToTreasuryHandler
    ) -> None:
        """Handler supports MINT_TO_TREASURY operation type."""
        assert OperationType.MINT_TO_TREASURY in handler.operation_types

    def test_handler_is_operation_handler_protocol(
        self, handler: MintToTreasuryHandler
    ) -> None:
        """Handler implements OperationHandler protocol."""
        assert isinstance(handler, OperationHandler)

    def test_mint_to_treasury_sets_scaled_amount_to_none(
        self, handler: MintToTreasuryHandler
    ) -> None:
        """
        MINT_TO_TREASURY sets scaled_amount=None.

        MINT_TO_TREASURY requires position data (current balance and last_index)
        to correctly calculate accruedToTreasury. The enrichment layer doesn't
        have access to position data, so leave scaled_amount as None.
        The correct calculation is performed in aave.py with position context.
        See debug/aave/0014 - MINT_TO_TREASURY AccruedToTreasury Calculation Error.md
        """
        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            amount=1000,
            index=1_000_000_000_000_000_0000,
            balance_increase=None,
        )

        operation = _create_mock_operation(OperationType.MINT_TO_TREASURY)
        context = _create_mock_context()

        result = handler.handle(scaled_event, operation, context)

        assert result.scaled_amount is None
        assert result.raw_amount == 1000


# Helper functions to create mock objects


def _create_mock_scaled_event(
    event_type: ScaledTokenEventType,
    amount: int,
    index: int,
    balance_increase: int | None = None,
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
        index=index,
        balance_increase=balance_increase,
        user_address=user_address,
    )


def _create_mock_operation(operation_type: OperationType) -> "Operation":
    """Create a minimal mock Operation."""

    @dataclass
    class MockOperation:
        operation_type: OperationType
        pool_event: LogReceipt | None = None

    return MockOperation(operation_type=operation_type, pool_event=None)


def _create_mock_context() -> MagicMock:
    """Create a mock EnrichmentContext for MINT_TO_TREASURY."""

    MagicMock()
    mock_context = MagicMock(spec=EnrichmentContext)
    mock_context.pool_revision = 1
    mock_context.token_revisions = {}

    def mock_build_enriched_event(
        event: "ScaledTokenEvent",
        operation: "Operation",
        raw_amount: int,
        scaled_amount: int | None,
    ) -> EnrichedScaledTokenEvent:
        """Build enriched event for testing."""
        event_type = event.event_type

        kwargs: dict[str, Any] = {
            "event": event.event,
            "event_type": event_type,
            "user_address": event.user_address,
            "raw_amount": raw_amount,
            "scaled_amount": scaled_amount,
            "pool_revision": 1,
            "token_revision": 1,
            "token_address": ChecksumAddress(event.event["address"]),
            "underlying_asset": ChecksumAddress("0x" + "u" * 40),
            "index": event.index,
            "balance_increase": event.balance_increase,
        }

        return EnrichedScaledTokenEvent(**kwargs)

    mock_context.build_enriched_event = mock_build_enriched_event
    return mock_context
