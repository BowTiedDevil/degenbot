"""Tests for REPAY_WITH_ATOKENS operation handler."""

from dataclasses import dataclass
from typing import TYPE_CHECKING
from unittest.mock import MagicMock

import pytest
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.aave.enrichment.handlers.base import OperationHandler
from degenbot.aave.enrichment.handlers.repay_with_atokens import RepayWithAtokensHandler
from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.models import (
    EnrichedCollateralBurnEvent,
    EnrichedCollateralMintEvent,
    EnrichedDebtBurnEvent,
    EnrichedDebtMintEvent,
)
from degenbot.aave.operation_types import OperationType

if TYPE_CHECKING:
    from degenbot.cli.aave_transaction_operations import Operation, ScaledTokenEvent


class TestRepayWithAtokensHandler:
    """Tests for RepayWithAtokensHandler."""

    @pytest.fixture
    def handler(self) -> RepayWithAtokensHandler:
        return RepayWithAtokensHandler()

    def test_handler_supports_repay_with_atokens_operation(
        self, handler: RepayWithAtokensHandler
    ) -> None:
        """Handler supports REPAY_WITH_ATOKENS operation type."""
        assert OperationType.REPAY_WITH_ATOKENS in handler.operation_types

    def test_handler_is_operation_handler_protocol(
        self, handler: RepayWithAtokensHandler
    ) -> None:
        """Handler implements OperationHandler protocol."""
        assert isinstance(handler, OperationHandler)

    def test_standard_collateral_burn(
        self, handler: RepayWithAtokensHandler
    ) -> None:
        """Standard collateral burn for REPAY_WITH_ATOKENS."""
        index = 2_000_000_000_000_000_000_000_000_000
        raw_amount = 1_000_000_000_000_000_000

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.COLLATERAL_BURN,
            amount=raw_amount,
            index=index,
        )

        pool_event = _create_mock_pool_event(raw_amount)
        operation = _create_mock_operation(OperationType.REPAY_WITH_ATOKENS, pool_event)
        context = _create_mock_context_collateral()

        result = handler.handle(scaled_event, operation, context)

        assert result.raw_amount == raw_amount
        assert result.scaled_amount == 500_000_000_000_000_000
        assert isinstance(result, EnrichedCollateralBurnEvent)

    def test_standard_debt_burn(
        self, handler: RepayWithAtokensHandler
    ) -> None:
        """Standard debt burn for REPAY_WITH_ATOKENS."""
        index = 2_000_000_000_000_000_000_000_000_000
        raw_amount = 1_000_000_000_000_000_000

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.DEBT_BURN,
            amount=raw_amount,
            index=index,
        )

        pool_event = _create_mock_pool_event(raw_amount)
        operation = _create_mock_operation(OperationType.REPAY_WITH_ATOKENS, pool_event)
        context = _create_mock_context_debt()

        result = handler.handle(scaled_event, operation, context)

        assert result.raw_amount == raw_amount
        assert result.scaled_amount == 500_000_000_000_000_000
        assert isinstance(result, EnrichedDebtBurnEvent)

    def test_interest_exceeds_collateral_burn_uses_burn_calculation(
        self, handler: RepayWithAtokensHandler
    ) -> None:
        """
        When interest exceeds repayment for collateral, use COLLATERAL_BURN.

        Similar to WITHDRAW case - when COLLATERAL_MINT has amount < balance_increase,
        use COLLATERAL_BURN calculation (ceil rounding).
        scaled_amount=None is set to skip validation.
        """
        index = 2_000_000_000_000_000_000_000_000_000
        interest_amount = 2000
        net_amount = 500
        repay_amount = 1500

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            amount=net_amount,
            index=index,
            balance_increase=interest_amount,
        )

        pool_event = _create_mock_pool_event(repay_amount)
        operation = _create_mock_operation(OperationType.REPAY_WITH_ATOKENS, pool_event)
        context = _create_mock_context_collateral_mint()

        result = handler.handle(scaled_event, operation, context)

        assert result.raw_amount == repay_amount
        # Override skips validation with scaled_amount=None
        assert result.scaled_amount is None
        assert isinstance(result, EnrichedCollateralMintEvent)

    def test_interest_exceeds_debt_burn_uses_burn_calculation(
        self, handler: RepayWithAtokensHandler
    ) -> None:
        """
        When interest exceeds repayment for debt, use DEBT_BURN.

        Similar to REPAY case - when DEBT_MINT has balance_increase,
        use DEBT_BURN calculation (floor rounding).
        """
        index = 2_000_000_000_000_000_000_000_000_000
        interest_amount = 2000
        net_amount = 500
        repay_amount = 1500

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.DEBT_MINT,
            amount=net_amount,
            index=index,
            balance_increase=interest_amount,
        )

        pool_event = _create_mock_pool_event(repay_amount)
        operation = _create_mock_operation(OperationType.REPAY_WITH_ATOKENS, pool_event)
        context = _create_mock_context_debt_mint()

        result = handler.handle(scaled_event, operation, context)

        assert result.raw_amount == repay_amount
        assert result.scaled_amount == 750
        assert isinstance(result, EnrichedDebtMintEvent)


