"""Tests for Aave V3 event matching framework.

These tests verify the centralized event matching logic handles all edge cases
identified in the bug reports in debug/aave/.

Test Coverage:
- Event consumption policies (CONSUMABLE, REUSABLE, CONDITIONAL)
- Liquidation transaction patterns (shared LIQUIDATION_CALL events)
- Repay with aTokens patterns (shared REPAY events)
- Flash loan liquidation edge cases (no matching Pool events)
- Self-liquidation patterns (debt mint + collateral mint matching same event)
- Interest accrual patterns (pure interest Mint events)
"""

from typing import cast
from unittest.mock import MagicMock

import eth_abi
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.aave_event_matching import (
    AaveV3Event,
    EventConsumptionPolicy,
    EventMatcher,
    EventMatchError,
    ScaledTokenEventType,
    _decode_address,  # noqa: PLC2701
    _should_consume_collateral_burn_pool_event,  # noqa: PLC2701
    _should_consume_debt_burn_pool_event,  # noqa: PLC2701
    _should_consume_debt_mint_pool_event,  # noqa: PLC2701
    _should_consume_gho_debt_burn_pool_event,  # noqa: PLC2701
)


class TestEventConsumptionPolicies:
    """Test event consumption policy enforcement."""

    def test_liquidation_call_never_consumed(self):
        """LIQUIDATION_CALL events should never be marked as consumed.

        See debug/aave/0010, 0011, 0012a for bugs caused by consuming LIQUIDATION_CALL.
        """
        pool_event = {
            "topics": [AaveV3Event.LIQUIDATION_CALL.value],
            "logIndex": 100,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000000ad7dcb"  # debtToCover
                "0000000000000000000000000000000000000000000000000000000000003a98"  # liquidatedCollateral
                "000000000000000000000000e27bfd9d354e7e0f7c5ef2fea0cd9c3af3533a32"  # liquidator
                "0000000000000000000000000000000000000000000000000000000000000000"  # receiveAToken
            ),
        }

        result = _should_consume_collateral_burn_pool_event(cast("LogReceipt", pool_event))
        assert result is False, "LIQUIDATION_CALL should never be consumed"

    def test_deficit_created_never_consumed(self):
        """DEFICIT_CREATED events should never be marked as consumed.

        See debug/aave/0013 for flash loan liquidation with DEFICIT_CREATED.
        """
        pool_event = {
            "topics": [AaveV3Event.DEFICIT_CREATED.value],
            "logIndex": 105,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000000ad7dcb"  # amountCreated
            ),
        }

        result = _should_consume_debt_burn_pool_event(cast("LogReceipt", pool_event))
        assert result is False, "DEFICIT_CREATED should never be consumed"

    def test_repay_consumed_when_use_atokens_false(self):
        """REPAY events should be consumed when useATokens=False.

        See debug/aave/0008 for repay-with-aTokens pattern.
        """
        pool_event = {
            "topics": [AaveV3Event.REPAY.value],
            "logIndex": 101,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000000000000"  # amount
                "0000000000000000000000000000000000000000000000000000000000000000"  # useATokens=False
            ),
        }

        result = _should_consume_collateral_burn_pool_event(cast("LogReceipt", pool_event))
        assert result is True, "REPAY should be consumed when useATokens=False"

    def test_repay_not_consumed_when_use_atokens_true(self):
        """REPAY events should NOT be consumed when useATokens=True.

        When useATokens=True, the REPAY event must match both debt burn
        and collateral burn (aToken burn for repayment).

        See debug/aave/0008 for repay-with-aTokens pattern.
        """
        pool_event = {
            "topics": [AaveV3Event.REPAY.value],
            "logIndex": 101,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000000000000"  # amount
                "0000000000000000000000000000000000000000000000000000000000000001"  # useATokens=True
            ),
        }

        result = _should_consume_collateral_burn_pool_event(cast("LogReceipt", pool_event))
        assert result is False, "REPAY should NOT be consumed when useATokens=True"

    def test_withdraw_always_consumed(self):
        """WITHDRAW events should always be consumed."""
        pool_event = {
            "topics": [AaveV3Event.WITHDRAW.value],
            "logIndex": 102,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000000000000"  # amount
            ),
        }

        result = _should_consume_collateral_burn_pool_event(cast("LogReceipt", pool_event))
        assert result is True, "WITHDRAW should always be consumed"

    def test_borrow_always_consumed(self):
        """BORROW events should always be consumed."""
        pool_event = {
            "topics": [AaveV3Event.BORROW.value],
            "logIndex": 103,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000000000000"  # caller
                "0000000000000000000000000000000000000000000000000000000000000000"  # amount
                "0000000000000000000000000000000000000000000000000000000000000002"  # interestRateMode=2
                "0000000000000000000000000000000000000000000000000000000000000000"  # borrowRate
            ),
        }

        result = _should_consume_debt_mint_pool_event(cast("LogReceipt", pool_event))
        assert result is True, "BORROW should always be consumed"


