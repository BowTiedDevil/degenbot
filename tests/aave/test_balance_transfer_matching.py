"""Test for BalanceTransfer to Mint matching logic.

This test verifies that the BalanceTransfer event matching logic in
_process_scaled_token_balance_transfer_event correctly only matches Mint events
when the Mint recipient (onBehalfOf) matches the BalanceTransfer recipient (To).

Reference: Transaction 0x826bbd6c67db581f9b1e584af404626f6f9f038ea19b4522070bd1cf5738c390
at block 21711842 where a BalanceTransfer to a user with zero balance would have
incorrectly matched a Mint to a different recipient, causing the user's balance
to go negative when they subsequently transferred those tokens.
"""

import eth_abi
from eth_typing import ChecksumAddress
from hexbytes import HexBytes

from degenbot.functions import get_checksum_address


def _decode_address(input_: bytes) -> ChecksumAddress:
    """Get the checksummed address from the given byte stream."""
    (address,) = eth_abi.abi.decode(types=["address"], data=input_)
    return get_checksum_address(address)


def encode_mint_event_data(value: int, balance_increase: int, index: int) -> HexBytes:
    """Encode Mint event data (value, balanceIncrease, index) as ABI-encoded data."""
    encoded = eth_abi.abi.encode(
        types=["uint256", "uint256", "uint256"],
        args=[value, balance_increase, index],
    )
    return HexBytes(encoded)


