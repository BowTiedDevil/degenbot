"""Tests for LIQUIDATION/GHO_LIQUIDATION operation handler."""

from dataclasses import dataclass
from typing import TYPE_CHECKING
from unittest.mock import MagicMock

import pytest
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.aave.enrichment.handlers.base import OperationHandler
from degenbot.aave.enrichment.handlers.liquidation import LiquidationHandler
from degenbot.aave.events import ScaledTokenEventType
from degenbot.aave.models import (
    EnrichedCollateralBurnEvent,
    EnrichedDebtBurnEvent,
    EnrichedDebtMintEvent,
    EnrichedGhoDebtBurnEvent,
)
from degenbot.aave.operation_types import OperationType

if TYPE_CHECKING:
    from degenbot.cli.aave_transaction_operations import Operation, ScaledTokenEvent


class TestLiquidationHandler:
    """Tests for LiquidationHandler."""

    @pytest.fixture
    def handler(self) -> LiquidationHandler:
        return LiquidationHandler()

    def test_handler_supports_liquidation_operations(
        self, handler: LiquidationHandler
    ) -> None:
        """Handler supports LIQUIDATION and GHO_LIQUIDATION operation types."""
        assert OperationType.LIQUIDATION in handler.operation_types
        assert OperationType.GHO_LIQUIDATION in handler.operation_types

    def test_handler_is_operation_handler_protocol(
        self, handler: LiquidationHandler
    ) -> None:
        """Handler implements OperationHandler protocol."""
        assert isinstance(handler, OperationHandler)

    def test_debt_burn_uses_debt_extractor(
        self, handler: LiquidationHandler
    ) -> None:
        """
        LIQUIDATION debt events use debtToCover from LiquidationCall event.
        """
        index = 2_000_000_000_000_000_000_000_000_000
        debt_to_cover = 1_000_000_000_000_000_000
        collateral_liquidated = 1_100_000_000_000_000_000  # Slightly more due to liquidation bonus

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.DEBT_BURN,
            amount=debt_to_cover,
            index=index,
        )

        pool_event = _create_mock_liquidation_pool_event(debt_to_cover, collateral_liquidated)
        operation = _create_mock_operation(OperationType.LIQUIDATION, pool_event)
        context = _create_mock_context_debt()

        result = handler.handle(scaled_event, operation, context)

        # Should extract debtToCover
        assert result.raw_amount == debt_to_cover
        assert result.scaled_amount == 500_000_000_000_000_000
        assert isinstance(result, EnrichedDebtBurnEvent)

    def test_collateral_burn_uses_collateral_extractor(
        self, handler: LiquidationHandler
    ) -> None:
        """
        LIQUIDATION collateral events use liquidatedCollateralAmount.
        """
        index = 2_000_000_000_000_000_000_000_000_000
        debt_to_cover = 1_000_000_000_000_000_000
        collateral_liquidated = 1_100_000_000_000_000_000

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.COLLATERAL_BURN,
            amount=collateral_liquidated,
            index=index,
        )

        pool_event = _create_mock_liquidation_pool_event(debt_to_cover, collateral_liquidated)
        operation = _create_mock_operation(OperationType.LIQUIDATION, pool_event)
        context = _create_mock_context_collateral()

        result = handler.handle(scaled_event, operation, context)

        # Should extract liquidatedCollateralAmount
        assert result.raw_amount == collateral_liquidated
        assert result.scaled_amount == 550_000_000_000_000_000
        assert isinstance(result, EnrichedCollateralBurnEvent)

    def test_pool_rev9_plus_pre_scales_debt(
        self, handler: LiquidationHandler
    ) -> None:
        """
        Pool Revision 9+ passes pre-scaled amounts for debt burns.

        The Pool calculates scaledAmount = debtToCover.rayDivFloor(index)
        and passes it to vToken.burn(). We must calculate this ourselves.
        See debug/aave/0044 for details.
        """
        index = 2_000_000_000_000_000_000_000_000_000
        debt_to_cover = 1_000_000_000_000_000_000

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.DEBT_BURN,
            amount=debt_to_cover,
            index=index,
        )

        pool_event = _create_mock_liquidation_pool_event(debt_to_cover, 1_100_000_000_000_000_000)
        operation = _create_mock_operation(OperationType.LIQUIDATION, pool_event)
        context = _create_mock_context_debt_rev9()

        result = handler.handle(scaled_event, operation, context)

        assert result.raw_amount == debt_to_cover
        # Pre-scaled: should calculate debtToCover / index
        assert result.scaled_amount == 500_000_000_000_000_000

    def test_net_debt_increase_uses_burn_calculation(
        self, handler: LiquidationHandler
    ) -> None:
        """
        When debt repayment < accrued interest, DEBT_MINT is a net increase.

        When balance_increase > amount on DEBT_MINT during liquidation,
        it's a net debt increase. Use DEBT_BURN calculation with
        raw_amount = balance_increase - amount.
        """
        index = 2_000_000_000_000_000_000_000_000_000
        debt_to_cover = 500
        interest = 2000
        net_increase = 1500  # balance_increase - amount

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.DEBT_MINT,
            amount=500,  # net_increase - debt_to_cover
            index=index,
            balance_increase=interest,
        )

        pool_event = _create_mock_liquidation_pool_event(debt_to_cover, 600)
        operation = _create_mock_operation(OperationType.LIQUIDATION, pool_event)
        context = _create_mock_context_debt_mint()

        result = handler.handle(scaled_event, operation, context)

        # Should use balance_increase - amount as raw_amount
        assert result.raw_amount == net_increase
        assert result.scaled_amount == 750
        assert isinstance(result, EnrichedDebtMintEvent)

    def test_gho_liquidation_debt_burn(
        self, handler: LiquidationHandler
    ) -> None:
        """GHO_LIQUIDATION uses GHO_DEBT_BURN."""
        index = 2_000_000_000_000_000_000_000_000_000
        debt_to_cover = 1_000_000_000_000_000_000

        scaled_event = _create_mock_scaled_event(
            event_type=ScaledTokenEventType.GHO_DEBT_BURN,
            amount=debt_to_cover,
            index=index,
        )

        pool_event = _create_mock_liquidation_pool_event(debt_to_cover, 1_100_000_000_000_000_000)
        operation = _create_mock_operation(OperationType.GHO_LIQUIDATION, pool_event)
        context = _create_mock_context_gho_debt()

        result = handler.handle(scaled_event, operation, context)

        assert result.raw_amount == debt_to_cover
        assert result.scaled_amount == 500_000_000_000_000_000
        assert isinstance(result, EnrichedGhoDebtBurnEvent)


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


