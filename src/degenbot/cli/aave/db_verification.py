"""
Verification database operations for Aave V3.

Functions for verifying on-chain state against database state.
"""

import tqdm
from eth_typing import ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session

from degenbot.cli.aave.constants import GHO_DISCOUNT_DEPRECATION_REVISION
from degenbot.cli.aave.db_users import get_gho_vtoken_revision
from degenbot.database.models.aave import (
    AaveGhoToken,
    AaveV3Asset,
    AaveV3DebtPosition,
    AaveV3Market,
    AaveV3User,
)
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger
from degenbot.provider.interface import ProviderAdapter


def verify_gho_discount_amounts(
    *,
    provider: ProviderAdapter,
    session: Session,
    market: AaveV3Market,
    gho_asset: AaveGhoToken,
    block_number: int,
    show_progress: bool,
    user_addresses: set[ChecksumAddress] | None = None,
) -> None:
    """
    Verify that GHO discount values in the database match the contract.

    If user_addresses is provided, only verifies those specific users.
    Otherwise, verifies all users in the market.
    """

    # Skip verification if discount mechanism is not supported (revision 4+)
    revision = get_gho_vtoken_revision(session=session, market=market)
    logger.debug(f"Verifying GHO discounts: revision={revision}, market.id={market.id}")
    if revision is None or revision >= GHO_DISCOUNT_DEPRECATION_REVISION:
        logger.debug(
            f"Skipping GHO discount verification for GHO VariableDebtToken revision {revision}"
        )
        return

    assert gho_asset.v_gho_discount_token is not None
    assert gho_asset.v_token is not None

    gho_vtoken_address = gho_asset.v_token.address

    # Only verify users who have GHO debt positions
    stmt = (
        select(AaveV3User)
        .join(AaveV3DebtPosition)
        .join(AaveV3Asset)
        .where(
            AaveV3User.market_id == market.id,
            AaveV3Asset.v_token_id == gho_asset.v_token_id,
        )
        .distinct()
    )
    if user_addresses is not None:
        stmt = stmt.where(AaveV3User.address.in_(user_addresses))

    users_to_verify = session.scalars(stmt).all()

    for user in tqdm.tqdm(
        users_to_verify,
        desc="Verifying GHO discount amounts",
        leave=False,
        disable=not show_progress,
    ):
        (discount_percent,) = raw_call(
            w3=provider,
            address=gho_vtoken_address,
            calldata=encode_function_calldata(
                function_prototype="getDiscountPercent(address)",
                function_arguments=[user.address],
            ),
            return_types=["uint256"],
            block_identifier=block_number,
        )
        assert user.gho_discount == discount_percent, (
            f"User {user.address}: GHO discount {user.gho_discount} "
            f"does not match GHO vDebtToken contract ({discount_percent}) "
            f"@ {gho_vtoken_address} at block {block_number}"
        )


def verify_stk_aave_balances(
    *,
    provider: ProviderAdapter,
    session: Session,
    market: AaveV3Market,
    gho_asset: AaveGhoToken,
    block_number: int,
    show_progress: bool,
    user_addresses: set[ChecksumAddress] | None = None,
) -> None:
    """
    Verify that tracked stkAAVE balances in the database match the contract.

    If user_addresses is provided, only verifies those specific users.
    Otherwise, verifies all users in the market.
    """

    if gho_asset.v_gho_discount_token is None:
        return

    discount_token = gho_asset.v_gho_discount_token

    stmt = select(AaveV3User).where(
        AaveV3User.market_id == market.id,
        AaveV3User.stk_aave_balance.is_not(None),
    )
    if user_addresses is not None:
        stmt = stmt.where(AaveV3User.address.in_(user_addresses))

    users_to_verify = session.scalars(stmt).all()

    for user in tqdm.tqdm(
        users_to_verify,
        desc="Verifying stkAAVE balances",
        leave=False,
        disable=not show_progress,
    ):
        assert user.stk_aave_balance is not None

        (actual_balance,) = raw_call(
            w3=provider,
            address=discount_token,
            calldata=encode_function_calldata(
                function_prototype="balanceOf(address)",
                function_arguments=[user.address],
            ),
            return_types=["uint256"],
            block_identifier=block_number,
        )

        assert user.stk_aave_balance == actual_balance, (
            f"User {user.address}: stkAAVE balance {user.stk_aave_balance} "
            f"does not match contract ({actual_balance}) "
            f"@ {discount_token} at block {block_number}"
        )