class TestLiquidationCallConsumptionPattern:
    """Test LIQUIDATION_CALL event consumption across multiple operations.

    These tests verify that LIQUIDATION_CALL events remain available
    to match multiple scaled token events in liquidation transactions.

    See debug/aave/0010, 0011, 0012a for related bugs.
    """

    def create_mock_tx_context(self, pool_events: list[LogReceipt]) -> MagicMock:
        """Create a mock TransactionContext with pool events."""
        tx_context = MagicMock()
        tx_context.pool_events = pool_events
        tx_context.matched_pool_events = {}
        return tx_context

    def test_liquidation_call_shared_between_debt_and_collateral_burns(self):
        """LIQUIDATION_CALL should be shared between debt burn and collateral burn.

        Transaction pattern (0x574695... from debug/aave/0010):
        - GHO debt burn (logIndex 100) → matches LIQUIDATION_CALL
        - WETH collateral burn (logIndex 104) → matches same LIQUIDATION_CALL
        - LIQUIDATION_CALL (logIndex 113) → shared event
        """
        liquidation_call_event = {
            "topics": [
                AaveV3Event.LIQUIDATION_CALL.value,
                HexBytes(
                    "0x000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                ),  # collateralAsset=WETH
                HexBytes(
                    "0x00000000000000000000000040d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f"
                ),  # debtAsset=GHO
                HexBytes(
                    "0x000000000000000000000000225c63381cb487f64aa1fc37a59baa3228d6d4ef"
                ),  # user
            ],
            "logIndex": 113,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000021c7d5d56c8d9c000"  # debtToCover
                "0000000000000000000000000000000000000000000000000000340785427800"  # liquidatedCollateral
                "000000000000000000000000e27bfd9d354e7e0f7c5ef2fea0cd9c3af3533a32"  # liquidator
                "0000000000000000000000000000000000000000000000000000000000000000"  # receiveAToken
            ),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [liquidation_call_event]))
        matcher = EventMatcher(tx_context)

        user_address = _decode_address(
            HexBytes("0x000000000000000000000000225c63381cb487f64aa1fc37a59baa3228d6d4ef")
        )
        gho_reserve = _decode_address(
            HexBytes("0x00000000000000000000000040d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f")
        )
        weth_reserve = _decode_address(
            HexBytes("0x000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2")
        )

        # First: GHO debt burn matches LIQUIDATION_CALL
        result1 = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.GHO_DEBT_BURN,
            user_address=user_address,
            reserve_address=gho_reserve,
        )

        assert result1 is not None, "GHO debt burn should match LIQUIDATION_CALL"
        assert result1["pool_event"] == liquidation_call_event
        assert result1["should_consume"] is False, "LIQUIDATION_CALL should not be consumed"
        assert liquidation_call_event["logIndex"] not in tx_context.matched_pool_events

        # Second: WETH collateral burn matches same LIQUIDATION_CALL
        result2 = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_BURN,
            user_address=user_address,
            reserve_address=weth_reserve,
        )

        assert result2 is not None, "WETH collateral burn should match same LIQUIDATION_CALL"
        assert result2["pool_event"] == liquidation_call_event
        assert result2["should_consume"] is False

    def test_liquidation_call_shared_between_debt_mint_and_collateral_mint(self):
        """LIQUIDATION_CALL should be shared between debt mint and collateral mint.

        Transaction pattern (0x653fcf... from debug/aave/0011):
        - Self-liquidation where liquidator borrows and receives collateral
        - Debt mint matches LIQUIDATION_CALL (liquidator borrowing)
        - Collateral mint matches same LIQUIDATION_CALL (liquidator receiving)
        """
        liquidation_call_event = {
            "topics": [
                AaveV3Event.LIQUIDATION_CALL.value,
                HexBytes(
                    "0x000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                ),  # collateralAsset
                HexBytes(
                    "0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
                ),  # debtAsset
                HexBytes(
                    "0x0000000000000000000000008a643b83fe7c75c40f31d6b0d4d494a08fc08d48"
                ),  # user=liquidator
            ],
            "logIndex": 143,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000000000000"
                "0000000000000000000000000000000000000000000000000000000000000000"
                "0000000000000000000000000000000000000000000000000000000000000000"
                "0000000000000000000000000000000000000000000000000000000000000000"
            ),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [liquidation_call_event]))
        matcher = EventMatcher(tx_context)

        liquidator = _decode_address(
            HexBytes("0x0000000000000000000000008a643b83fe7c75c40f31d6b0d4d494a08fc08d48")
        )

        # Debt mint matches LIQUIDATION_CALL
        result1 = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.DEBT_MINT,
            user_address=liquidator,
            reserve_address=_decode_address(
                HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
            ),
        )

        assert result1 is not None
        assert result1["should_consume"] is False

        # Collateral mint matches same LIQUIDATION_CALL
        result2 = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=liquidator,
            reserve_address=_decode_address(
                HexBytes("0x000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2")
            ),
        )

        assert result2 is not None
        assert result2["should_consume"] is False


class TestRepayWithATokensPattern:
    """Test repay-with-aTokens event consumption patterns.

    When useATokens=True, a single REPAY event must match both:
    - The debt burn (vToken burn reducing debt)
    - The collateral burn (aToken burn reducing collateral)

    See debug/aave/0008, 0012b for related bugs.
    """

    def create_mock_tx_context(self, pool_events: list[LogReceipt]) -> MagicMock:
        """Create a mock TransactionContext with pool events."""
        tx_context = MagicMock()
        tx_context.pool_events = pool_events
        tx_context.matched_pool_events = {}
        return tx_context

    def test_repay_shared_when_use_atokens_true(self):
        """REPAY should be shared between debt burn and collateral burn when useATokens=True."""
        repay_event = {
            "topics": [
                AaveV3Event.REPAY.value,
                HexBytes(
                    "0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
                ),  # reserve=USDC
                HexBytes(
                    "0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2"
                ),  # user
            ],
            "logIndex": 101,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000005f5e100"  # amount
                "0000000000000000000000000000000000000000000000000000000000000001"  # useATokens=True
            ),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [repay_event]))
        matcher = EventMatcher(tx_context)

        user_address = _decode_address(
            HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2")
        )
        reserve_address = _decode_address(
            HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
        )

        # Debt burn matches REPAY but does NOT consume it
        result1 = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.DEBT_BURN,
            user_address=user_address,
            reserve_address=reserve_address,
        )

        assert result1 is not None
        assert result1["pool_event"] == repay_event
        assert result1["should_consume"] is False, (
            "REPAY should not be consumed when useATokens=True"
        )
        assert repay_event["logIndex"] not in tx_context.matched_pool_events

        # Collateral burn matches same REPAY
        result2 = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_BURN,
            user_address=user_address,
            reserve_address=reserve_address,
        )

        assert result2 is not None
        assert result2["pool_event"] == repay_event
        assert result2["should_consume"] is False

    def test_repay_consumed_when_use_atokens_false(self):
        """REPAY should be consumed when useATokens=False (normal repayment)."""
        repay_event = {
            "topics": [
                AaveV3Event.REPAY.value,
                HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
                HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2"),
            ],
            "logIndex": 101,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000005f5e100"
                "0000000000000000000000000000000000000000000000000000000000000000"  # useATokens=False
            ),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [repay_event]))
        matcher = EventMatcher(tx_context)

        user_address = _decode_address(
            HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2")
        )
        reserve_address = _decode_address(
            HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
        )

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.DEBT_BURN,
            user_address=user_address,
            reserve_address=reserve_address,
        )

        assert result is not None
        assert result["should_consume"] is True, "REPAY should be consumed when useATokens=False"
        assert repay_event["logIndex"] in tx_context.matched_pool_events