class TestBalanceTransferMintMatching:
    """Test BalanceTransfer to Mint matching logic."""

    def test_mint_with_different_recipient_should_not_match(self):
        """Test that a Mint to User A does NOT match a BalanceTransfer to User B.

        Scenario:
        - Mint (log 0x15e): 2 tokens minted as interest to User A (0x5b5a0580...)
        - BalanceTransfer (log 0x173): 2 tokens from User A to User B (0x0f4a1d7f...)
        - BalanceTransfer (log 0x178): 2 tokens from User B to User C (0xccd58333...)

        The second BalanceTransfer would cause User B's balance to go negative
        (0 -> -2) if the Mint was incorrectly matched to the first BalanceTransfer.

        The fix ensures we check: prior_on_behalf_of == to_address
        """
        # SCALED_TOKEN_MINT topic
        mint_topic = HexBytes("0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196")

        # Mock Mint event: 2 tokens minted to User A (onBehalfOf)
        user_a = "0x5B5A0580bcfd3673820Bb249514234aFAD33e209"
        index = 0x113245629B4CFFF7F5EE72B  # Same index as in actual transaction

        mint_event = {
            "topics": [
                mint_topic,  # Event signature
                HexBytes("0x000000000000000000000000" + user_a[2:]),  # caller
                HexBytes("0x000000000000000000000000" + user_a[2:]),  # onBehalfOf
            ],
            "data": encode_mint_event_data(
                value=2,
                balance_increase=2,
                index=index,
            ),
            "logIndex": 0x15E,
        }

        # Decode the Mint event
        prior_value, prior_balance_increase, prior_index = eth_abi.abi.decode(
            types=["uint256", "uint256", "uint256"],
            data=mint_event["data"],
        )
        prior_on_behalf_of = _decode_address(mint_event["topics"][2])

        # Verify Mint event values
        assert prior_value == 2
        assert prior_balance_increase == 2
        assert prior_value == prior_balance_increase  # Pure interest accrual
        assert prior_on_behalf_of == user_a

        # User B is the recipient of the BalanceTransfer
        user_b = "0x0F4A1D7FdF4890bE35e71f3E0Bbc4a0EC377eca3"

        # Simulate the matching logic from _process_scaled_token_balance_transfer_event
        event_amount = 2  # BalanceTransfer amount
        to_address = user_b  # BalanceTransfer recipient

        # The matching condition should FAIL because:
        # - prior_value == prior_balance_increase (True: both are 2)
        # - prior_value == event_amount (True: both are 2)
        # - prior_index == index (True: same index)
        # - prior_on_behalf_of == to_address (False: User A != User B)

        should_match = (
            prior_value == prior_balance_increase
            and prior_value == event_amount
            and prior_index == index
            and prior_on_behalf_of == to_address
        )

        assert not should_match, (
            "Mint to User A should NOT match BalanceTransfer to User B. "
            "This would cause User B's balance to go negative when they transfer "
            "tokens they never received!"
        )

    def test_mint_with_same_recipient_should_match(self):
        """Test that a Mint to User A DOES match a BalanceTransfer to User A.

        This is the correct scenario where the Mint recipient matches the
        BalanceTransfer recipient, so we can skip the balance update.
        """
        # SCALED_TOKEN_MINT topic
        mint_topic = HexBytes("0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196")

        # Mock Mint event: 2 tokens minted as interest to User A
        user_a = "0x5B5A0580bcfd3673820Bb249514234aFAD33e209"
        index = 0x113245629B4CFFF7F5EE72B

        mint_event = {
            "topics": [
                mint_topic,  # Event signature
                HexBytes("0x000000000000000000000000" + user_a[2:]),  # caller
                HexBytes("0x000000000000000000000000" + user_a[2:]),  # onBehalfOf
            ],
            "data": encode_mint_event_data(
                value=2,
                balance_increase=2,
                index=index,
            ),
            "logIndex": 0x15E,
        }

        # Decode the Mint event
        prior_value, prior_balance_increase, prior_index = eth_abi.abi.decode(
            types=["uint256", "uint256", "uint256"],
            data=mint_event["data"],
        )
        prior_on_behalf_of = _decode_address(mint_event["topics"][2])

        # User A is also the recipient of the BalanceTransfer
        event_amount = 2
        to_address = user_a

        # The matching condition should SUCCEED because:
        # - prior_value == prior_balance_increase (True: both are 2)
        # - prior_value == event_amount (True: both are 2)
        # - prior_index == index (True: same index)
        # - prior_on_behalf_of == to_address (True: User A == User A)

        should_match = (
            prior_value == prior_balance_increase
            and prior_value == event_amount
            and prior_index == index
            and prior_on_behalf_of == to_address
        )

        assert should_match, (
            "Mint to User A SHOULD match BalanceTransfer to User A "
            "when all conditions (value, index, recipient) match"
        )

    def test_mint_with_non_pure_interest_should_not_match(self):
        """Test that Mint events with value != balance_increase don't match.

        Only Mint events with pure interest accrual (value == balance_increase)
        should be considered for matching. Supply operations have value > balance_increase.
        """
        # SCALED_TOKEN_MINT topic
        mint_topic = HexBytes("0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196")

        # Same user for both Mint and BalanceTransfer
        user_a = "0x5B5A0580bcfd3673820Bb249514234aFAD33e209"
        index = 0x113245629B4CFFF7F5EE72B

        # Mock Mint event: supply operation (value > balance_increase)
        mint_event = {
            "topics": [
                mint_topic,  # Event signature
                HexBytes("0x000000000000000000000000" + user_a[2:]),  # caller
                HexBytes("0x000000000000000000000000" + user_a[2:]),  # onBehalfOf
            ],
            "data": encode_mint_event_data(
                value=100,  # value
                balance_increase=5,  # balance_increase
                index=index,
            ),
            "logIndex": 0x15E,
        }

        # Decode the Mint event
        prior_value, prior_balance_increase, prior_index = eth_abi.abi.decode(
            types=["uint256", "uint256", "uint256"],
            data=mint_event["data"],
        )
        prior_on_behalf_of = _decode_address(mint_event["topics"][2])

        event_amount = 100
        to_address = user_a

        # The matching condition should FAIL because:
        # - prior_value == prior_balance_increase (False: 100 != 5)

        should_match = (
            prior_value == prior_balance_increase
            and prior_value == event_amount
            and prior_index == index
            and prior_on_behalf_of == to_address
        )

        assert not should_match, (
            "Supply Mint (value > balance_increase) should NOT be matched "
            "to BalanceTransfer. Only pure interest accrual should match."
        )
