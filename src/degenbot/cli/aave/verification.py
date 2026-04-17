"""Position verification module for on-chain validation of Aave positions."""

from typing import assert_never, cast

import tqdm
from eth_typing import ChecksumAddress
from sqlalchemy import delete, select
from sqlalchemy.orm import Session, joinedload

from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.aave.db_assets import get_contract, get_gho_asset
from degenbot.cli.aave.db_verification import verify_gho_discount_amounts, verify_stk_aave_balances
from degenbot.cli.aave.types import TransactionContext
from degenbot.constants import DEAD_ADDRESS, ZERO_ADDRESS
from degenbot.database.models.aave import (
    AaveGhoToken,
    AaveV3Asset,
    AaveV3CollateralPosition,
    AaveV3DebtPosition,
    AaveV3Market,
    AaveV3User,
)
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger
from degenbot.provider.interface import ProviderAdapter


def get_current_borrow_index_from_pool(
    provider: ProviderAdapter,
    pool_address: ChecksumAddress,
    underlying_asset_address: ChecksumAddress,
    block_number: int,
) -> int | None:
    """
    Fetch the current borrow index from the Aave Pool contract.

    This is used when the asset's cached borrow_index is 0 (not yet updated
    by a ReserveDataUpdated event) to get the current global index.

    Args:
        provider: ProviderAdapter for blockchain calls
        pool_address: The Aave Pool contract address
        underlying_asset_address: The underlying asset address (e.g., GHO token)
        block_number: The block number to query at

    Returns:
        The current borrow index, or None if the call fails
    """

    borrow_index: int
    (borrow_index,) = raw_call(
        w3=provider,
        address=pool_address,
        calldata=encode_function_calldata(
            function_prototype="getReserveNormalizedVariableDebt(address)",
            function_arguments=[underlying_asset_address],
        ),
        return_types=["uint256"],
        block_identifier=block_number,
    )
    return borrow_index


def update_debt_position_index(
    *,
    tx_context: TransactionContext,
    debt_asset: AaveV3Asset,
    debt_position: AaveV3DebtPosition,
    event_index: int,
    event_block_number: int,
) -> None:
    """
    Update debt position's last_index from current pool state.

    Fetches the current global borrow index from the pool contract and updates
    the position's last_index if the new index is greater than the current one.
    """

    pool_contract = get_contract(
        session=tx_context.session,
        market=tx_context.market,
        contract_name="POOL",
    )
    assert pool_contract is not None

    fetched_index = get_current_borrow_index_from_pool(
        provider=tx_context.provider,
        pool_address=get_checksum_address(pool_contract.address),
        underlying_asset_address=get_checksum_address(debt_asset.underlying_token.address),
        block_number=event_block_number,
    )
    # Use fetched index if available, otherwise fall back to event index
    current_index = fetched_index if fetched_index is not None else event_index
    # Only update last_index if the new index is greater than current
    # This prevents earlier events (in log index order) from overwriting
    # later events' indices when operations are processed out of order
    if current_index > (debt_position.last_index or 0):
        debt_position.last_index = current_index


def cleanup_zero_balance_positions(
    *,
    session: Session,
    market: AaveV3Market,
) -> None:
    """
    Delete all zero-balance debt and collateral positions for the market.
    """

    # Delete zero-balance collateral positions using bulk delete
    session.execute(
        delete(AaveV3CollateralPosition).where(
            AaveV3CollateralPosition.id.in_(
                select(AaveV3CollateralPosition.id)
                .join(AaveV3User)
                .where(
                    AaveV3User.market_id == market.id,
                    AaveV3CollateralPosition.balance == 0,
                )
            )
        )
    )

    # Delete zero-balance debt positions using bulk delete
    session.execute(
        delete(AaveV3DebtPosition).where(
            AaveV3DebtPosition.id.in_(
                select(AaveV3DebtPosition.id)
                .join(AaveV3User)
                .where(
                    AaveV3User.market_id == market.id,
                    AaveV3DebtPosition.balance == 0,
                )
            )
        )
    )


