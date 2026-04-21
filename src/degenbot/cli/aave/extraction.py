"""
User address extraction from Aave V3 events.

This module provides functions to extract user addresses from Aave event logs,
used for batch prefetching users to avoid N+1 queries during transaction processing.
"""

from typing import assert_never

import eth_abi.abi
from eth_typing import ChecksumAddress
from web3.types import LogReceipt

from degenbot.aave.events import (
    AaveV3GhoDebtTokenEvent,
    AaveV3OracleEvent,
    AaveV3PoolConfigEvent,
    AaveV3PoolEvent,
    AaveV3ScaledTokenEvent,
    AaveV3StkAaveEvent,
    ERC20Event,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.aave_utils import decode_address
from degenbot.constants import ZERO_ADDRESS


def extract_user_addresses_from_transaction(events: list[LogReceipt]) -> set[ChecksumAddress]:
    """
    Extract all unique user addresses from a list of transaction events.

    This is used for batch prefetching users to avoid N+1 queries during
    transaction processing.
    """
    return {address for event in events for address in extract_user_addresses_from_event(event)}


def extract_user_addresses_from_event(event: LogReceipt) -> set[ChecksumAddress]:
    """
    Extract user addresses from an Aave event.

    Returns a set of all user addresses (senders, recipients, onBehalfOf, etc.)
    that are involved in the event.
    """

    user_addresses: set[ChecksumAddress] = set()
    topic = event["topics"][0]

    if topic == AaveV3ScaledTokenEvent.MINT.value:
        """
        Event definition:
            event Mint(
                address indexed caller,
                address indexed onBehalfOf,
                uint256 value,
                uint256 balanceIncrease,
                uint256 index
                );
        """

        user_addresses.add(decode_address(event["topics"][1]))
        user_addresses.add(decode_address(event["topics"][2]))

    elif topic == AaveV3ScaledTokenEvent.BURN.value:
        """
        Event definition:
            event Burn(
                address indexed from,
                address indexed target,
                uint256 value,
                uint256 balanceIncrease,
                uint256 index
            );
        """

        user_addresses.add(decode_address(event["topics"][1]))
        user_addresses.add(decode_address(event["topics"][2]))

    elif topic == AaveV3ScaledTokenEvent.BALANCE_TRANSFER.value:
        """
        Event definition:
            event BalanceTransfer(
                address indexed from,
                address indexed to,
                uint256 value,
                uint256 index
            );
        """

        user_addresses.add(decode_address(event["topics"][1]))
        user_addresses.add(decode_address(event["topics"][2]))

    elif topic == ERC20Event.TRANSFER.value:
        """
        Event definition:
            event Transfer(
                address indexed from,
                address indexed to,
                uint256 value
            );
        """

        from_addr = decode_address(event["topics"][1])
        to_addr = decode_address(event["topics"][2])
        if from_addr != ZERO_ADDRESS:
            user_addresses.add(from_addr)
        if to_addr != ZERO_ADDRESS:
            user_addresses.add(to_addr)

    elif topic in {
        AaveV3GhoDebtTokenEvent.DISCOUNT_PERCENT_UPDATED.value,
        AaveV3PoolEvent.USER_E_MODE_SET.value,
        AaveV3PoolEvent.DEFICIT_CREATED.value,
    }:
        """
        Event definitions:
            event DiscountPercentUpdated(
                address indexed user,
                uint256 oldDiscountPercent,
                uint256 indexed newDiscountPercent
            );
            event UserEModeSet(
                address indexed user,
                uint8 categoryId
            );
            event DeficitCreated(
                address indexed user,
                address indexed debtAsset,
                uint256 amountCreated
            );
        """

        user_addresses.add(decode_address(event["topics"][1]))

    elif topic in {
        AaveV3PoolEvent.BORROW.value,
        AaveV3PoolEvent.REPAY.value,
        AaveV3PoolEvent.SUPPLY.value,
        AaveV3PoolEvent.WITHDRAW.value,
    }:
        """
        Event definitions:
            event Borrow(
                address indexed reserve,
                address user,
                address indexed onBehalfOf,
                uint256 amount,
                DataTypes.InterestRateMode interestRateMode,
                uint256 borrowRate,
                uint16 indexed referralCode
            );
            event Repay(
                address indexed reserve,
                address indexed user,
                address indexed repayer,
                uint256 amount,
                bool useATokens
            );
            event Supply(
                address indexed reserve,
                address user,
                address indexed onBehalfOf,
                uint256 amount,
                uint16 indexed referralCode
            );
            event Withdraw(
                address indexed reserve,
                address indexed user,
                address indexed to,
                uint256 amount
            );
        """

        user_addresses.add(decode_address(event["topics"][2]))

    elif topic == AaveV3PoolEvent.LIQUIDATION_CALL.value:
        """
        Event definition:
            event LiquidationCall(
                address indexed collateralAsset,
                address indexed debtAsset,
                address indexed user,
                uint256 debtToCover,
                uint256 liquidatedCollateralAmount,
                address liquidator,
                bool receiveAToken
            );
        """

        user_addresses.add(decode_address(event["topics"][3]))
        liquidator: str
        receive_a_token: bool
        _, _, liquidator, receive_a_token = eth_abi.abi.decode(
            types=["uint256", "uint256", "address", "bool"],
            data=event["data"],
        )
        if receive_a_token:
            user_addresses.add(get_checksum_address(liquidator))

    elif topic in {
        AaveV3StkAaveEvent.STAKED.value,
        AaveV3StkAaveEvent.REDEEM.value,
    }:
        """
        Event definitions:
            event Staked(
                address indexed from,
                address indexed onBehalfOf,
                uint256 amount
            );
            event Redeem(
                address indexed from,
                address indexed to,
                uint256 amount
            );
        """

        user_addresses.add(decode_address(event["topics"][1]))
        user_addresses.add(decode_address(event["topics"][2]))

    elif topic in {
        AaveV3GhoDebtTokenEvent.DISCOUNT_RATE_STRATEGY_UPDATED.value,
        AaveV3GhoDebtTokenEvent.DISCOUNT_TOKEN_UPDATED.value,
        AaveV3OracleEvent.ASSET_SOURCE_UPDATED.value,
        AaveV3PoolConfigEvent.ADDRESS_SET.value,
        AaveV3PoolConfigEvent.POOL_CONFIGURATOR_UPDATED.value,
        AaveV3PoolConfigEvent.POOL_DATA_PROVIDER_UPDATED.value,
        AaveV3PoolConfigEvent.POOL_UPDATED.value,
        AaveV3PoolConfigEvent.PRICE_ORACLE_UPDATED.value,
        AaveV3PoolConfigEvent.UPGRADED.value,
        AaveV3PoolEvent.MINTED_TO_TREASURY.value,
        AaveV3PoolEvent.RESERVE_DATA_UPDATED.value,
        AaveV3PoolEvent.RESERVE_USED_AS_COLLATERAL_DISABLED.value,
        AaveV3PoolEvent.RESERVE_USED_AS_COLLATERAL_ENABLED.value,
    }:
        # no relevant user data in these events
        pass

    else:
        assert_never(topic)

    return user_addresses
