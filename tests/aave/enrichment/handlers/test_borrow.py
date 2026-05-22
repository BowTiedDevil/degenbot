"""Tests for BORROW operation handler."""

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any
from unittest.mock import MagicMock

import eth_abi.abi
import pytest
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.aave.enrichment.context import EnrichmentContext
from degenbot.aave.enrichment.handlers.base import OperationHandler
from degenbot.aave.enrichment.handlers.borrow import BorrowHandler
from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.models import EnrichedScaledTokenEvent
from degenbot.aave.operation_types import OperationType

if TYPE_CHECKING:
    from degenbot.aave.operations import Operation, ScaledTokenEvent


class TestBorrowHandler:
    """Tests for BorrowHandler."""

    @pytest.fixture
    def handler(self) -> BorrowHandler:
        return BorrowHandler()

    def test_handler_supports_borrow_operations(self, handler: BorrowHandler) -> None:
        """Handler supports BORROW and GHO_BORROW operation types."""
        assert OperationType.BORROW in handler.operation_types
        assert OperationType.GHO_BORROW in handler.operation_types

    def test_handler_is_operation_handler_protocol(self, handler: BorrowHandler) -> None:
        """Handler implements OperationHandler protocol."""
        assert isinstance(handler, OperationHandler)

    def test_borrow_calculates_scaled_amount(self, handler: BorrowHandler) -> None:
        """
        BORROW calculates scaled amount from raw amount and index.

        For BORROW operations:
        - Extract raw amount from Pool event
        - Calculate scaled amount using debt mint (ceil) rounding
        """
        index = 2_000_000_000_000_000_000_000_000_000  # 2.0 * RAY
        raw_amount = 1_000_000_000_000_000_000  # 1.0 * WAD (1 token)

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.DEBT_MINT,
            amount=raw_amount,
            index=index,
        )

        pool_event = _create_mock_pool_event(
            event_topic="0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0",  # BORROW
            amount=raw_amount,
        )
        operation = _create_mock_operation(OperationType.BORROW, pool_event)
        context = _create_mock_context()

        result = handler.handle(scaled_event, operation, context)

        assert result.raw_amount == raw_amount
        # Scaled amount = raw_amount / index (ceil for debt mint)
        assert result.scaled_amount == 500_000_000_000_000_000
        assert result.event_type == ScaledTokenEventType.DEBT_MINT

    def test_gho_borrow_calculates_scaled_amount(self, handler: BorrowHandler) -> None:
        """GHO_BORROW calculates scaled amount for GHO debt."""
        index = 2_000_000_000_000_000_000_000_000_000
        raw_amount = 1_000_000_000_000_000_000

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.GHO_DEBT_MINT,
            amount=raw_amount,
            index=index,
        )

        pool_event = _create_mock_pool_event(
            event_topic="0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0",
            amount=raw_amount,
        )
        operation = _create_mock_operation(OperationType.GHO_BORROW, pool_event)
        context = _create_mock_context()

        result = handler.handle(scaled_event, operation, context)

        assert result.raw_amount == raw_amount
        assert result.scaled_amount == 500_000_000_000_000_000
        assert result.event_type == ScaledTokenEventType.GHO_DEBT_MINT


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


def _create_mock_pool_event(
    event_topic: str,
    amount: int,
) -> LogReceipt:
    """Create a mock Pool BORROW event."""
    # BORROW event: (address, uint256 amount, uint8, uint256)
    data = eth_abi.abi.encode(
        ["address", "uint256", "uint8", "uint256"],
        ["0x1111111111111111111111111111111111111111", amount, 2, 0],
    )

    return LogReceipt({
        "address": "0x" + "p" * 40,
        "topics": [HexBytes(bytes.fromhex(event_topic[2:]))],
        "data": data,
        "blockNumber": 1,
        "transactionHash": HexBytes(b"\x00" * 32),
        "transactionIndex": 0,
        "blockHash": HexBytes(b"\x00" * 32),
        "logIndex": 0,
    })


def _create_mock_operation(
    operation_type: OperationType,
    pool_event: LogReceipt | None = None,
) -> "Operation":
    """Create a minimal mock Operation."""

    @dataclass
    class MockOperation:
        operation_type: OperationType
        pool_event: LogReceipt | None = None

    return MockOperation(operation_type=operation_type, pool_event=pool_event)


def _create_mock_context() -> MagicMock:
    """Create a mock EnrichmentContext for BORROW."""
    MagicMock()
    mock_context = MagicMock(spec=EnrichmentContext)
    mock_context.pool_revision = 1
    mock_context.token_revisions = {}

    def mock_get_token_revision(token_address: ChecksumAddress) -> int:
        return 1

    def mock_get_underlying_asset(token_address: ChecksumAddress) -> ChecksumAddress:
        return ChecksumAddress("0x1111111111111111111111111111111111111111")

    def mock_extract_pool_amount(
        pool_event: LogReceipt,
        event_type: ScaledTokenEventType | None = None,
        operation_type: OperationType | None = None,
    ) -> int:
        """Extract amount from the mock pool event."""
        (_, amount, _, _) = eth_abi.abi.decode(
            ["address", "uint256", "uint8", "uint256"],
            pool_event["data"],
        )
        return amount

    def mock_calculate(
        event_type: ScaledTokenEventType,
        raw_amount: int,
        index: int,
        token_revision: int,
    ) -> int:
        """Calculate scaled amount using TokenMath."""
        RAY = 10**27
        return raw_amount * RAY // index

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
            "underlying_asset": ChecksumAddress("0x1111111111111111111111111111111111111111"),
            "index": event.index,
            "balance_increase": event.balance_increase,
            "caller_address": None,
        }

        if event_type == ScaledTokenEventType.GHO_DEBT_MINT:
            kwargs["discount_percent"] = 0
            kwargs["discount_scaled"] = 0

        return EnrichedScaledTokenEvent(**kwargs)

    mock_context.get_token_revision = mock_get_token_revision
    mock_context.get_underlying_asset = mock_get_underlying_asset
    mock_context.extract_pool_amount = mock_extract_pool_amount
    mock_context.calculate = mock_calculate
    mock_context.build_enriched_event = mock_build_enriched_event
    return mock_context
