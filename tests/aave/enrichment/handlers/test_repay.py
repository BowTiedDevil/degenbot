"""Tests for REPAY/GHO_REPAY operation handler."""

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
from degenbot.aave.enrichment.handlers.repay import RepayHandler
from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.models import EnrichedScaledTokenEvent
from degenbot.aave.operation_types import OperationType

if TYPE_CHECKING:
    from degenbot.cli.aave_transaction_operations import Operation, ScaledTokenEvent


class TestRepayHandler:
    """Tests for RepayHandler."""

    @pytest.fixture
    def handler(self) -> RepayHandler:
        return RepayHandler()

    def test_handler_supports_repay_operations(self, handler: RepayHandler) -> None:
        """Handler supports REPAY and GHO_REPAY operation types."""
        assert OperationType.REPAY in handler.operation_types
        assert OperationType.GHO_REPAY in handler.operation_types

    def test_handler_is_operation_handler_protocol(self, handler: RepayHandler) -> None:
        """Handler implements OperationHandler protocol."""
        assert isinstance(handler, OperationHandler)

    def test_standard_repay_calculates_scaled_amount(self, handler: RepayHandler) -> None:
        """
        Standard REPAY calculates scaled amount with floor rounding.

        For standard repayments (burn events), use DEBT_BURN calculation.
        """
        index = 2_000_000_000_000_000_000_000_000_000
        raw_amount = 1_000_000_000_000_000_000

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.DEBT_BURN,
            amount=raw_amount,
            index=index,
        )

        pool_event = _create_mock_pool_event(raw_amount)
        operation = _create_mock_operation(OperationType.REPAY, pool_event)
        context = _create_mock_context()

        result = handler.handle(scaled_event, operation, context)

        assert result.raw_amount == raw_amount
        assert result.scaled_amount == 500_000_000_000_000_000
        assert result.event_type == ScaledTokenEventType.DEBT_BURN

    def test_interest_exceeds_repayment_uses_burn_calculation(self, handler: RepayHandler) -> None:
        """
        When interest exceeds repayment, use DEBT_BURN calculation.

        When a DEBT_MINT event is emitted during REPAY, it means
        interest > repayment. Use DEBT_BURN calculation (floor rounding).

        NOTE: Do NOT set scaled_amount=None for REPAY with DEBT_MINT.
        The processing layer uses the enriched scaled_amount directly.
        See debug/aave/0037 for details.
        """
        index = 2_000_000_000_000_000_000_000_000_000
        interest_amount = 2000  # balance_increase (interest)
        net_amount = 500  # amount on Mint event (interest - repayment)
        repay_amount = 1500  # actual repayment from Pool event

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.DEBT_MINT,
            amount=net_amount,
            index=index,
            balance_increase=interest_amount,
        )

        pool_event = _create_mock_pool_event(repay_amount)
        operation = _create_mock_operation(OperationType.REPAY, pool_event)
        context = _create_mock_context_with_mint()

        result = handler.handle(scaled_event, operation, context)

        # Should extract actual repayment amount
        assert result.raw_amount == repay_amount
        # Should use DEBT_BURN calculation (floor)
        assert result.scaled_amount == 750
        assert result.event_type == ScaledTokenEventType.DEBT_MINT

    def test_gho_repay_standard_burn(self, handler: RepayHandler) -> None:
        """GHO_REPAY standard burn calculation."""
        index = 2_000_000_000_000_000_000_000_000_000
        raw_amount = 1_000_000_000_000_000_000

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.GHO_DEBT_BURN,
            amount=raw_amount,
            index=index,
        )

        pool_event = _create_mock_pool_event(raw_amount)
        operation = _create_mock_operation(OperationType.GHO_REPAY, pool_event)
        context = _create_mock_context_gho()

        result = handler.handle(scaled_event, operation, context)

        assert result.raw_amount == raw_amount
        assert result.scaled_amount == 500_000_000_000_000_000
        assert result.event_type == ScaledTokenEventType.GHO_DEBT_BURN

    def test_gho_repay_interest_exceeds_repayment(self, handler: RepayHandler) -> None:
        """GHO_REPAY with interest > repayment uses GHO_DEBT_BURN calculation."""
        index = 2_000_000_000_000_000_000_000_000_000
        interest_amount = 2000
        net_amount = 500
        repay_amount = 1500

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.GHO_DEBT_MINT,
            amount=net_amount,
            index=index,
            balance_increase=interest_amount,
        )

        pool_event = _create_mock_pool_event(repay_amount)
        operation = _create_mock_operation(OperationType.GHO_REPAY, pool_event)
        context = _create_mock_context_gho_mint()

        result = handler.handle(scaled_event, operation, context)

        assert result.raw_amount == repay_amount
        assert result.scaled_amount == 750
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


