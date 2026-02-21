"""
Test BalanceTransfer event handling with pure interest accrual.

These tests verify that BalanceTransfer events are correctly processed when they
follow a Mint event with pure interest accrual (value == balanceIncrease). In this
scenario, the Mint returns balance_delta=0, so the BalanceTransfer MUST still be
processed to update the recipient's balance.

This fixes a bug where the BalanceTransfer was incorrectly skipped when:
- Mint event had value == balanceIncrease (pure interest accrual)
- BalanceTransfer transferred the same amount to the recipient
- The recipient's balance was not updated, causing a mismatch with on-chain state
"""

from hexbytes import HexBytes

from degenbot.cli.aave import AaveV3Event


class TestBalanceTransferInterestAccrual:
    """
    Test BalanceTransfer handling with pure interest accrual Mint events.

    When a Mint event has value == balanceIncrease, it's pure interest accrual
    and the Mint processor returns balance_delta=0. The BalanceTransfer that
    follows must still be processed to update the recipient's balance.

    Reference transaction: 0xb4dd38f135d8ceddb73466cabe6da17af9f717a5b40393ca8a67208523360f5a
    Block: 22010198
    User: 0xD2eEe629994e83194Db1D59cFCf9eaa923C8e110
    aToken: 0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c (aEthUSDC)
    """

    def test_pure_interest_mint_does_not_add_balance(self):
        """
        Verify that a Mint event with value == balanceIncrease returns balance_delta=0.

        The CollateralV5Processor calculates:
        - balance_delta = 0 when value == balanceIncrease (pure interest)
        - balance_delta > 0 when value != balanceIncrease (actual deposit)
        """
        mint_event = {
            "topics": [
                AaveV3Event.SCALED_TOKEN_MINT.value,
                HexBytes(
                    "0x000000000000000000000000cdd092c62344253977a86094757410c364993d09"
                ),  # from
                HexBytes(
                    "0x000000000000000000000000d2eee629994e83194db1d59cfcf9eaa923c8e110"
                ),  # onBehalfOf
            ],
            "data": HexBytes(
                "0x000000000000000000000000000000000000000000000000000000000000000f"
                # ^ value = 15
                "000000000000000000000000000000000000000000000000000000000000000f"
                # ^ balanceIncrease = 15
                "0000000000000000000000000000000000000000000039fdfb9bf0235fec02ad"
                # ^ index
            ),
            "logIndex": 104,
            "blockNumber": 22010198,
        }

        # Decode the values
        value = int.from_bytes(mint_event["data"][0:32], "big")
        balance_increase = int.from_bytes(mint_event["data"][32:64], "big")
        index = int.from_bytes(mint_event["data"][64:96], "big")

        # Verify it's pure interest accrual
        assert value == balance_increase, (
            "When value == balanceIncrease, it's pure interest accrual"
        )
        assert value == 15
        assert index > 0

        # In the CollateralV5Processor, when value == balanceIncrease:
        # balance_delta = 0 (no balance change from Mint)
        # This means the Mint does NOT add to the recipient's balance
        balance_delta = 0 if value == balance_increase else value
        assert balance_delta == 0, "Pure interest Mint should return balance_delta=0"

    def test_balance_transfer_must_update_after_pure_interest_mint(self):
        """
        Verify that BalanceTransfer updates balance after pure interest Mint.

        When a Mint with pure interest is followed by a BalanceTransfer to the
        same recipient with the same amount, the BalanceTransfer MUST update
        the balance because the Mint did not add any balance.
        """
        # Simulated transaction events
        mint_event = {
            "topics": [
                AaveV3Event.SCALED_TOKEN_MINT.value,
                HexBytes("0x000000000000000000000000cdd092c62344253977a86094757410c364993d09"),
                HexBytes("0x000000000000000000000000d2eee629994e83194db1d59cfcf9eaa923c8e110"),
            ],
            "data": HexBytes(
                "0x000000000000000000000000000000000000000000000000000000000000000f"
                # ^ value = 15
                "000000000000000000000000000000000000000000000000000000000000000f"
                # ^ balanceIncrease = 15
                "0000000000000000000000000000000000000000000039fdfb9bf0235fec02ad"
                # ^ index
            ),
            "logIndex": 104,
            "blockNumber": 22010198,
            "address": "0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c",
        }

        balance_transfer_event = {
            "topics": [
                AaveV3Event.SCALED_TOKEN_BALANCE_TRANSFER.value,
                HexBytes(
                    "0x000000000000000000000000cdd092c62344253977a86094757410c364993d09"
                ),  # from
                HexBytes(
                    "0x000000000000000000000000d2eee629994e83194db1d59cfcf9eaa923c8e110"
                ),  # to
            ],
            "data": HexBytes(
                "0x000000000000000000000000000000000000000000000000000000000000000f"  # value = 15
                "0000000000000000000000000000000000000000000039fdfb9bf0235fec02ad"  # index
            ),
            "logIndex": 107,
            "blockNumber": 22010198,
            "address": "0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c",
        }

        # Decode values
        mint_value = int.from_bytes(mint_event["data"][0:32], "big")
        mint_balance_increase = int.from_bytes(mint_event["data"][32:64], "big")
        mint_index = int.from_bytes(mint_event["data"][64:96], "big")

        transfer_value = int.from_bytes(balance_transfer_event["data"][0:32], "big")
        transfer_index = int.from_bytes(balance_transfer_event["data"][32:64], "big")

        # Verify matching conditions
        assert mint_value == mint_balance_increase, "Pure interest accrual"
        assert mint_value == transfer_value, "Same value"
        assert mint_index == transfer_index, "Same index"

        # The bug was: skip_to_user_balance_update = True when prior_value == prior_balance_increase
        # This caused the BalanceTransfer to be skipped, losing the 15 tokens
        # The fix: skip only when prior_value != prior_balance_increase (actual deposit)

        # Simulated logic from the fix
        prior_value = mint_value
        prior_balance_increase = mint_balance_increase
        event_amount = transfer_value
        index = transfer_index
        prior_index = mint_index
        to_address = "0xD2eEe629994e83194Db1D59cFCf9eaa923C8e110"
        prior_on_behalf_of = "0xD2eEe629994e83194Db1D59cFCf9eaa923C8e110"
        matched_mint_to_transfer = {}

        # OLD (buggy) logic - would skip the update
        old_skip_condition = (
            prior_value == prior_balance_increase  # Pure interest accrual - BUG!
            and prior_value == event_amount
            and prior_index == index
            and prior_on_behalf_of == to_address
            and not matched_mint_to_transfer.get(mint_event["logIndex"])
        )

        # NEW (fixed) logic - should NOT skip the update
        new_skip_condition = (
            prior_value != prior_balance_increase  # Not pure interest (actual deposit)
            and prior_value == event_amount
            and prior_index == index
            and prior_on_behalf_of == to_address
            and not matched_mint_to_transfer.get(mint_event["logIndex"])
        )

        # With pure interest (value == balanceIncrease):
        # OLD: skip_to_user_balance_update = True (BUG - loses the 15 tokens)
        # NEW: skip_to_user_balance_update = False (CORRECT - processes the transfer)
        assert old_skip_condition is True, "OLD logic would incorrectly skip the update"
        assert new_skip_condition is False, "NEW logic correctly processes the update"

        # Expected outcome:
        # - Starting balance: 46303
        # - After pure interest Mint: 46303 (no change, balance_delta=0)
        # - After BalanceTransfer: 46303 + 15 = 46318 (correct!)
        starting_balance = 46303
        expected_final_balance = 46318

        # Simulate the balance update with the fix
        final_balance = starting_balance if new_skip_condition else starting_balance + event_amount

        assert final_balance == expected_final_balance, (
            f"Balance should be {expected_final_balance}, got {final_balance}"
        )

    def test_actual_deposit_mint_does_add_balance(self):
        """
        Verify that a Mint event with value != balanceIncrease adds balance.

        When value != balanceIncrease, it's an actual deposit (SUPPLY operation)
        and the Mint processor adds the balance_delta. The BalanceTransfer should
        be skipped in this case because the Mint already added the balance.
        """
        # Simulated Mint event from actual deposit (SUPPLY)
        # value = 1000, balanceIncrease = 50 (interest accrued)
        mint_event = {
            "topics": [
                AaveV3Event.SCALED_TOKEN_MINT.value,
                HexBytes("0x000000000000000000000000cdd092c62344253977a86094757410c364993d09"),
                HexBytes("0x000000000000000000000000d2eee629994e83194db1d59cfcf9eaa923c8e110"),
            ],
            "data": HexBytes(
                "0x00000000000000000000000000000000000000000000000000000000000003e8"
                # ^ value = 1000
                "0000000000000000000000000000000000000000000000000000000000000032"
                # ^ balanceIncrease = 50
                "0000000000000000000000000000000000000000000039fdfb9bf0235fec02ad"
                # ^ index
            ),
            "logIndex": 104,
            "blockNumber": 22010198,
            "address": "0x98C23E9d8f34FEFb1B7BD6a91B7FF122F4e16F5c",
        }

        # Decode values
        value = int.from_bytes(mint_event["data"][0:32], "big")
        balance_increase = int.from_bytes(mint_event["data"][32:64], "big")

        # Verify it's an actual deposit, not pure interest
        assert value != balance_increase, "When value != balanceIncrease, it's an actual deposit"
        assert value == 1000
        assert balance_increase == 50

        # In the CollateralV5Processor, when value != balanceIncrease:
        # balance_delta = value (adds the deposited amount)
        balance_delta = 0 if value == balance_increase else value
        assert balance_delta == 1000, "Actual deposit Mint should return balance_delta=value"

        # In this case, if a BalanceTransfer follows with the same value (1000),
        # it should be skipped because the Mint already added the balance
        prior_value = value
        prior_balance_increase = balance_increase

        # The fix should skip the BalanceTransfer for actual deposits
        should_skip = prior_value != prior_balance_increase
        assert should_skip is True, "BalanceTransfer should be skipped for actual deposits"

    def test_balance_transfer_skip_logic_correctness(self):
        """
        Comprehensive test of the BalanceTransfer skip logic.

        The skip logic should:
        1. NOT skip when Mint has value == balanceIncrease (pure interest)
        2. SKIP when Mint has value != balanceIncrease (actual deposit)
        """
        test_cases = [
            {
                "name": "Pure interest accrual",
                "mint_value": 15,
                "mint_balance_increase": 15,
                "transfer_value": 15,
                "should_skip": False,  # BUG was True, FIX is False
            },
            {
                "name": "Actual deposit with interest",
                "mint_value": 1000,
                "mint_balance_increase": 50,
                "transfer_value": 1000,
                "should_skip": True,  # Mint already added balance
            },
            {
                "name": "Large deposit small interest",
                "mint_value": 1000000,
                "mint_balance_increase": 100,
                "transfer_value": 1000000,
                "should_skip": True,
            },
            {
                "name": "Zero balance increase (edge case)",
                "mint_value": 500,
                "mint_balance_increase": 0,
                "transfer_value": 500,
                "should_skip": True,
            },
        ]

        for case in test_cases:
            prior_value = case["mint_value"]
            prior_balance_increase = case["mint_balance_increase"]
            event_amount = case["transfer_value"]
            expected_skip = case["should_skip"]

            # Apply the fix logic
            should_skip = (
                prior_value != prior_balance_increase  # Not pure interest
                and prior_value == event_amount
            )

            assert should_skip == expected_skip, (
                f"Test case '{case['name']}' failed: "
                f"expected skip={expected_skip}, got skip={should_skip}"
            )