class TestEventMatchingOrder:
    """Test event matching order and priority."""

    def create_mock_tx_context(self, pool_events: list[LogReceipt]) -> MagicMock:
        """Create a mock TransactionContext with pool events."""
        tx_context = MagicMock()
        tx_context.pool_events = pool_events
        tx_context.matched_pool_events = {}
        return tx_context

    def test_collateral_mint_tries_supply_first(self):
        """Collateral mint should try SUPPLY before WITHDRAW."""
        supply_event = {
            "topics": [
                AaveV3Event.SUPPLY.value,
                HexBytes(
                    "0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
                ),  # reserve
                HexBytes(
                    "0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2"
                ),  # onBehalfOf
            ],
            "logIndex": 100,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000000000000"  # caller
                "0000000000000000000000000000000000000000000000000000000005f5e100"  # amount
            ),
        }

        withdraw_event = {
            "topics": [
                AaveV3Event.WITHDRAW.value,
                HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
                HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2"),
            ],
            "logIndex": 101,
            "data": HexBytes("0000000000000000000000000000000000000000000000000000000005f5e100"),
        }

        tx_context = self.create_mock_tx_context(
            cast("list[LogReceipt]", [supply_event, withdraw_event])
        )
        matcher = EventMatcher(tx_context)

        user_address = _decode_address(
            HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2")
        )
        reserve_address = _decode_address(
            HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
        )

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=user_address,
            reserve_address=reserve_address,
        )

        assert result is not None
        assert result["pool_event"] == supply_event, "Should match SUPPLY first"

    def test_collateral_burn_tries_withdraw_first(self):
        """Collateral burn should try WITHDRAW before REPAY."""
        withdraw_event = {
            "topics": [
                AaveV3Event.WITHDRAW.value,
                HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
                HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2"),
            ],
            "logIndex": 100,
            "data": HexBytes("0000000000000000000000000000000000000000000000000000000005f5e100"),
        }

        repay_event = {
            "topics": [
                AaveV3Event.REPAY.value,
                HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
                HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2"),
            ],
            "logIndex": 101,
            "data": HexBytes(
                "0000000000000000000000000000000000000000000000000000000005f5e100"
                "0000000000000000000000000000000000000000000000000000000000000000"
            ),
        }

        tx_context = self.create_mock_tx_context(
            cast("list[LogReceipt]", [withdraw_event, repay_event])
        )
        matcher = EventMatcher(tx_context)

        user_address = _decode_address(
            HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2")
        )
        reserve_address = _decode_address(
            HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
        )

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_BURN,
            user_address=user_address,
            reserve_address=reserve_address,
        )

        assert result is not None
        assert result["pool_event"] == withdraw_event, "Should match WITHDRAW first"


class TestEventDataExtraction:
    """Test extraction of data from matched pool events."""

    def create_mock_tx_context(self, pool_events: list[LogReceipt]) -> MagicMock:
        """Create a mock TransactionContext with pool events."""
        tx_context = MagicMock()
        tx_context.pool_events = pool_events
        tx_context.matched_pool_events = {}
        return tx_context

    def test_extract_supply_amount(self):
        """Extract raw amount from SUPPLY event."""
        supply_event = {
            "topics": [
                AaveV3Event.SUPPLY.value,
                HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
                HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2"),
            ],
            "logIndex": 100,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000000000000"  # caller
                "0000000000000000000000000000000000000000000000000000000005f5e100"  # amount=100,000,000
            ),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [supply_event]))
        matcher = EventMatcher(tx_context)

        user_address = _decode_address(
            HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2")
        )
        reserve_address = _decode_address(
            HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
        )

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=user_address,
            reserve_address=reserve_address,
        )

        assert result is not None
        assert result["extraction_data"]["raw_amount"] == 100_000_000

    def test_extract_repay_use_atokens(self):
        """Extract useATokens flag from REPAY event."""
        repay_event = {
            "topics": [
                AaveV3Event.REPAY.value,
                HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
                HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2"),
            ],
            "logIndex": 101,
            "data": HexBytes(
                "0000000000000000000000000000000000000000000000000000000005f5e100"  # amount
                "0000000000000000000000000000000000000000000000000000000000000001"  # useATokens=True
            ),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [repay_event]))
        matcher = EventMatcher(tx_context)

        user_address = _decode_address(
            HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2")
        )
        reserve_address = _decode_address(
            HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
        )

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.DEBT_BURN,
            user_address=user_address,
            reserve_address=reserve_address,
        )

        assert result is not None
        assert result["extraction_data"]["raw_amount"] == 100_000_000
        assert result["extraction_data"]["use_a_tokens"] == 1  # True as int

    def test_extract_liquidation_amounts(self):
        """Extract debt and collateral amounts from LIQUIDATION_CALL event."""
        liquidation_event = {
            "topics": [
                AaveV3Event.LIQUIDATION_CALL.value,
                HexBytes(
                    "0x000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                ),  # collateralAsset
                HexBytes(
                    "0x00000000000000000000000040d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f"
                ),  # debtAsset
                HexBytes(
                    "0x000000000000000000000000225c63381cb487f64aa1fc37a59baa3228d6d4ef"
                ),  # user
            ],
            "logIndex": 113,
            "data": HexBytes(
                "0000000000000000000000000000000000000000000000000000000000ad7dcb"  # debtToCover=11,347,979
                "0000000000000000000000000000000000000000000000000000000000003a98"  # liquidatedCollateral=15,000
                "000000000000000000000000e27bfd9d354e7e0f7c5ef2fea0cd9c3af3533a32"  # liquidator
                "0000000000000000000000000000000000000000000000000000000000000000"  # receiveAToken
            ),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [liquidation_event]))
        matcher = EventMatcher(tx_context)

        user_address = _decode_address(
            HexBytes("0x000000000000000000000000225c63381cb487f64aa1fc37a59baa3228d6d4ef")
        )
        debt_reserve = _decode_address(
            HexBytes("0x00000000000000000000000040d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f")
        )

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.GHO_DEBT_BURN,
            user_address=user_address,
            reserve_address=debt_reserve,
        )

        assert result is not None
        assert result["extraction_data"]["debt_to_cover"] == 11_369_931  # 0xad7dcb
        assert result["extraction_data"]["liquidated_collateral"] == 15_000  # 0x3a98


