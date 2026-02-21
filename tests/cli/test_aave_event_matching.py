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

from unittest.mock import MagicMock

from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.cli.aave_event_matching import (
    AaveV3Event,
    EventConsumptionPolicy,
    EventMatcher,
    EventMatchError,
    MatchConfig,
    ScaledTokenEventType,
    _decode_address,
    _should_consume_collateral_burn_pool_event,
    _should_consume_debt_burn_pool_event,
    _should_consume_debt_mint_pool_event,
)


class TestEventConsumptionPolicies:
    """Test event consumption policy enforcement."""

    def test_liquidation_call_never_consumed(self):
        """LIQUIDATION_CALL events should never be marked as consumed.

        See debug/aave/0010, 0011, 0012a for bugs caused by consuming LIQUIDATION_CALL.
        """
        config = MatchConfig(
            target_event=ScaledTokenEventType.COLLATERAL_BURN,
            pool_event_types=[AaveV3Event.LIQUIDATION_CALL],
            consumption_policy=EventConsumptionPolicy.REUSABLE,
        )

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

        matcher = MagicMock()
        result = _should_consume_collateral_burn_pool_event(pool_event)
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

        result = _should_consume_debt_burn_pool_event(pool_event)
        assert result is False, "DEFICIT_CREATED should never be consumed"

    def test_repay_consumed_when_useATokens_false(self):
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

        result = _should_consume_collateral_burn_pool_event(pool_event)
        assert result is True, "REPAY should be consumed when useATokens=False"

    def test_repay_not_consumed_when_useATokens_true(self):
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

        result = _should_consume_collateral_burn_pool_event(pool_event)
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

        result = _should_consume_collateral_burn_pool_event(pool_event)
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

        result = _should_consume_debt_mint_pool_event(pool_event)
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

        tx_context = self.create_mock_tx_context([liquidation_call_event])
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

        tx_context = self.create_mock_tx_context([liquidation_call_event])
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

    def test_repay_shared_when_useATokens_true(self):
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

        tx_context = self.create_mock_tx_context([repay_event])
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

    def test_repay_consumed_when_useATokens_false(self):
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

        tx_context = self.create_mock_tx_context([repay_event])
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

        tx_context = self.create_mock_tx_context([supply_event, withdraw_event])
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

        tx_context = self.create_mock_tx_context([withdraw_event, repay_event])
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

        tx_context = self.create_mock_tx_context([supply_event])
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

    def test_extract_repay_useATokens(self):
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

        tx_context = self.create_mock_tx_context([repay_event])
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

        tx_context = self.create_mock_tx_context([liquidation_event])
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
        error = EventMatchError(
            "No matching event found",
            tx_hash=HexBytes("0x1234"),
            user_address=_decode_address(
                HexBytes("0x0000000000000000000000004490db0fc0e8de7c7192f12f9c5e8409e7cadda2")
            ),
            reserve_address=_decode_address(
                HexBytes("0x000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
            ),
            available_events=["0xe413a321", "0x2b6273e6"],
        )

        assert error.tx_hash == HexBytes("0x1234")
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

        tx_context = self.create_mock_tx_context([supply_event])
        tx_context.matched_pool_events[100] = True  # Mark as consumed
        matcher = EventMatcher(tx_context)

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address="0x4490db0fc0e8de7c7192f12f9c5e8409e7cadda2",
            reserve_address="0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
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

        tx_context = self.create_mock_tx_context([supply_event])
        matcher = EventMatcher(tx_context)

        from degenbot.checksum_cache import get_checksum_address

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

        tx_context = self.create_mock_tx_context([borrow_event])
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

        tx_context = self.create_mock_tx_context([borrow_event])
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

        tx_context = self.create_mock_tx_context([liquidation_event])
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

        tx_context = self.create_mock_tx_context([repay_event, borrow_event])
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

        tx_context = self.create_mock_tx_context([supply_event])
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

        tx_context = self.create_mock_tx_context([liquidation_event])
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

        tx_context = self.create_mock_tx_context([repay_event])
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