def _create_mock_liquidation_pool_event(debt_to_cover: int, collateral_liquidated: int) -> LogReceipt:
    """Create a mock LiquidationCall Pool event."""
    import eth_abi.abi

    # LiquidationCall event:
    # (debtToCover, liquidatedCollateralAmount, liquidator, receiveAToken)
    data = eth_abi.abi.encode(
        ["uint256", "uint256", "address", "bool"],
        [debt_to_cover, collateral_liquidated, "0x1111111111111111111111111111111111111111", True],
    )

    # LIQUIDATION_CALL topic
    topic = HexBytes(bytes.fromhex("e413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286"))

    return LogReceipt({
        "address": "0x" + "p" * 40,
        "topics": [topic],
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


def _create_mock_context_debt() -> MagicMock:
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent

    mock_session = MagicMock()
    mock_context = MagicMock(spec=EnrichmentContext)
    mock_context.pool_revision = 1  # Pre-rev9
    mock_context.token_revisions = {}

    def mock_get_token_revision(token_address: ChecksumAddress) -> int:
        return 1

    def mock_get_underlying_asset(token_address: ChecksumAddress) -> ChecksumAddress:
        return ChecksumAddress("0x1111111111111111111111111111111111111111")

    def mock_extract_pool_amount(pool_event: LogReceipt, event_type: ScaledTokenEventType | None = None, operation_type: OperationType | None = None) -> int:
        import eth_abi.abi
        # Debt events extract debtToCover
        if event_type in {ScaledTokenEventType.DEBT_BURN, ScaledTokenEventType.GHO_DEBT_BURN}:
            (debt_to_cover, _, _, _) = eth_abi.abi.decode(["uint256", "uint256", "address", "bool"], pool_event["data"])
            return debt_to_cover
        # Collateral events extract liquidatedCollateralAmount
        if event_type == ScaledTokenEventType.COLLATERAL_BURN:
            (_, collateral, _, _) = eth_abi.abi.decode(["uint256", "uint256", "address", "bool"], pool_event["data"])
            return collateral
        raise ValueError(f"Unexpected event type: {event_type}")

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


def _create_mock_context_debt_rev9() -> MagicMock:
    """Context for Pool rev 9+ which pre-scales debt amounts."""
    from degenbot.aave.enrichment.context import EnrichmentContext
    from degenbot.aave.models import EnrichedScaledTokenEvent

    mock_session = MagicMock()
    mock_context = MagicMock(spec=EnrichmentContext)
    mock_context.pool_revision = 9  # Rev 9+
    mock_context.token_revisions = {}

    def mock_get_token_revision(token_address: ChecksumAddress) -> int:
        return 1

    def mock_get_underlying_asset(token_address: ChecksumAddress) -> ChecksumAddress:
        return ChecksumAddress("0x1111111111111111111111111111111111111111")

    def mock_extract_pool_amount(pool_event: LogReceipt, event_type: ScaledTokenEventType | None = None, operation_type: OperationType | None = None) -> int:
        import eth_abi.abi
        (debt_to_cover, _, _, _) = eth_abi.abi.decode(["uint256", "uint256", "address", "bool"], pool_event["data"])
        return debt_to_cover

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
            "pool_revision": 9,
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

    def mock_extract_pool_amount(pool_event: LogReceipt, event_type: ScaledTokenEventType | None = None, operation_type: OperationType | None = None) -> int:
        import eth_abi.abi
        (_, collateral, _, _) = eth_abi.abi.decode(["uint256", "uint256", "address", "bool"], pool_event["data"])
        return collateral

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


def _create_mock_context_debt_mint() -> MagicMock:
    """Context for net debt increase case."""
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

    def mock_extract_pool_amount(pool_event: LogReceipt, event_type: ScaledTokenEventType | None = None, operation_type: OperationType | None = None) -> int:
        import eth_abi.abi
        (debt_to_cover, _, _, _) = eth_abi.abi.decode(["uint256", "uint256", "address", "bool"], pool_event["data"])
        return debt_to_cover

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


def _create_mock_context_gho_debt() -> MagicMock:
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

    def mock_extract_pool_amount(pool_event: LogReceipt, event_type: ScaledTokenEventType | None = None, operation_type: OperationType | None = None) -> int:
        import eth_abi.abi
        (debt_to_cover, _, _, _) = eth_abi.abi.decode(["uint256", "uint256", "address", "bool"], pool_event["data"])
        return debt_to_cover

    def mock_calculate(event_type: ScaledTokenEventType, raw_amount: int, index: int, token_revision: int) -> int:
        RAY = 10**27
        return raw_amount * RAY // index

    def mock_build_enriched_event(event: "ScaledTokenEvent", operation: "Operation", raw_amount: int, scaled_amount: int | None) -> EnrichedScaledTokenEvent:
        event_type = event.event_type
        class_map = {ScaledTokenEventType.GHO_DEBT_BURN: EnrichedGhoDebtBurnEvent}
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
            "discount_percent": 0,
            "discount_scaled": 0,
        }
        return enriched_class(**kwargs)

    mock_context.get_token_revision = mock_get_token_revision
    mock_context.get_underlying_asset = mock_get_underlying_asset
    mock_context.extract_pool_amount = mock_extract_pool_amount
    mock_context.calculate = mock_calculate
    mock_context.build_enriched_event = mock_build_enriched_event
    return mock_context