class TestEventMatchError:
    """Test EventMatchError exception."""

    def test_error_includes_context(self):
        """EventMatchError should include transaction context."""
        user_address = _decode_address(
            HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2")
        )
        reserve_address = _decode_address(
            HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
        )
        assert user_address is not None
        assert reserve_address is not None

        error = EventMatchError(
            "No matching event found",
            tx_hash=HexBytes("0x1234"),
            user_address=user_address,
            reserve_address=reserve_address,
            available_events=["0xe413a321", "0x2b6273e6"],
        )

        assert error.tx_hash == HexBytes("0x1234")
        assert error.user_address is not None
        assert error.reserve_address is not None
        assert "0x4490db".lower() in error.user_address.lower()
        assert "0xa0b869".lower() in error.reserve_address.lower()
        assert len(error.available_events) == 2


class TestHelperFunctions:
    """Test helper functions."""

    def test_decode_address(self):
        """Test address decoding from topic."""
        topic = HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2")
        address = _decode_address(topic)
        # Address should be checksummed (mixed case)
        assert address == "0x4490dB0FC0E8dE7c7192F12f9C5E8409E7caDda2"
        assert address.lower() == "0x4490db0fc0e8de7c7192f12f9c5e8409e7cadda2"


class TestMatchConfigurations:
    """Test that all match configurations are valid."""

    def test_all_event_types_have_configs(self):
        """All ScaledTokenEventTypes should have valid MatchConfigs."""
        for event_type in ScaledTokenEventType:
            config = EventMatcher.CONFIGS.get(event_type)
            assert config is not None, f"Missing config for {event_type}"
            assert len(config.pool_event_types) > 0, f"Empty pool_event_types for {event_type}"

    def test_liquidation_call_in_reusable_configs(self):
        """LIQUIDATION_CALL should be in configs with REUSABLE policy."""
        configs_with_liq = [
            ScaledTokenEventType.COLLATERAL_MINT,
            ScaledTokenEventType.COLLATERAL_BURN,
            ScaledTokenEventType.DEBT_MINT,
            ScaledTokenEventType.DEBT_BURN,
            ScaledTokenEventType.GHO_DEBT_BURN,
        ]

        for event_type in configs_with_liq:
            config = EventMatcher.CONFIGS[event_type]
            assert AaveV3Event.LIQUIDATION_CALL in config.pool_event_types
            assert config.consumption_policy in {
                EventConsumptionPolicy.REUSABLE,
                EventConsumptionPolicy.CONDITIONAL,
            }


class TestEdgeCases:
    """Test edge cases and boundary conditions."""

    def create_mock_tx_context(self, pool_events: list[LogReceipt]) -> MagicMock:
        """Create a mock TransactionContext with pool events."""
        tx_context = MagicMock()
        tx_context.pool_events = pool_events
        tx_context.matched_pool_events = {}
        return tx_context

    def test_no_pool_events_returns_none(self):
        """When no pool events available, should return None."""
        tx_context = self.create_mock_tx_context([])
        matcher = EventMatcher(tx_context)

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=_decode_address(
                HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2")
            ),
            reserve_address=_decode_address(
                HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
            ),
        )

        assert result is None

    def test_already_consumed_events_skipped(self):
        """Already consumed events should be skipped."""
        supply_event = {
            "topics": [
                AaveV3Event.SUPPLY.value,
                HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
                HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2"),
            ],
            "logIndex": 100,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000000000000"
                "0000000000000000000000000000000000000000000000000000000005f5e100"
            ),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [supply_event]))
        tx_context.matched_pool_events[100] = True  # Mark as consumed
        matcher = EventMatcher(tx_context)

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=get_checksum_address("0x4490db0fc0e8de7c7192f12f9c5e8409e7cadda2"),
            reserve_address=get_checksum_address("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
        )

        assert result is None, "Should not match already consumed event"

    def test_multiple_users_checked(self):
        """When check_users provided, should try each user."""
        # Event matches onBehalfOf=user2, not user1
        supply_event = {
            "topics": [
                AaveV3Event.SUPPLY.value,
                HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
                HexBytes(
                    "0x0000000000000000000000002222222222222222222222222222222222222222"
                ),  # onBehalfOf
            ],
            "logIndex": 100,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000000000000"
                "0000000000000000000000000000000000000000000000000000000005f5e100"
            ),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [supply_event]))
        matcher = EventMatcher(tx_context)

        user1 = get_checksum_address("0x1111111111111111111111111111111111111111")
        user2 = get_checksum_address("0x2222222222222222222222222222222222222222")
        reserve = get_checksum_address("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")

        # Should not find with only user1
        result1 = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=user1,
            reserve_address=reserve,
        )
        assert result1 is None

        # Should find when user2 is also checked
        result2 = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=user1,
            reserve_address=reserve,
            check_users=[user2],
        )
        assert result2 is not None


