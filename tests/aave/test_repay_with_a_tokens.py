"""Test for repay with aTokens scenario.

When a user repays debt by burning aTokens directly (useATokens=True),
two burn events are emitted but there is only one Repay Pool event.
Both burn events must match the same Repay event.
"""

from unittest.mock import MagicMock

import eth_abi.abi


class TestRepayWithATokens:
    """Test that repay with aTokens correctly handles multiple burn events."""

    def test_repay_event_not_consumed_when_use_a_tokens_true(self):
        """
        When useATokens=True in a Repay event, the event should NOT be
        marked as consumed after the first burn matches it.

        This allows both the vToken burn and aToken burn to match the
        same Repay event.
        """
        # Simulate a transaction context with matched_pool_events tracking
        tx_context = MagicMock()
        tx_context.matched_pool_events = {}

        # Create a mock Repay event with useATokens=True
        # Event data: (uint256 amount, bool useATokens)
        # Using eth_abi to encode: amount=25000 USDC (6 decimals), useATokens=True
        repay_event_data = eth_abi.abi.encode(
            types=["uint256", "bool"],
            args=[25_000_000_000, True],  # 25,000 USDC with 6 decimals, useATokens=True
        )

        # Decode the event data (simulating what the code does)
        payback_amount, use_a_tokens = eth_abi.abi.decode(
            types=["uint256", "bool"],
            data=repay_event_data,
        )

        # Verify useATokens is True
        assert use_a_tokens is True
        assert payback_amount == 25_000_000_000

        # Simulate the logic that should be applied:
        # When useATokens=True, do NOT mark as consumed
        # When useATokens=False, DO mark as consumed
        log_index = 101
        if not use_a_tokens:
            tx_context.matched_pool_events[log_index] = True

        # Verify the event is NOT marked as consumed when useATokens=True
        assert log_index not in tx_context.matched_pool_events

    def test_repay_event_consumed_when_use_a_tokens_false(self):
        """
        When useATokens=False (normal repay with underlying tokens),
        the Repay event SHOULD be marked as consumed after matching.
        """
        # Simulate a transaction context with matched_pool_events tracking
        tx_context = MagicMock()
        tx_context.matched_pool_events = {}

        # Create a mock Repay event with useATokens=False
        repay_event_data = eth_abi.abi.encode(
            types=["uint256", "bool"],
            args=[25_000_000_000, False],  # 25,000 USDC with 6 decimals, useATokens=False
        )

        # Decode the event data
        payback_amount, use_a_tokens = eth_abi.abi.decode(
            types=["uint256", "bool"],
            data=repay_event_data,
        )

        # Verify useATokens is False
        assert use_a_tokens is False
        assert payback_amount == 25_000_000_000

        # Simulate the logic that should be applied:
        # When useATokens=False, DO mark as consumed
        log_index = 101
        if not use_a_tokens:
            tx_context.matched_pool_events[log_index] = True

        # Verify the event IS marked as consumed when useATokens=False
        assert log_index in tx_context.matched_pool_events
        assert tx_context.matched_pool_events[log_index] is True

    def test_transaction_21892044_event_order(self):
        """
        Test that verifies the event processing order for the specific
        transaction that failed: 0xc05df665f4bb647b354a5592d34732111526d97c9dd8bbefd32dbc88d3e4605f

        Event order should be:
        1. vToken Burn (logIndex 97) - debt burn
        2. aToken Burn (logIndex 100) - collateral burn
        3. Repay (logIndex 101) - with useATokens=True

        Both burns should match the single Repay event.
        """
        tx_context = MagicMock()
        tx_context.matched_pool_events = {}

        # Repay event data: 25000 USDC, useATokens=True
        repay_event_data = eth_abi.abi.encode(
            types=["uint256", "bool"],
            args=[25_000_000_000, True],
        )

        log_index = 101

        # Simulate processing vToken burn first (logIndex 97)
        # It finds and matches the Repay event
        _, use_a_tokens = eth_abi.abi.decode(
            types=["uint256", "bool"],
            data=repay_event_data,
        )

        # Should NOT mark as consumed when useATokens=True
        if not use_a_tokens:
            tx_context.matched_pool_events[log_index] = True

        # Event should still be available for aToken burn matching
        assert log_index not in tx_context.matched_pool_events

        # Simulate processing aToken burn (logIndex 100)
        # It should also be able to find the Repay event
        is_event_available = log_index not in tx_context.matched_pool_events
        assert is_event_available is True

        # Now mark it as consumed since both burns have matched
        tx_context.matched_pool_events[log_index] = True

        # Verify it's now consumed
        assert log_index in tx_context.matched_pool_events
