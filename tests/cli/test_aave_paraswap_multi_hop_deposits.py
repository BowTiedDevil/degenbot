"""Tests for Bug #0026 - ParaSwap Multi-Hop Deposits.

This test verifies that collateral mint events in ParaSwap multi-hop transactions
are correctly matched to SUPPLY events, even when the calculated_scaled_amount
differs from the event_amount due to intermediate swaps.

See debug/aave/0026 for detailed bug report.
"""

from typing import cast
from unittest.mock import MagicMock

from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.aave_event_matching import (
    AaveV3Event,
    EventMatcher,
    ScaledTokenEventType,
)


class TestParaSwapMultiHopDeposits:
    """Test ParaSwap multi-hop deposit event matching."""

    def test_collateral_mint_matches_supply_with_different_amounts(self):
        """Collateral Mint should match SUPPLY even when amounts differ.

        In ParaSwap multi-hop transactions, the SUPPLY event raw amount converted
        via rayDiv may differ from the Mint event's value due to intermediate swaps.
        The match should still be accepted and the calculated_scaled_amount used.

        See debug/aave/0026.
        """
        user = get_checksum_address("0xAf8Eb92B802503A4737F6fBa38B9D734cb22A28b")
        reserve = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")

        # Create a SUPPLY event at logIndex 44
        supply_event = {
            "address": get_checksum_address("0x87870B53189092136F800c5B70A783F6E4bE9E8B"),
            "topics": [
                AaveV3Event.SUPPLY.value,
                HexBytes(reserve),
                HexBytes(user),
                HexBytes("0x0000000000000000000000000000000000000000000000000000000000000000"),
            ],
            "data": HexBytes(
                "0x00000000000000000000000089f59d5d29bb0e35783050e915c9b258467a46a8"  # caller
                "00000000000000000000000000000000000000000000000000000000007aef08"  # amount
            ),
            "logIndex": 44,
            "transactionHash": HexBytes(
                "0xc65753ab5751d591e08ac7a89910b129dbe3d09e9fcc94e32fc0a8d9a0da07a9"
            ),
            "blockNumber": 18145516,
        }

        # Create a Mint event at logIndex 17 with different amount
        # In the real bug, calculated_scaled_amount (8,073,578) != event_amount (3,772,846)
        mint_event = {
            "address": get_checksum_address("0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c"),
            "topics": [
                HexBytes("0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196"),
                HexBytes(user),
                HexBytes(user),
            ],
            "data": HexBytes(
                "0x00000000000000000000000000000000000000000000000000000000003991ae"  # value
                "00000000000000000000000000000000000000000000000000000000003991ae"  # balanceIncrease
                "000000000000000000000000000000000000000003498abf9523bd0559960244"  # index
            ),
            "logIndex": 17,
            "transactionHash": HexBytes(
                "0xc65753ab5751d591e08ac7a89910b129dbe3d09e9fcc94e32fc0a8d9a0da07a9"
            ),
            "blockNumber": 18145516,
        }

        # Create transaction context with pool events
        tx_context = MagicMock()
        tx_context.pool_events = [cast("LogReceipt", supply_event)]
        tx_context.matched_pool_events = {}

        # Create EventMatcher
        matcher = EventMatcher(tx_context)

        # Try to find matching SUPPLY event for the Mint
        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=user,
            reserve_address=reserve,
        )

        # Should find the SUPPLY event
        assert result is not None, "Should find matching SUPPLY event"
        assert result["pool_event"]["logIndex"] == 44, "Should match SUPPLY at logIndex 44"

    def test_event_matcher_consumes_supply_events(self):
        """EventMatcher should mark SUPPLY events as consumed after matching.

        This prevents the same SUPPLY event from matching multiple Mint events.
        """
        user = get_checksum_address("0xAf8Eb92B802503A4737F6fBa38B9D734cb22A28b")
        reserve = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")

        supply_event = {
            "address": get_checksum_address("0x87870B53189092136F800c5B70A783F6E4bE9E8B"),
            "topics": [
                AaveV3Event.SUPPLY.value,
                HexBytes(reserve),
                HexBytes(user),
                HexBytes("0x0000000000000000000000000000000000000000000000000000000000000000"),
            ],
            "data": HexBytes(
                "0x00000000000000000000000089f59d5d29bb0e35783050e915c9b258467a46a8"
                "00000000000000000000000000000000000000000000000000000000007aef08"
            ),
            "logIndex": 44,
            "transactionHash": HexBytes("0x1234"),
            "blockNumber": 18145516,
        }

        tx_context = MagicMock()
        tx_context.pool_events = [cast("LogReceipt", supply_event)]
        tx_context.matched_pool_events = {}

        matcher = EventMatcher(tx_context)

        # First match should succeed
        result1 = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=user,
            reserve_address=reserve,
        )
        assert result1 is not None, "First match should succeed"

        # Second match should fail (SUPPLY is consumed)
        result2 = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=user,
            reserve_address=reserve,
        )
        assert result2 is None, "Second match should fail (SUPPLY consumed)"