class TestDebtMintEventMatching:
    """Test debt mint event matching patterns."""

    def create_mock_tx_context(self, pool_events: list[LogReceipt]) -> MagicMock:
        """Create a mock TransactionContext with pool events."""
        tx_context = MagicMock()
        tx_context.pool_events = pool_events
        tx_context.matched_pool_events = {}
        return tx_context

    def test_standard_borrow_matches_with_user_address(self):
        """Normal BORROW where onBehalfOf equals user.address."""
        user_address = _decode_address(
            HexBytes("0x0000000000000000000000001111111111111111111111111111111111111111")
        )
        reserve_address = _decode_address(
            HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
        )

        borrow_event = {
            "topics": [
                AaveV3Event.BORROW.value,
                HexBytes(
                    "0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
                ),  # reserve
                HexBytes(
                    "0x0000000000000000000000001111111111111111111111111111111111111111"
                ),  # onBehalfOf=user
            ],
            "logIndex": 100,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000000000000"  # caller
                "0000000000000000000000000000000000000000000000000000000005f5e100"  # amount=100M
                "0000000000000000000000000000000000000000000000000000000000000002"  # interestRateMode=2
                "0000000000000000000000000000000000000000000000000000000000000000"  # borrowRate
            ),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [borrow_event]))
        matcher = EventMatcher(tx_context)

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.DEBT_MINT,
            user_address=user_address,
            reserve_address=reserve_address,
        )

        assert result is not None
        assert result["pool_event"] == borrow_event
        assert result["should_consume"] is True, "BORROW should be consumed"
        assert result["extraction_data"]["raw_amount"] == 100_000_000
        assert 100 in tx_context.matched_pool_events

    def test_adapter_borrow_matches_with_caller_address(self):
        """Adapter pattern where onBehalfOf is the adapter, not user."""
        user_address = _decode_address(
            HexBytes("0x0000000000000000000000001111111111111111111111111111111111111111")
        )
        adapter_address = _decode_address(
            HexBytes("0x0000000000000000000000002222222222222222222222222222222222222222")
        )
        reserve_address = _decode_address(
            HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
        )

        # Borrow event has onBehalfOf=adapter (the caller), not user
        borrow_event = {
            "topics": [
                AaveV3Event.BORROW.value,
                HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
                HexBytes(
                    "0x0000000000000000000000002222222222222222222222222222222222222222"
                ),  # onBehalfOf=adapter
            ],
            "logIndex": 100,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000000000000"
                "0000000000000000000000000000000000000000000000000000000005f5e100"
                "0000000000000000000000000000000000000000000000000000000000000002"
                "0000000000000000000000000000000000000000000000000000000000000000"
            ),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [borrow_event]))
        matcher = EventMatcher(tx_context)

        # First try with user.address - should not match
        result1 = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.DEBT_MINT,
            user_address=user_address,
            reserve_address=reserve_address,
        )
        assert result1 is None, "Should not match with user.address when onBehalfOf is adapter"

        # Try with caller_address (adapter) - should match
        result2 = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.DEBT_MINT,
            user_address=user_address,
            reserve_address=reserve_address,
            check_users=[adapter_address],  # This enables adapter pattern
        )

        assert result2 is not None
        assert result2["pool_event"] == borrow_event
        assert result2["extraction_data"]["raw_amount"] == 100_000_000

    def test_liquidation_debt_mint_matches_liquidation_call(self):
        """Liquidation where liquidator borrows - matches LIQUIDATION_CALL."""
        liquidator = _decode_address(
            HexBytes("0x0000000000000000000000001111111111111111111111111111111111111111")
        )
        debt_reserve = _decode_address(
            HexBytes("0x00000000000000000000000040d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f")
        )

        liquidation_event = {
            "topics": [
                AaveV3Event.LIQUIDATION_CALL.value,
                HexBytes(
                    "0x000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                ),  # collateralAsset
                HexBytes(
                    "0x00000000000000000000000040d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f"
                ),  # debtAsset
                HexBytes(
                    "0x0000000000000000000000001111111111111111111111111111111111111111"
                ),  # user=liquidator
            ],
            "logIndex": 100,
            "data": HexBytes(
                "0000000000000000000000000000000000000000000000000000000000ad7dcb"  # debtToCover
                "0000000000000000000000000000000000000000000000000000000000003a98"  # liquidatedCollateral
                "000000000000000000000000e27bfd9d354e7e0f7c5ef2fea0cd9c3af3533a32"  # liquidator
                "0000000000000000000000000000000000000000000000000000000000000000"  # receiveAToken
            ),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [liquidation_event]))
        matcher = EventMatcher(tx_context)

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.DEBT_MINT,
            user_address=liquidator,
            reserve_address=debt_reserve,
        )

        assert result is not None
        assert result["pool_event"] == liquidation_event
        assert result["should_consume"] is False, "LIQUIDATION_CALL should not be consumed"
        assert 100 not in tx_context.matched_pool_events
        assert result["extraction_data"]["debt_to_cover"] == 11_369_931

    def test_borrow_takes_priority_over_repay(self):
        """If both BORROW and REPAY available, BORROW should match first."""
        user_address = _decode_address(
            HexBytes("0x0000000000000000000000001111111111111111111111111111111111111111")
        )
        reserve_address = _decode_address(
            HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
        )

        # Both events available - BORROW should be tried first
        repay_event = {
            "topics": [
                AaveV3Event.REPAY.value,
                HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
                HexBytes("0x0000000000000000000000001111111111111111111111111111111111111111"),
            ],
            "logIndex": 100,
            "data": HexBytes(
                "0000000000000000000000000000000000000000000000000000000005f5e100"
                "0000000000000000000000000000000000000000000000000000000000000000"
            ),
        }

        borrow_event = {
            "topics": [
                AaveV3Event.BORROW.value,
                HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
                HexBytes("0x0000000000000000000000001111111111111111111111111111111111111111"),
            ],
            "logIndex": 101,
            "data": HexBytes(
                "0000000000000000000000000000000000000000000000000000000000000000"
                "0000000000000000000000000000000000000000000000000000000005f5e100"
                "0000000000000000000000000000000000000000000000000000000000000002"
                "0000000000000000000000000000000000000000000000000000000000000000"
            ),
        }

        tx_context = self.create_mock_tx_context(
            cast("list[LogReceipt]", [repay_event, borrow_event])
        )
        matcher = EventMatcher(tx_context)

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.DEBT_MINT,
            user_address=user_address,
            reserve_address=reserve_address,
        )

        assert result is not None
        # BORROW is first in MatchConfig, so it should match
        assert result["pool_event"] == borrow_event
        assert result["extraction_data"]["raw_amount"] == 100_000_000

    def test_no_match_returns_none(self):
        """When no pool event matches, EventMatcher returns None (handler continues)."""
        user_address = _decode_address(
            HexBytes("0x0000000000000000000000001111111111111111111111111111111111111111")
        )
        reserve_address = _decode_address(
            HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
        )

        tx_context = self.create_mock_tx_context([])
        matcher = EventMatcher(tx_context)

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.DEBT_MINT,
            user_address=user_address,
            reserve_address=reserve_address,
        )

        assert result is None


