"""Tests for BALANCE_TRANSFER operation handler."""

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any
from unittest.mock import MagicMock

import pytest
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.aave.enrichment.context import EnrichmentContext
from degenbot.aave.enrichment.handlers.base import OperationHandler
from degenbot.aave.enrichment.handlers.transfer import BalanceTransferHandler
from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.models import EnrichedScaledTokenEvent
from degenbot.aave.operation_types import OperationType

if TYPE_CHECKING:
    from degenbot.aave.operations import Operation, ScaledTokenEvent


class TestBalanceTransferHandler:
    """Tests for BalanceTransferHandler."""

    @pytest.fixture
    def handler(self) -> BalanceTransferHandler:
        return BalanceTransferHandler()

    def test_handler_supports_balance_transfer_operation(
        self, handler: BalanceTransferHandler
    ) -> None:
        """Handler supports BALANCE_TRANSFER operation type."""
        assert OperationType.BALANCE_TRANSFER in handler.operation_types

    def test_handler_is_operation_handler_protocol(self, handler: BalanceTransferHandler) -> None:
        """Handler implements OperationHandler protocol."""
        assert isinstance(handler, OperationHandler)

    def test_collateral_transfer_uses_amount_directly(
        self, handler: BalanceTransferHandler
    ) -> None:
        """
        Collateral transfers bypass index-based scaling.

        For transfers, raw_amount = scaled_amount (no index-based calculation).
        """
        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.COLLATERAL_TRANSFER,
            amount=1000,
            from_address=ChecksumAddress("0x" + "1" * 40),
            to_address=ChecksumAddress("0x" + "2" * 40),
        )

        operation = _create_mock_operation(OperationType.BALANCE_TRANSFER)
        context = _create_mock_context()

        result = handler.handle(scaled_event, operation, context)

        assert result.scaled_amount == 1000
        assert result.raw_amount == 1000
        assert result.event_type == ScaledTokenEventType.COLLATERAL_TRANSFER

    def test_debt_transfer_uses_amount_directly(self, handler: BalanceTransferHandler) -> None:
        """Debt transfers bypass index-based scaling."""
        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.DEBT_TRANSFER,
            amount=500,
            from_address=ChecksumAddress("0x" + "1" * 40),
            to_address=ChecksumAddress("0x" + "2" * 40),
        )

        operation = _create_mock_operation(OperationType.BALANCE_TRANSFER)
        context = _create_mock_context()

        result = handler.handle(scaled_event, operation, context)

        assert result.scaled_amount == 500
        assert result.raw_amount == 500
        assert result.event_type == ScaledTokenEventType.DEBT_TRANSFER

    def test_gho_debt_transfer_uses_amount_directly(self, handler: BalanceTransferHandler) -> None:
        """GHO debt transfers bypass index-based scaling."""
        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.GHO_DEBT_TRANSFER,
            amount=250,
            from_address=ChecksumAddress("0x" + "1" * 40),
            to_address=ChecksumAddress("0x" + "2" * 40),
        )

        operation = _create_mock_operation(OperationType.BALANCE_TRANSFER)
        context = _create_mock_context()

        result = handler.handle(scaled_event, operation, context)

        assert result.scaled_amount == 250
        assert result.raw_amount == 250
        assert result.event_type == ScaledTokenEventType.GHO_DEBT_TRANSFER

    def test_erc20_collateral_transfer_uses_amount_directly(
        self, handler: BalanceTransferHandler
    ) -> None:
        """ERC20 collateral transfers bypass index-based scaling."""
        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
            amount=750,
            from_address=ChecksumAddress("0x" + "1" * 40),
            to_address=ChecksumAddress("0x" + "2" * 40),
        )

        operation = _create_mock_operation(OperationType.BALANCE_TRANSFER)
        context = _create_mock_context()

        result = handler.handle(scaled_event, operation, context)

        assert result.scaled_amount == 750
        assert result.raw_amount == 750
        assert result.event_type == ScaledTokenEventType.COLLATERAL_TRANSFER


# Helper functions to create mock objects


def _create_mock_scaled_event(
    event_type: ScaledTokenEventType,
    amount: int,
    from_address: ChecksumAddress | None = None,
    to_address: ChecksumAddress | None = None,
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
        index=None,  # Transfers don't have index
        balance_increase=None,
        user_address=user_address,
        from_address=from_address,
        target_address=to_address,
    )


def _create_mock_operation(operation_type: OperationType) -> "Operation":
    """Create a minimal mock Operation."""

    @dataclass
    class MockOperation:
        operation_type: OperationType
        pool_event: LogReceipt | None = None

    return MockOperation(operation_type=operation_type, pool_event=None)


def _create_mock_context() -> MagicMock:
    """Create a mock EnrichmentContext for transfers."""
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

        # Map ERC20 transfer types to their base types
        event_type_map: dict[ScaledTokenEventType, ScaledTokenEventType] = {
            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER: (
                ScaledTokenEventType.COLLATERAL_TRANSFER
            ),
            ScaledTokenEventType.ERC20_DEBT_TRANSFER: ScaledTokenEventType.DEBT_TRANSFER,
        }
        actual_event_type = event_type_map.get(event_type, event_type)

        kwargs: dict[str, Any] = {
            "event": event.event,
            "event_type": actual_event_type,
            "user_address": event.user_address,
            "raw_amount": raw_amount,
            "scaled_amount": scaled_amount,
            "pool_revision": 1,
            "token_revision": 1,
            "token_address": ChecksumAddress(event.event["address"]),
            "underlying_asset": ChecksumAddress("0x" + "u" * 40),
            "from_address": event.from_address or event.user_address,
            "to_address": event.target_address or event.user_address,
        }

        if event_type == ScaledTokenEventType.GHO_DEBT_TRANSFER:
            kwargs["discount_scaled"] = 0

        return EnrichedScaledTokenEvent(**kwargs)

    mock_context.build_enriched_event = mock_build_enriched_event
    return mock_context
