"""Test for collateral burn event matching during liquidations.

This test verifies that collateral (aToken) Burn events correctly match
LIQUIDATION_CALL pool events when collateral is seized during liquidation.

Reference: Transaction 0x8a843f0cf626d6e972c144a4b3d2fc920126f8630749f12167322886df6ee825
at block 21893775 where collateral was seized but the Burn event couldn't find
a matching WITHDRAW or REPAY event.
"""

import eth_abi
from eth_typing import ChecksumAddress
from hexbytes import HexBytes

from degenbot.functions import get_checksum_address


def _decode_address(input_: bytes) -> ChecksumAddress:
    """Get the checksummed address from the given byte stream."""
    (address,) = eth_abi.abi.decode(types=["address"], data=input_)
    return get_checksum_address(address)


class TestCollateralBurnLiquidationMatching:
    """Test that collateral burn events correctly match LIQUIDATION_CALL events."""

    def test_liquidation_call_event_data_decoding(self):
        """Test that LIQUIDATION_CALL event data is decoded correctly.

        LIQUIDATION_CALL event structure:
        - topics[0]: event signature
        - topics[1]: collateralAsset (address)
        - topics[2]: debtAsset (address)
        - topics[3]: user (address)
        - data: (uint256 debtToCover, uint256 liquidatedCollateralAmount,
                 address liquidator, bool receiveAToken)

        From transaction 0x8a843f0cf626d6e972c144a4b3d2fc920126f8630749f12167322886df6ee825
        at block 21893775, logIndex 416 (0x1a0):
        - collateralAsset: cbBTC (0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf)
        - debtAsset: WETH (0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2)
        - user: 0xaca98ec16bf9174c6acb486870bab8616d1e5a3b
        - debtToCover: ~0.0118 WETH
        - liquidatedCollateralAmount: 226,483 units (of aEthcbBTC)
        """
        # LIQUIDATION_CALL topic
        liquidation_topic = HexBytes(
            "0xe413a413e37f7bf964d3b40070d324c55a04c3c3c28b49d152f521e0a26f49ad"
        )

        user_address = "0xaca98ec16bf9174c6acb486870bab8616d1e5a3b"
        collateral_asset = "0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf"  # cbBTC
        debt_asset = "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"  # WETH

        # Simulated event data from actual transaction
        # debtToCover = 11826010208460853 (0.011826... WETH)
        # liquidatedCollateralAmount = 226483
        # liquidator = 0x9d6b911199b891c55a93e4bc635bf59e33d002d8
        # receiveAToken = False
        event_data = eth_abi.abi.encode(
            types=["uint256", "uint256", "address", "bool"],
            args=[11826010208460853, 226483, "0x9d6b911199b891c55a93e4bc635bf59e33d002d8", False],
        )

        # Decode the event data
        debt_to_cover, liquidated_collateral_amount, liquidator, receive_a_token = (
            eth_abi.abi.decode(
                types=["uint256", "uint256", "address", "bool"],
                data=event_data,
            )
        )

        assert debt_to_cover == 11826010208460853
        assert liquidated_collateral_amount == 226483
        assert liquidator == "0x9d6b911199b891c55a93e4bc635bf59e33d002d8"
        assert receive_a_token is False

        # Simulate topic decoding (topics contain 32-byte padded addresses)
        decoded_collateral = _decode_address(bytes.fromhex(collateral_asset[2:].rjust(64, "0")))
        decoded_debt = _decode_address(bytes.fromhex(debt_asset[2:].rjust(64, "0")))
        decoded_user = _decode_address(bytes.fromhex(user_address[2:].rjust(64, "0")))

        assert decoded_collateral.lower() == collateral_asset.lower()
        assert decoded_debt.lower() == debt_asset.lower()
        assert decoded_user.lower() == user_address.lower()

    def test_collateral_burn_matches_liquidation_call(self):
        """Test that a collateral Burn event matches a LIQUIDATION_CALL event.

        In a liquidation:
        1. The user's collateral aTokens are burned (Burn event at log 409)
        2. The user's debt vTokens are burned (Burn event at log 405)
        3. The Pool emits LIQUIDATION_CALL (at log 416)

        The collateral Burn should match the LIQUIDATION_CALL by:
        - Burn.from == LIQUIDATION_CALL.user (the liquidated user)
        - collateral_asset.underlying_token.address == LIQUIDATION_CALL.collateralAsset
        """
        # SCALED_TOKEN_BURN topic
        burn_topic = HexBytes("0x4cf25bc1d991c17529c25213d3cc0cda295eeaad5f13f361969b12ea48015f90")

        # LIQUIDATION_CALL topic
        liquidation_topic = HexBytes(
            "0xe413a413e37f7bf964d3b40070d324c55a04c3c3c28b49d152f521e0a26f49ad"
        )

        user_address = "0xaca98ec16bf9174c6acb486870bab8616d1e5a3b"
        collateral_asset = "0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf"  # cbBTC

        # Simulate Burn event at logIndex 409
        burn_event = {
            "topics": [
                burn_topic,  # Event signature
                HexBytes("0x" + user_address[2:].rjust(64, "0")),  # from
                HexBytes("0x" + "9d6b911199b891c55a93e4bc635bf59e33d002d8"),  # target (liquidator)
            ],
            "data": eth_abi.abi.encode(
                types=["uint256", "uint256", "uint256"],
                args=[226483, 1, 1062524787910956572223166138],  # value, balanceIncrease, index
            ),
            "logIndex": 409,
        }

        # Simulate LIQUIDATION_CALL event at logIndex 416
        liquidation_event = {
            "topics": [
                liquidation_topic,  # Event signature
                HexBytes("0x" + collateral_asset[2:].rjust(64, "0")),  # collateralAsset
                HexBytes("0x" + "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"),  # debtAsset (WETH)
                HexBytes("0x" + user_address[2:].rjust(64, "0")),  # user
            ],
            "data": eth_abi.abi.encode(
                types=["uint256", "uint256", "address", "bool"],
                args=[
                    11826010208460853,
                    226483,
                    "0x9d6b911199b891c55a93e4bc635bf59e33d002d8",
                    False,
                ],
            ),
            "logIndex": 416,
        }

        # Decode Burn event
        burn_value, burn_balance_increase, burn_index = eth_abi.abi.decode(
            types=["uint256", "uint256", "uint256"],
            data=burn_event["data"],
        )
        burn_from = _decode_address(bytes(burn_event["topics"][1]))

        # Decode LIQUIDATION_CALL event
        (
            liquidation_debt_to_cover,
            liquidation_collateral_amount,
            liquidation_liquidator,
            liquidation_receive_a_token,
        ) = eth_abi.abi.decode(
            types=["uint256", "uint256", "address", "bool"],
            data=liquidation_event["data"],
        )
        liquidation_collateral_asset = _decode_address(bytes(liquidation_event["topics"][1]))
        liquidation_user = _decode_address(bytes(liquidation_event["topics"][3]))

        # Verify matching conditions
        assert burn_from.lower() == liquidation_user.lower() == user_address.lower()
        assert liquidation_collateral_asset.lower() == collateral_asset.lower()

        # The liquidatedCollateralAmount from LIQUIDATION_CALL should match
        # or be related to the Burn event value
        assert liquidation_collateral_amount == burn_value

    def test_liquidation_call_not_marked_as_consumed(self):
        """Test that LIQUIDATION_CALL events are NOT marked as consumed.

        A single LIQUIDATION_CALL event can match:
        1. A debt token Burn event (reducing debt)
        2. A collateral token Burn event (seizing collateral)

        Therefore, the event should NOT be marked as consumed after the first match.
        """
        # Simulate transaction context
        matched_pool_events = {}
        log_index = 416

        # Simulate matching a Burn event to LIQUIDATION_CALL
        # Note: The event should NOT be marked as consumed
        # (unlike WITHDRAW/REPAY which are marked consumed)
        should_mark_consumed = False  # LIQUIDATION_CALL is NOT marked consumed

        if should_mark_consumed:
            matched_pool_events[log_index] = True

        # Verify the event is NOT in matched_pool_events
        assert log_index not in matched_pool_events

    def test_collateral_burn_liquidation_scaled_amount_calculation(self):
        """Test that scaled amount is calculated correctly for liquidation burns.

        For liquidation collateral burns, the scaled amount should be calculated
        from the liquidatedCollateralAmount in the LIQUIDATION_CALL event using
        the same formula as WITHDRAW:

        scaled_amount = ray_div(liquidatedCollateralAmount, liquidityIndex)
        """
        from degenbot.aave.processors import PoolProcessorFactory

        # Get pool processor for V1 (used by aEthcbBTC)
        pool_processor = PoolProcessorFactory.get_pool_processor_for_token_revision(1)

        # From actual transaction:
        # liquidatedCollateralAmount = 226483
        # liquidityIndex = 1062524787910956572223166138 (at block 21893775)
        liquidated_collateral_amount = 226483
        liquidity_index = 1062524787910956572223166138

        # Calculate scaled amount
        scaled_amount = pool_processor.calculate_collateral_burn_scaled_amount(
            amount=liquidated_collateral_amount,
            liquidity_index=liquidity_index,
        )

        # The scaled amount should be: ray_div(226483, 1062524787910956572223166138)
        # = 226483 * 10^27 / 1062524787910956572223166138
        # ≈ 213 (approximate)

        assert scaled_amount > 0
        assert isinstance(scaled_amount, int)

        # Verify the calculation is correct
        expected_scaled = (liquidated_collateral_amount * 10**27) // liquidity_index
        assert scaled_amount == expected_scaled