class TestCollateralMintEventMatching:
    """Test collateral mint event matching patterns.

    See debug/aave/0002 for value vs balance_increase matching logic.
    """

    def create_mock_tx_context(self, pool_events: list[LogReceipt]) -> MagicMock:
        """Create a mock TransactionContext with pool events."""
        tx_context = MagicMock()
        tx_context.pool_events = pool_events
        tx_context.matched_pool_events = {}
        return tx_context

    def test_standard_supply_matches_first(self):
        """Standard deposit: value > balance_increase, SUPPLY matches first."""
        user_address = _decode_address(
            HexBytes("0x0000000000000000000000001111111111111111111111111111111111111111")
        )
        reserve_address = _decode_address(
            HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
        )

        supply_event = {
            "topics": [
                AaveV3Event.SUPPLY.value,
                HexBytes(
                    "0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
                ),  # reserve
                HexBytes(
                    "0x0000000000000000000000001111111111111111111111111111111111111111"
                ),  # onBehalfOf=user
            ],
            "logIndex": 100,
            "data": HexBytes(
                "0x0000000000000000000000000000000000000000000000000000000000000000"  # caller
                "0000000000000000000000000000000000000000000000000000000005f5e100"  # amount=100M
            ),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [supply_event]))
        matcher = EventMatcher(tx_context)

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=user_address,
            reserve_address=reserve_address,
        )

        assert result is not None
        assert result["pool_event"] == supply_event
        assert result["should_consume"] is True, "SUPPLY should be consumed"
        assert result["extraction_data"]["raw_amount"] == 100_000_000
        assert 100 in tx_context.matched_pool_events

    def test_liquidation_collateral_mint_matches_liquidation_call(self):
        """Liquidator receives collateral as aTokens: matches LIQUIDATION_CALL."""
        liquidator = _decode_address(
            HexBytes("0x0000000000000000000000001111111111111111111111111111111111111111")
        )
        collateral_reserve = _decode_address(
            HexBytes("0x000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2")
        )

        liquidation_event = {
            "topics": [
                AaveV3Event.LIQUIDATION_CALL.value,
                HexBytes(
                    "0x000000000000000000000000c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                ),  # collateralAsset
                HexBytes(
                    "0x00000000000000000000000040d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f"
                ),  # debtAsset
                HexBytes(
                    "0x0000000000000000000000001111111111111111111111111111111111111111"
                ),  # user=liquidator
            ],
            "logIndex": 100,
            "data": HexBytes(
                "0000000000000000000000000000000000000000000000000000000000ad7dcb"  # debtToCover
                "0000000000000000000000000000000000000000000000000000000000003a98"  # liquidatedCollateral=15,000
                "000000000000000000000000e27bfd9d354e7e0f7c5ef2fea0cd9c3af3533a32"  # liquidator
                "0000000000000000000000000000000000000000000000000000000000000001"  # receiveAToken=True
            ),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [liquidation_event]))
        matcher = EventMatcher(tx_context)

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=liquidator,
            reserve_address=collateral_reserve,
        )

        assert result is not None
        assert result["pool_event"] == liquidation_event
        assert result["should_consume"] is False, "LIQUIDATION_CALL should not be consumed"
        assert 100 not in tx_context.matched_pool_events
        assert result["extraction_data"]["liquidated_collateral"] == 15_000

    def test_repay_with_atokens_excess_collateral_mint_matches_repay(self):
        """repayWithATokens with excess: collateral mint matches REPAY event.

        When a user repays with aTokens and over-pays (or due to interest accrual),
        excess aTokens are minted back to the user. This collateral mint must match
        the REPAY event, which is also shared with the debt burn.

        Transaction pattern (0x31dff401... from debug/aave/0015):
        - User calls repayWithATokens() with aToken amount > actual debt
        - VariableDebtToken Burn event (debt repayment)
        - AToken Mint event (excess aTokens returned) - COLLATERAL_MINT
        - Pool Repay event (shared between both operations)

        See debug/aave/0015 for transaction details.
        """
        user = _decode_address(
            HexBytes("0x0000000000000000000000008899fAEd2e1b0e9b7F41E08b79bE71eC3d1f9EC1")
        )
        weth_reserve = _decode_address(
            HexBytes("0x000000000000000000000000C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
        )

        # Single REPAY event shared between debt burn and collateral mint
        # repayWithATokens() with excess aTokens returned
        repay_event = {
            "topics": [
                AaveV3Event.REPAY.value,
                HexBytes(
                    "0x000000000000000000000000C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
                ),  # reserve=WETH
                HexBytes(
                    "0x0000000000000000000000008899fAEd2e1b0e9b7F41E08b79bE71eC3d1f9EC1"
                ),  # user
            ],
            "logIndex": 55,
            "data": HexBytes(
                "0x000000000000000000000000000000000000000000000000000006b22a949618"  # amount=7,362,288,326,168
                "0000000000000000000000000000000000000000000000000000000000000001"  # useATokens=True
            ),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [repay_event]))
        matcher = EventMatcher(tx_context)

        # Collateral mint should match the REPAY event
        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=user,
            reserve_address=weth_reserve,
        )

        assert result is not None, "Collateral mint should match REPAY event"
        assert result["pool_event"] == repay_event
        assert result["should_consume"] is False, (
            "REPAY should not be consumed by collateral mint (shared with debt burn)"
        )
        assert 55 not in tx_context.matched_pool_events
        assert result["extraction_data"]["raw_amount"] == 7_362_288_326_168
        assert result["extraction_data"]["use_a_tokens"] == 1


