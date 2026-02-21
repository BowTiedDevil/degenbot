"""Test for SUPPLY event matching with zero onBehalfOf address.

Tests the fix for Bug #0022: Wrapped Token Gateway deposits where the Pool's SUPPLY
event has onBehalfOf set to zero address and user set to the actual beneficiary.

Transaction: 0xa4a5f3993fd60bd01665f8389c1c5cded8cfed0007de913142cd9a8bb0f13117
Block: 16496817
"""

from dataclasses import dataclass, field
from typing import TYPE_CHECKING

from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.cli.aave_event_matching import (
    AaveV3Event,
    EventMatcher,
    ScaledTokenEventType,
)

if TYPE_CHECKING:
    from collections.abc import Sequence


@dataclass
class MockTransactionContext:
    """Mock transaction context for testing."""

    events: "Sequence[LogReceipt]" = field(default_factory=list)
    pool_events: "Sequence[LogReceipt]" = field(default_factory=list)
    matched_pool_events: dict[int, bool] = field(default_factory=dict)


def create_supply_event(
    *,
    reserve: ChecksumAddress,
    user: ChecksumAddress,
    on_behalf_of: ChecksumAddress,
    amount: int,
    referral_code: int = 0,
    log_index: int = 100,
) -> LogReceipt:
    """Create a SUPPLY event LogReceipt."""
    # SUPPLY: topics[1]=reserve, topics[2]=user, topics[3]=onBehalfOf
    # data: (address caller, uint256 amount, uint16 referralCode)
    from eth_abi import abi

    data = abi.encode(
        types=["address", "uint256", "uint16"],
        args=[user, amount, referral_code],
    )

    return LogReceipt({
        "address": "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2",  # Pool
        "topics": [
            AaveV3Event.SUPPLY.value,
            HexBytes("0x" + "0" * 24 + reserve[2:]),
            HexBytes("0x" + "0" * 24 + user[2:]),
            HexBytes("0x" + "0" * 24 + on_behalf_of[2:]),
        ],
        "data": data,
        "blockNumber": 16496817,
        "transactionHash": HexBytes(
            "0xa4a5f3993fd60bd01665f8389c1c5cded8cfed0007de913142cd9a8bb0f13117"
        ),
        "logIndex": log_index,
    })


class TestWrappedTokenGatewaySupply:
    """Test SUPPLY event matching for Wrapped Token Gateway deposits."""

    def test_gateway_supply_with_zero_on_behalf_of_matches_user(self):
        """
        Test that COLLATERAL_MINT matches SUPPLY when onBehalfOf is zero address.

        When using the Wrapped Token Gateway:
        - The gateway calls Pool.supply() on behalf of the user
        - Pool sets user=beneficiary, onBehalfOf=zero_address
        - The collateral mint event has onBehalfOf=beneficiary

        The matching should succeed by matching on the 'user' topic when
        onBehalfOf is the zero address.
        """
        # Addresses from the actual failing transaction
        user_address = "0x7FA5195595EFE0dFbc79f03303448af3FbE4ea91"
        gateway_address = "0xD322A49006FC828F9B5B37Ab215F99B4E5caB19C"
        reserve_address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"  # WETH
        zero_address = "0x0000000000000000000000000000000000000000"

        # Create the SUPPLY event as it appears in the transaction
        supply_event = create_supply_event(
            reserve=reserve_address,
            user=user_address,  # Actual beneficiary
            on_behalf_of=zero_address,  # Zero when caller != user
            amount=40000000000000000,  # 0.04 WETH
            log_index=6,
        )

        context = MockTransactionContext(
            pool_events=[supply_event],
        )

        matcher = EventMatcher(context)

        # Try to match as COLLATERAL_MINT with user_address
        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=user_address,
            reserve_address=reserve_address,
            check_users=[gateway_address],
            max_log_index=10,
        )

        assert result is not None, "Should find matching SUPPLY event"
        assert result["pool_event"] == supply_event
        assert result["extraction_data"]["raw_amount"] == 40000000000000000

    def test_direct_supply_with_non_zero_on_behalf_of_matches_on_behalf_of(self):
        """
        Test that standard direct supply still works when onBehalfOf != zero.

        For direct supplies:
        - User calls Pool.supply() directly
        - Pool sets user=caller, onBehalfOf=beneficiary (usually same as user)
        - The collateral mint event has onBehalfOf=beneficiary

        The matching should succeed by matching on the 'onBehalfOf' topic when
        it's not the zero address.
        """
        user_address = "0x7FA5195595EFE0dFbc79f03303448af3FbE4ea91"
        reserve_address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

        supply_event = create_supply_event(
            reserve=reserve_address,
            user=user_address,
            on_behalf_of=user_address,  # Non-zero for direct supply
            amount=1000000000000000000,  # 1 WETH
            log_index=5,
        )

        context = MockTransactionContext(
            pool_events=[supply_event],
        )

        matcher = EventMatcher(context)

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=user_address,
            reserve_address=reserve_address,
            max_log_index=10,
        )

        assert result is not None, "Should find matching SUPPLY event"
        assert result["pool_event"] == supply_event

    def test_supply_with_different_user_and_on_behalf_of(self):
        """
        Test supply where user and onBehalfOf are different (delegated supply).

        When supplying on behalf of another address:
        - Caller supplies for a different beneficiary
        - Pool sets user=beneficiary, onBehalfOf=beneficiary
        - Both topics should match the same address
        """
        user_address = "0x7FA5195595EFE0dFbc79f03303448af3FbE4ea91"
        reserve_address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"

        supply_event = create_supply_event(
            reserve=reserve_address,
            user=user_address,
            on_behalf_of=user_address,  # Same as user
            amount=500000000000000000,
            log_index=3,
        )

        context = MockTransactionContext(
            pool_events=[supply_event],
        )

        matcher = EventMatcher(context)

        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=user_address,
            reserve_address=reserve_address,
            max_log_index=10,
        )

        assert result is not None
        assert result["pool_event"] == supply_event

    def test_no_match_when_user_and_on_behalf_of_both_dont_match(self):
        """Test that match fails when neither user nor onBehalfOf matches."""
        actual_user = "0x7FA5195595EFE0dFbc79f03303448af3FbE4ea91"
        wrong_user = "0x1234567890123456789012345678901234567890"
        reserve_address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
        zero_address = "0x0000000000000000000000000000000000000000"

        supply_event = create_supply_event(
            reserve=reserve_address,
            user=actual_user,
            on_behalf_of=zero_address,
            amount=1000000000000000000,
            log_index=5,
        )

        context = MockTransactionContext(
            pool_events=[supply_event],
        )

        matcher = EventMatcher(context)

        # Try to match with wrong user
        result = matcher.find_matching_pool_event(
            event_type=ScaledTokenEventType.COLLATERAL_MINT,
            user_address=wrong_user,
            reserve_address=reserve_address,
            max_log_index=10,
        )

        assert result is None, "Should not match when user doesn't match"