def verify_positions_for_users(
    *,
    provider: ProviderAdapter,
    market: AaveV3Market,
    session: Session,
    gho_asset: AaveGhoToken,
    block_number: int,
    show_progress: bool,
    user_addresses: set[ChecksumAddress] | None = None,
) -> None:
    """
    Verify positions for specified users or all users.

    If user_addresses is provided, only verifies those specific users.
    Otherwise, verifies all users in the market.
    """

    verify_scaled_token_positions(
        provider=provider,
        market=market,
        session=session,
        position_table=AaveV3CollateralPosition,
        block_number=block_number,
        show_progress=show_progress,
        user_addresses=user_addresses,
    )
    verify_scaled_token_positions(
        provider=provider,
        market=market,
        session=session,
        position_table=AaveV3DebtPosition,
        block_number=block_number,
        show_progress=show_progress,
        user_addresses=user_addresses,
    )
    verify_stk_aave_balances(
        provider=provider,
        session=session,
        market=market,
        gho_asset=gho_asset,
        block_number=block_number,
        show_progress=show_progress,
        user_addresses=user_addresses,
    )
    verify_gho_discount_amounts(
        provider=provider,
        session=session,
        market=market,
        gho_asset=gho_asset,
        block_number=block_number,
        show_progress=show_progress,
        user_addresses=user_addresses,
    )


def verify_all_positions(
    *,
    provider: ProviderAdapter,
    market: AaveV3Market,
    session: Session,
    block_number: int,
    show_progress: bool,
) -> None:
    """
    Verify all positions in the market against on-chain state.

    This performs a comprehensive verification of all collateral positions,
    debt positions, stkAAVE balances, and GHO discount amounts for the
    entire market.

    Args:
        provider: ProviderAdapter for blockchain calls
        market: The Aave V3 market to verify
        session: Database session
        block_number: The block number to verify against
        show_progress: If True, show progress bars
    """

    logger.info(f"Performing full verification of all positions at block {block_number:,}")

    gho_asset = get_gho_asset(session=session, market=market)

    verify_positions_for_users(
        provider=provider,
        market=market,
        session=session,
        gho_asset=gho_asset,
        block_number=block_number,
        show_progress=show_progress,
        user_addresses=None,
    )


def verify_scaled_token_positions(
    *,
    provider: ProviderAdapter,
    market: AaveV3Market,
    session: Session,
    position_table: type[AaveV3CollateralPosition | AaveV3DebtPosition],
    block_number: int,
    show_progress: bool,
    user_addresses: set[ChecksumAddress],
) -> None:
    """
    Verify that database position balances match the contract.

    If user_addresses is provided, only verifies positions for those specific users.
    Otherwise, verifies all users in the market.
    """

    stmt = (
        select(position_table)
        .join(AaveV3User)
        .where(AaveV3User.market_id == market.id)
        .options(
            joinedload(position_table.user),
            joinedload(position_table.asset).joinedload(AaveV3Asset.a_token),
            joinedload(position_table.asset).joinedload(AaveV3Asset.v_token),
        )
    )

    if user_addresses is not None:
        stmt = stmt.where(AaveV3User.address.in_(user_addresses))

    all_positions = session.scalars(stmt).all()

    for position in tqdm.tqdm(
        all_positions,
        desc=(
            "Verifying collateral positions"
            if position_table is AaveV3CollateralPosition
            else "Verifying debt positions"
        ),
        leave=False,
        disable=not show_progress,
    ):
        if position.user.address in {DEAD_ADDRESS, ZERO_ADDRESS}:
            continue

        position = cast("AaveV3CollateralPosition | AaveV3DebtPosition", position)

        if position_table is AaveV3CollateralPosition:
            token_address = get_checksum_address(position.asset.a_token.address)
        elif position_table is AaveV3DebtPosition:
            token_address = get_checksum_address(position.asset.v_token.address)
        else:
            assert_never(position_table)

        (actual_scaled_balance,) = raw_call(
            w3=provider,
            address=token_address,
            calldata=encode_function_calldata(
                function_prototype="scaledBalanceOf(address)",
                function_arguments=[position.user.address],
            ),
            return_types=["uint256"],
            block_identifier=block_number,
        )

        position_type = "collateral" if position_table is AaveV3CollateralPosition else "debt"
        assert actual_scaled_balance == position.balance, (
            f"{position_type.capitalize()} balance verification failure for {position.asset}. "
            f"User {position.user} scaled balance ({position.balance}) does not match contract "
            f"balance ({actual_scaled_balance}) at block {block_number}"
        )

        (actual_last_index,) = raw_call(
            w3=provider,
            address=token_address,
            calldata=encode_function_calldata(
                function_prototype="getPreviousIndex(address)",
                function_arguments=[position.user.address],
            ),
            return_types=["uint256"],
            block_identifier=block_number,
        )

        assert actual_last_index == position.last_index, (
            f"{position_type.capitalize()} index verification failure for {position.asset}. "
            f"User {position.user} last_index ({position.last_index}) does not match contract "
            f"last_index ({actual_last_index}) at block {block_number}"
        )