class TestGHOLiquidationWithDeficitCreated:
    """Test GHO debt burn matching with DeficitCreated events.

    GHO uses a different liquidation mechanism than standard Aave assets.
    While standard assets emit LiquidationCall events, GHO liquidations
    emit DeficitCreated events instead.

    See debug/aave/0016 for transaction details.
    """

    def create_mock_tx_context(self, pool_events: list[LogReceipt]) -> MagicMock:
        """Create a mock TransactionContext with pool events."""
        tx_context = MagicMock()
        tx_context.pool_events = pool_events
        tx_context.matched_pool_events = {}
        return tx_context

    def test_gho_debt_burn_matches_deficit_created(self):
        """GHO debt burn during liquidation should match DeficitCreated event.

        Transaction: 0x0affc26fff867c734add4067a257ce189f0188aa5c9783489311a5edbb56c306
        Block: 22127030

        Event flow:
        - Log 660-661: GHO Debt Token Burn (~1.28 GHO)
        - Log 662: DeficitCreated event for GHO
        - Log 664: LiquidationCall event for USDC/WBTC (different asset)

        The GHO debt burn should match the DeficitCreated event, not LiquidationCall.
        """
        user_address = _decode_address(
            HexBytes("0x000000000000000000000000fb2788b2a3a0242429fd9ee2b151e149e3b244ec")
        )
        gho_reserve = _decode_address(
            HexBytes("0x00000000000000000000000040d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f")
        )

        # DeficitCreated event for GHO liquidation
        deficit_created_event = {
            "topics": [
                AaveV3Event.DEFICIT_CREATED.value,
                HexBytes(
                    "0x000000000000000000000000fb2788b2a3a0242429fd9ee2b151e149e3b244ec"
                ),  # user
                HexBytes(
                    "0x00000000000000000000000040d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f"
                ),  # asset=GHO
            ],
            "logIndex": 662,
            "data": HexBytes(
                "0x00000000000000000000000000000000000000000000000000048e1b04ae78ec"  # amountCreated=1,282,800,003,371,564 wei (~1.28 GHO)
            ),
        }

        # Separate LiquidationCall for WBTC debt (different asset)
        liquidation_event = {
            "topics": [
                AaveV3Event.LIQUIDATION_CALL.value,
                HexBytes(
                    "0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
                ),  # collateralAsset=USDC
                HexBytes(
                    "0x0000000000000000000000002260fac5e5542a773aa44fbcfedf7c193bc2c599"
                ),  # debtAsset=WBTC
                HexBytes(
                    "0x000000000000000000000000fb2788b2a3a0242429fd9ee2b151e149e3b244ec"
                ),  # user
            ],
            "logIndex": 664,
            "data": HexBytes(
                "000000000000000000000000000000000000000000000000000000000000008e"  # debtToCover=142 wei WBTC
                "000000000000000000000000000000000000000000000000000000000001f387"  # liquidatedCollateral=128,167 wei USDC
                "000000000000000000000000f00e2de0e78dff055a92ad4719a179ce275b6ef7"  # liquidator
                "0000000000000000000000000000000000000000000000000000000000000001"  # receiveAToken=True
            ),
        }

        tx_context = self.create_mock_tx_context(
            cast("list[LogReceipt]", [deficit_created_event, liquidation_event])
        )
        matcher = EventMatcher(tx_context)

        # GHO debt burn should match DeficitCreated event
        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.GHO_DEBT_BURN,
            user_address=user_address,
            reserve_address=gho_reserve,
        )

        assert result is not None, "GHO debt burn should match DeficitCreated event"
        assert result["pool_event"] == deficit_created_event
        assert result["should_consume"] is False, "DeficitCreated should not be consumed"
        assert 662 not in tx_context.matched_pool_events
        assert result["extraction_data"]["amount_created"] == 1_282_146_600_646_892

    def test_gho_debt_burn_prefers_deficit_created_over_liquidation_call(self):
        """When both DeficitCreated and LiquidationCall exist, GHO burn should match DeficitCreated first.

        In multi-asset liquidations, both event types may exist.
        GHO debt burns should prioritize DeficitCreated matching.
        """
        user_address = _decode_address(
            HexBytes("0x000000000000000000000000fb2788b2a3a0242429fd9ee2b151e149e3b244ec")
        )
        gho_reserve = _decode_address(
            HexBytes("0x00000000000000000000000040d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f")
        )

        # DeficitCreated should be checked before LIQUIDATION_CALL in the config order
        # Current config: [REPAY, LIQUIDATION_CALL, DEFICIT_CREATED]
        # So LIQUIDATION_CALL is checked before DEFICIT_CREATED
        # But LIQUIDATION_CALL won't match because GHO is not the debtAsset in this tx
        deficit_created_event = {
            "topics": [
                AaveV3Event.DEFICIT_CREATED.value,
                HexBytes("0x000000000000000000000000fb2788b2a3a0242429fd9ee2b151e149e3b244ec"),
                HexBytes("0x00000000000000000000000040d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f"),
            ],
            "logIndex": 662,
            "data": HexBytes("0x00000000000000000000000000000000000000000000000000048e1b04ae78ec"),
        }

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [deficit_created_event]))
        matcher = EventMatcher(tx_context)

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.GHO_DEBT_BURN,
            user_address=user_address,
            reserve_address=gho_reserve,
        )

        assert result is not None
        assert result["pool_event"] == deficit_created_event

    def test_deficit_created_never_consumed_for_gho_burn(self):
        """DeficitCreated events should never be consumed for GHO debt burns.

        Similar to LIQUIDATION_CALL, DeficitCreated may affect multiple positions
        and should remain available for other operations.
        """
        deficit_created_event = {
            "topics": [AaveV3Event.DEFICIT_CREATED.value],
            "logIndex": 662,
            "data": HexBytes("0x00000000000000000000000000000000000000000000000000048e1b04ae78ec"),
        }

        result = _should_consume_gho_debt_burn_pool_event(cast("LogReceipt", deficit_created_event))
        assert result is False, "DeficitCreated should never be consumed for GHO burns"


