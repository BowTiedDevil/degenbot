"""
User database operations for Aave V3.

Functions for querying, creating, and managing AaveV3User records.
"""

import eth_abi.exceptions
from eth_typing import ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session
from web3.exceptions import ContractLogicError

from degenbot.cli.aave.constants import GHO_DISCOUNT_DEPRECATION_REVISION
from degenbot.cli.aave.types import TransactionContext
from degenbot.database.models.aave import AaveGhoToken, AaveV3Asset, AaveV3Market, AaveV3User
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger


def get_gho_vtoken_revision(
    session: Session,
    market: AaveV3Market,
) -> int | None:
    """
    Get the GHO vToken revision for the given market.

    Queries the AaveV3Asset table to get the v_token_revision for the GHO asset.
    """

    gho_asset = session.scalar(
        select(AaveGhoToken)
        .join(AaveGhoToken.token)
        .where(
            AaveGhoToken.v_token_id.is_not(None),
        )
    )

    if gho_asset is None or gho_asset.v_token is None:
        return None

    return session.scalar(
        select(AaveV3Asset.v_token_revision)
        .join(Erc20TokenTable, AaveV3Asset.v_token_id == Erc20TokenTable.id)
        .where(
            AaveV3Asset.market_id == market.id,
            Erc20TokenTable.address == gho_asset.v_token.address,
        )
    )


def is_discount_supported(
    session: Session,
    market: AaveV3Market,
) -> bool:
    """
    Check if GHO discount mechanism is supported.
    """

    revision = get_gho_vtoken_revision(session, market)
    return revision is not None and revision < GHO_DISCOUNT_DEPRECATION_REVISION


def get_or_create_user(
    *,
    tx_context: TransactionContext,
    user_address: ChecksumAddress,
    block_number: int,
) -> AaveV3User:
    """
    Get existing user or create new one with default e_mode.

    Uses the transaction context's user_cache to avoid repeated database queries.
    New users are created on-demand and added to the cache.

    When creating a new user, if w3 and block_number are provided and the user
    has an existing GHO debt position, their discount percent will be fetched
    from the contract to properly initialize their gho_discount value.
    """

    # User not in cache - query database (this handles the edge case where
    # a user was added by a concurrent transaction or cache wasn't pre-filled)
    user = tx_context.session.scalar(
        select(AaveV3User).where(
            AaveV3User.address == user_address,
            AaveV3User.market_id == tx_context.market.id,
        )
    )

    if user is not None:
        return user

    # Create new user
    # When creating a new user, check if they have a GHO discount on-chain
    # to properly initialize their gho_discount value
    gho_discount = 0

    # Only fetch discount if mechanism is supported (revision 2 or 3)
    gho_vtoken_address = tx_context.gho_vtoken_address

    if (
        gho_vtoken_address is not None
        and tx_context.gho_asset.v_gho_discount_token is not None
        and is_discount_supported(
            session=tx_context.session,
            market=tx_context.market,
        )
    ):
        try:
            (discount_percent,) = raw_call(
                w3=tx_context.provider,
                address=gho_vtoken_address,
                calldata=encode_function_calldata(
                    function_prototype="getDiscountPercent(address)",
                    function_arguments=[user_address],
                ),
                return_types=["uint256"],
                block_identifier=block_number,
            )
            gho_discount = discount_percent
        except (
            RuntimeError,
            eth_abi.exceptions.DecodingError,
            ContractLogicError,
        ) as e:
            # If the call fails (e.g., contract not deployed yet, node error,
            # or function not found after upgrade to revision 4+), default to 0
            logger.warning(
                f"Failed to fetch GHO discount for user {user_address} at block "
                f"{block_number}: {e}. Using default 0."
            )

    # Log all user creations for debugging
    logger.debug(f"CREATING USER: {user_address} gho_discount={gho_discount} block={block_number}")

    user = AaveV3User(
        market_id=tx_context.market.id,
        address=user_address,
        e_mode=0,
        gho_discount=gho_discount,
    )
    tx_context.session.add(user)
    tx_context.session.flush()

    return user