# Helper functions


def _create_mock_scaled_event(
    event_type: ScaledTokenEventType,
    amount: int,
    index: int,
    balance_increase: int | None = None,
    user_address: ChecksumAddress = ChecksumAddress("0x" + "1" * 40),
) -> "ScaledTokenEvent":
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
    import eth_abi.abi
    data = eth_abi.abi.encode(["uint256", "bool"], [amount, True])  # useATokens=True
    return LogReceipt({
        "address": "0x" + "p" * 40,
        "topics": [
            HexBytes(bytes.fromhex("a534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051")),
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
    @dataclass
    class MockOperation:
        operation_type: OperationType
        pool_event: LogReceipt | None = None

    return MockOperation(operation_type=operation_type, pool_event=pool_event)


def _create_mock_context_collateral() -> MagicMock:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent

    mock_session = MagicMock()
    mock_context = MagicMock(spec=EnrichmentContext)
    mock_context.pool_revision = 1
    mock_context.token_revisions = {}

    def mock_get_token_revision(token_address: ChecksumAddress) -> int:
        return 1

    def mock_get_underlying_asset(token_address: ChecksumAddress) -> ChecksumAddress:
        return ChecksumAddress("0x1111111111111111111111111111111111111111")

    def mock_extract_pool_amount(pool_event: LogReceipt, **kwargs) -> int:
        import eth_abi.abi
        (amount, _) = eth_abi.abi.decode(["uint256", "bool"], pool_event["data"])
        return amount

    def mock_calculate(event_type: ScaledTokenEventType, raw_amount: int, index: int, token_revision: int) -> int:
        RAY = 10**27
        return raw_amount * RAY // index

    def mock_build_enriched_event(event: "ScaledTokenEvent", operation: "Operation", raw_amount: int, scaled_amount: int | None) -> EnrichedScaledTokenEvent:
        event_type = event.event_type
        class_map = {ScaledTokenEventType.COLLATERAL_BURN: EnrichedCollateralBurnEvent}
        enriched_class = class_map.get(event_type)
        if enriched_class is None:
            raise ValueError(f"Unsupported: {event_type}")
        kwargs = {
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
        return enriched_class(**kwargs)

    mock_context.get_token_revision = mock_get_token_revision
    mock_context.get_underlying_asset = mock_get_underlying_asset
    mock_context.extract_pool_amount = mock_extract_pool_amount
    mock_context.calculate = mock_calculate
    mock_context.build_enriched_event = mock_build_enriched_event
    return mock_context


def _create_mock_context_collateral_mint() -> MagicMock:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent

    mock_session = MagicMock()
    mock_context = MagicMock(spec=EnrichmentContext)
    mock_context.pool_revision = 1
    mock_context.token_revisions = {}

    def mock_get_token_revision(token_address: ChecksumAddress) -> int:
        return 1

    def mock_get_underlying_asset(token_address: ChecksumAddress) -> ChecksumAddress:
        return ChecksumAddress("0x1111111111111111111111111111111111111111")

    def mock_extract_pool_amount(pool_event: LogReceipt, **kwargs) -> int:
        import eth_abi.abi
        (amount, _) = eth_abi.abi.decode(["uint256", "bool"], pool_event["data"])
        return amount

    def mock_calculate(event_type: ScaledTokenEventType, raw_amount: int, index: int, token_revision: int) -> int:
        RAY = 10**27
        return raw_amount * RAY // index

    def mock_build_enriched_event(event: "ScaledTokenEvent", operation: "Operation", raw_amount: int, scaled_amount: int | None) -> EnrichedScaledTokenEvent:
        event_type = event.event_type
        class_map = {ScaledTokenEventType.COLLATERAL_MINT: EnrichedCollateralMintEvent}
        enriched_class = class_map.get(event_type)
        if enriched_class is None:
            raise ValueError(f"Unsupported: {event_type}")
        kwargs = {
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
        return enriched_class(**kwargs)

    mock_context.get_token_revision = mock_get_token_revision
    mock_context.get_underlying_asset = mock_get_underlying_asset
    mock_context.extract_pool_amount = mock_extract_pool_amount
    mock_context.calculate = mock_calculate
    mock_context.build_enriched_event = mock_build_enriched_event
    return mock_context


def _create_mock_context_debt() -> MagicMock:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent

    mock_session = MagicMock()
    mock_context = MagicMock(spec=EnrichmentContext)
    mock_context.pool_revision = 1
    mock_context.token_revisions = {}

    def mock_get_token_revision(token_address: ChecksumAddress) -> int:
        return 1

    def mock_get_underlying_asset(token_address: ChecksumAddress) -> ChecksumAddress:
        return ChecksumAddress("0x1111111111111111111111111111111111111111")

    def mock_extract_pool_amount(pool_event: LogReceipt, **kwargs) -> int:
        import eth_abi.abi
        (amount, _) = eth_abi.abi.decode(["uint256", "bool"], pool_event["data"])
        return amount

    def mock_calculate(event_type: ScaledTokenEventType, raw_amount: int, index: int, token_revision: int) -> int:
        RAY = 10**27
        return raw_amount * RAY // index

    def mock_build_enriched_event(event: "ScaledTokenEvent", operation: "Operation", raw_amount: int, scaled_amount: int | None) -> EnrichedScaledTokenEvent:
        event_type = event.event_type
        class_map = {ScaledTokenEventType.DEBT_BURN: EnrichedDebtBurnEvent}
        enriched_class = class_map.get(event_type)
        if enriched_class is None:
            raise ValueError(f"Unsupported: {event_type}")
        kwargs = {
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
        return enriched_class(**kwargs)

    mock_context.get_token_revision = mock_get_token_revision
    mock_context.get_underlying_asset = mock_get_underlying_asset
    mock_context.extract_pool_amount = mock_extract_pool_amount
    mock_context.calculate = mock_calculate
    mock_context.build_enriched_event = mock_build_enriched_event
    return mock_context


def _create_mock_context_debt_mint() -> MagicMock:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent

    mock_session = MagicMock()
    mock_context = MagicMock(spec=EnrichmentContext)
    mock_context.pool_revision = 1
    mock_context.token_revisions = {}

    def mock_get_token_revision(token_address: ChecksumAddress) -> int:
        return 1

    def mock_get_underlying_asset(token_address: ChecksumAddress) -> ChecksumAddress:
        return ChecksumAddress("0x1111111111111111111111111111111111111111")

    def mock_extract_pool_amount(pool_event: LogReceipt, **kwargs) -> int:
        import eth_abi.abi
        (amount, _) = eth_abi.abi.decode(["uint256", "bool"], pool_event["data"])
        return amount

    def mock_calculate(event_type: ScaledTokenEventType, raw_amount: int, index: int, token_revision: int) -> int:
        RAY = 10**27
        return raw_amount * RAY // index

    def mock_build_enriched_event(event: "ScaledTokenEvent", operation: "Operation", raw_amount: int, scaled_amount: int | None) -> EnrichedScaledTokenEvent:
        event_type = event.event_type
        class_map = {ScaledTokenEventType.DEBT_MINT: EnrichedDebtMintEvent}
        enriched_class = class_map.get(event_type)
        if enriched_class is None:
            raise ValueError(f"Unsupported: {event_type}")
        kwargs = {
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
        return enriched_class(**kwargs)

    mock_context.get_token_revision = mock_get_token_revision
    mock_context.get_underlying_asset = mock_get_underlying_asset
    mock_context.extract_pool_amount = mock_extract_pool_amount
    mock_context.calculate = mock_calculate
    mock_context.build_enriched_event = mock_build_enriched_event
    return mock_context