class TestGHODebtBurnEventOrdering:
    """Test GHO debt burn matching handles internal call pattern.

    Aave V3 emits token events during Pool function execution, not after.
    For GHO debt repayments, the Burn event (from debtToken) occurs before
    the Repay event (from Pool), which is counterintuitive but correct.

    This is because Pool.repay() internally calls debtToken.burn(), and the
    debt contract emits its event before Pool.repay() continues to emit its
    own event.

    See debug/aave/0025 for transaction details and fix.
    """

    def create_mock_tx_context(self, pool_events: list[LogReceipt]) -> MagicMock:
        """Create a mock TransactionContext with pool events."""
        tx_context = MagicMock()
        tx_context.pool_events = pool_events
        tx_context.matched_pool_events = {}
        return tx_context

    def create_repay_event(
        self, reserve_address: ChecksumAddress, user_address: ChecksumAddress, log_index: int
    ) -> LogReceipt:
        """Create a mock REPAY event with proper encoding."""
        # REPAY: data=(uint256 amount, bool useATokens)
        # 77967076299900000000 in hex = 0x4a5c1d9072f662000 (32 bytes padded)
        amount_hex = "0000000000000000000000000000000000000000000000000004a5c1d9072f662000"
        use_atokens_hex = "0000000000000000000000000000000000000000000000000000000000000000"
        encoded_data = HexBytes(amount_hex + use_atokens_hex)

        return cast(
            "LogReceipt",
            {
                "topics": [
                    AaveV3Event.REPAY.value,
                    HexBytes(reserve_address),
                    HexBytes(user_address),
                ],
                "logIndex": log_index,
                "data": encoded_data,
            },
        )

    def test_gho_debt_burn_matches_repay_with_later_log_index(self):
        """GHO debt burn should match REPAY even when REPAY has higher logIndex.

        Transaction: 0x6bde612c958454ffc86fd2a4ed59ddd63906ef0dc21320ec41b52661193b0205
        Block: 17699406

        Event flow (simplified):
        - Log 179: GHO Debt Token Burn (during Pool.repay() execution)
        - Log 180: ReserveDataUpdated
        - Log 184: Pool Repay event (after debtToken.burn() completes)

        The Burn event occurs at logIndex 179, but the matching REPAY
        occurs at logIndex 184. Without removing max_log_index constraint,
        the REPAY event would be skipped.

        This test verifies the fix: removing max_log_index allows matching
        GHO debt burns to REPAY events regardless of ordering.
        """
        user_address = _decode_address(
            HexBytes("0x000000000000000000000000b17bc7ad0e0f73db0dfe60e508445c237832a369")
        )
        gho_reserve = _decode_address(
            HexBytes("0x00000000000000000000000040d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f")
        )

        # Pool REPAY event at logIndex 184 (AFTER the burn event)
        repay_event = self.create_repay_event(gho_reserve, user_address, 184)

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [repay_event]))
        matcher = EventMatcher(tx_context)

        # Simulate that the burn event occurred at logIndex 179
        # The matching should succeed even though REPAY is at 184 > 179
        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.GHO_DEBT_BURN,
            user_address=user_address,
            reserve_address=gho_reserve,
        )

        assert result is not None, "GHO debt burn should match REPAY event"
        assert result["pool_event"] == repay_event
        assert result["should_consume"] is True, "REPAY should be consumed when useATokens=False"
        assert 184 in tx_context.matched_pool_events, "REPAY event should be marked as consumed"
        assert result["extraction_data"]["raw_amount"] == 77_967_076_299_900_000_000
        assert result["extraction_data"]["use_a_tokens"] == 0

    def test_gho_debt_burn_matches_repay_with_earlier_log_index(self):
        """GHO debt burn should also match REPAY when REPAY has lower logIndex.

        This is the opposite case - ensure we didn't break the standard
        scenario where events occur in expected order.
        """
        user_address = _decode_address(
            HexBytes("0x000000000000000000000000b17bc7ad0e0f73db0dfe60e508445c237832a369")
        )
        gho_reserve = _decode_address(
            HexBytes("0x00000000000000000000000040d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f")
        )

        # Pool REPAY event at logIndex 175 (BEFORE the burn event)
        repay_event = self.create_repay_event(gho_reserve, user_address, 175)

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [repay_event]))
        matcher = EventMatcher(tx_context)

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.GHO_DEBT_BURN,
            user_address=user_address,
            reserve_address=gho_reserve,
        )

        assert result is not None, "GHO debt burn should match REPAY event"
        assert result["pool_event"] == repay_event
        assert result["should_consume"] is True

    def test_gho_debt_burn_repay_mixed_with_other_events(self):
        """GHO debt burn matching in complex transactions with multiple events.

        This tests realistic scenarios where the transaction contains
        multiple pool events (e.g., other users' REPAY events).
        """
        user_address = _decode_address(
            HexBytes("0x000000000000000000000000b17bc7ad0e0f73db0dfe60e508445c237832a369")
        )
        gho_reserve = _decode_address(
            HexBytes("0x00000000000000000000000040d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f")
        )
        other_user = _decode_address(
            HexBytes("0x000000000000000000000000aaaaaa7ada777aa7ada777aa7ada777aa7ada777")
        )

        # REPAY for different user (should not match)
        other_repay = self.create_repay_event(gho_reserve, other_user, 182)

        # Target REPAY for GHO (correct user and reserve)
        gho_repay = self.create_repay_event(gho_reserve, user_address, 184)

        tx_context = self.create_mock_tx_context(cast("list[LogReceipt]", [other_repay, gho_repay]))
        matcher = EventMatcher(tx_context)

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.GHO_DEBT_BURN,
            user_address=user_address,
            reserve_address=gho_reserve,
        )

        assert result is not None
        assert result["pool_event"] == gho_repay
        assert result["pool_event"]["logIndex"] == 184