def _create_mock_pool_event(amount: int) -> LogReceipt:
    """Create a mock Pool REPAY event."""

    # REPAY event: (uint256 amount, bool useATokens)
    data = eth_abi.abi.encode(["uint256", "bool"], [amount, False])

    return LogReceipt({
        "address": "0x" + "p" * 40,
        "topics": [
            HexBytes(
                bytes.fromhex("a534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051")
            ),
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
    """Create a mock EnrichmentContext for REPAY burn."""

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
        (amount, _) = eth_abi.abi.decode(["uint256", "bool"], pool_event["data"])
        return amount

    def mock_calculate(
        event_type: ScaledTokenEventType,
        raw_amount: int,
        index: int,
        token_revision: int,
    ) -> int:
        RAY = 10**27
        return raw_amount * RAY // index

    def mock_build_enriched_event(
        event: "ScaledTokenEvent",
        operation: "Operation",
        raw_amount: int,
        scaled_amount: int | None,
    ) -> EnrichedScaledTokenEvent:
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


def _create_mock_context_with_mint() -> MagicMock:
    """Create mock context for REPAY with DEBT_MINT case."""

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
        (amount, _) = eth_abi.abi.decode(["uint256", "bool"], pool_event["data"])
        return amount

    def mock_calculate(
        event_type: ScaledTokenEventType,
        raw_amount: int,
        index: int,
        token_revision: int,
    ) -> int:
        RAY = 10**27
        return raw_amount * RAY // index

    def mock_build_enriched_event(
        event: "ScaledTokenEvent",
        operation: "Operation",
        raw_amount: int,
        scaled_amount: int | None,
    ) -> EnrichedScaledTokenEvent:
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


def _create_mock_context_gho() -> MagicMock:
    """Create mock context for GHO_REPAY burn."""

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
        (amount, _) = eth_abi.abi.decode(["uint256", "bool"], pool_event["data"])
        return amount

    def mock_calculate(
        event_type: ScaledTokenEventType,
        raw_amount: int,
        index: int,
        token_revision: int,
    ) -> int:
        RAY = 10**27
        return raw_amount * RAY // index

    def mock_build_enriched_event(
        event: "ScaledTokenEvent",
        operation: "Operation",
        raw_amount: int,
        scaled_amount: int | None,
    ) -> EnrichedScaledTokenEvent:
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
            "discount_percent": 0,
            "discount_scaled": 0,
        }
        return EnrichedScaledTokenEvent(**kwargs)

    mock_context.get_token_revision = mock_get_token_revision
    mock_context.get_underlying_asset = mock_get_underlying_asset
    mock_context.extract_pool_amount = mock_extract_pool_amount
    mock_context.calculate = mock_calculate
    mock_context.build_enriched_event = mock_build_enriched_event
    return mock_context


def _create_mock_context_gho_mint() -> MagicMock:
    """Create mock context for GHO_REPAY with GHO_DEBT_MINT case."""

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
        (amount, _) = eth_abi.abi.decode(["uint256", "bool"], pool_event["data"])
        return amount

    def mock_calculate(
        event_type: ScaledTokenEventType,
        raw_amount: int,
        index: int,
        token_revision: int,
    ) -> int:
        RAY = 10**27
        return raw_amount * RAY // index

    def mock_build_enriched_event(
        event: "ScaledTokenEvent",
        operation: "Operation",
        raw_amount: int,
        scaled_amount: int | None,
    ) -> EnrichedScaledTokenEvent:
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
            "discount_percent": 0,
            "discount_scaled": 0,
        }
        return EnrichedScaledTokenEvent(**kwargs)

    mock_context.get_token_revision = mock_get_token_revision
    mock_context.get_underlying_asset = mock_get_underlying_asset
    mock_context.extract_pool_amount = mock_extract_pool_amount
    mock_context.calculate = mock_calculate
    mock_context.build_enriched_event = mock_build_enriched_event
    return mock_context
