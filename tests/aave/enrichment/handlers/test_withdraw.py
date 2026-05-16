"""Tests for WITHDRAW operation handler."""

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any
from unittest.mock import MagicMock

import pytest
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.aave.enrichment.handlers.base import OperationHandler
from degenbot.aave.enrichment.handlers.withdraw import WithdrawHandler
from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.models import EnrichedScaledTokenEvent
from degenbot.aave.operation_types import OperationType
import eth_abi.abi
from degenbot.aave.enrichment.context import EnrichmentContext

if TYPE_CHECKING:
    from degenbot.cli.aave_transaction_operations import Operation, ScaledTokenEvent


class TestWithdrawHandler:
    """Tests for WithdrawHandler."""

    @pytest.fixture
    def handler(self) -> WithdrawHandler:
        return WithdrawHandler()

    def test_handler_supports_withdraw_operation(
        self, handler: WithdrawHandler
    ) -> None:
        """Handler supports WITHDRAW operation type."""
        assert OperationType.WITHDRAW in handler.operation_types

    def test_handler_is_operation_handler_protocol(
        self, handler: WithdrawHandler
    ) -> None:
        """Handler implements OperationHandler protocol."""
        assert isinstance(handler, OperationHandler)

    def test_standard_withdraw_calculates_scaled_amount(
        self, handler: WithdrawHandler
    ) -> None:
        """
        Standard WITHDRAW calculates scaled amount with ceil rounding.

        For standard withdrawals (burn events), use COLLATERAL_BURN calculation.
        """
        index = 2_000_000_000_000_000_000_000_000_000  # 2.0 * RAY
        raw_amount = 1_000_000_000_000_000_000  # 1.0 * WAD

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.COLLATERAL_BURN,
            amount=raw_amount,
            index=index,
        )

        pool_event = _create_mock_pool_event(raw_amount)
        operation = _create_mock_operation(OperationType.WITHDRAW, pool_event)
        context = _create_mock_context()

        result = handler.handle(scaled_event, operation, context)

        assert result.raw_amount == raw_amount
        assert result.scaled_amount == 500_000_000_000_000_000
        assert result.event_type == ScaledTokenEventType.COLLATERAL_BURN

    def test_interest_exceeds_withdrawal_uses_burn_calculation(
        self, handler: WithdrawHandler
    ) -> None:
        """
        When interest exceeds withdrawal, use COLLATERAL_BURN calculation.

        When a COLLATERAL_MINT event has amount < balance_increase, it means
        interest > withdrawal. The aToken contract emits a Mint event instead
        of a Burn event. We must:
        1. Extract the actual withdrawal amount from Pool event
        2. Use COLLATERAL_BURN calculation (ceil rounding)
        3. Set scaled_amount=None to skip validation

        See debug/aave/0031 for details.
        """
        index = 2_000_000_000_000_000_000_000_000_000
        interest_amount = 2000  # balance_increase (interest)
        net_amount = 500  # amount on Mint event (interest - withdrawal)
        withdraw_amount = 1500  # actual withdrawal from Pool event

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            amount=net_amount,
            index=index,
            balance_increase=interest_amount,
        )

        pool_event = _create_mock_pool_event(withdraw_amount)
        operation = _create_mock_operation(OperationType.WITHDRAW, pool_event)
        context = _create_mock_context_with_override()

        result = handler.handle(scaled_event, operation, context)

        # Should extract actual withdrawal amount
        assert result.raw_amount == withdraw_amount
        # Should set scaled_amount=None to skip validation (calculation type override)
        assert result.scaled_amount is None
        assert result.event_type == ScaledTokenEventType.COLLATERAL_MINT


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


def _create_mock_pool_event(amount: int) -> LogReceipt:
    """Create a mock Pool WITHDRAW event."""

    # WITHDRAW event: (uint256 amount)
    data = eth_abi.abi.encode(["uint256"], [amount])

    return LogReceipt({
        "address": "0x" + "p" * 40,
        "topics": [
            HexBytes(bytes.fromhex("3115d1449a7b732c986cba18244e897a450f61e1bb8d589cd2e69e6c8924f9f7")),
        ],
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
    """Create a mock EnrichmentContext for WITHDRAW."""

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

        (amount,) = eth_abi.abi.decode(["uint256"], pool_event["data"])
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
            "from_address": event.user_address,
            "target_address": None,
        }

        return EnrichedScaledTokenEvent(**kwargs)

    mock_context.get_token_revision = mock_get_token_revision
    mock_context.get_underlying_asset = mock_get_underlying_asset
    mock_context.extract_pool_amount = mock_extract_pool_amount
    mock_context.calculate = mock_calculate
    mock_context.build_enriched_event = mock_build_enriched_event
    return mock_context


def _create_mock_context_with_override() -> MagicMock:
    """Create a mock EnrichmentContext for interest>withdrawal case."""

    MagicMock()
    mock_context = MagicMock(spec=EnrichmentContext)
    mock_context.pool_revision = 1
    mock_context.token_revisions = {}
    mock_context._calculation_override = None

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

        (amount,) = eth_abi.abi.decode(["uint256"], pool_event["data"])
        return amount

    def mock_calculate(
        event_type: ScaledTokenEventType,
        raw_amount: int,
        index: int,
        token_revision: int,
    ) -> int:
        """Calculate scaled amount using TokenMath."""
        mock_context._calculation_override = event_type
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

        return EnrichedScaledTokenEvent(**kwargs)

    mock_context.get_token_revision = mock_get_token_revision
    mock_context.get_underlying_asset = mock_get_underlying_asset
    mock_context.extract_pool_amount = mock_extract_pool_amount
    mock_context.calculate = mock_calculate
    mock_context.build_enriched_event = mock_build_enriched_event
    return mock_context
