import sys
from typing import TYPE_CHECKING, cast

import click
import eth_abi.abi
import tqdm
from eth_typing import ChainId, ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session, joinedload
from tqdm.contrib.logging import logging_redirect_tqdm

from degenbot.aave.deployments import EthereumMainnetAaveV3
from degenbot.aave.events import AaveV3GhoDebtTokenEvent, AaveV3PoolConfigEvent
from degenbot.aave.position_analysis import UserPositionSummary, analyze_positions_for_market
from degenbot.checksum_cache import get_checksum_address
from degenbot.cli import cli
from degenbot.cli.aave.constants import POSITION_RISK_DISPLAY_LIMIT
from degenbot.cli.aave.db_assets import get_contract, get_gho_asset, get_or_create_erc20_token
from degenbot.cli.aave.event_fetchers import (
    fetch_address_provider_events,
    fetch_discount_config_events,
    fetch_oracle_events,
    fetch_pool_events,
    fetch_reserve_initialization_events,
    fetch_scaled_token_events,
    fetch_stk_aave_events,
)
from degenbot.cli.aave.event_handlers import (
    _process_asset_collateral_in_emode_changed_event,
    _process_asset_initialization_event,
    _process_collateral_configuration_changed_event,
    _process_discount_token_updated_event,
    _process_e_mode_category_added_event,
    _process_emode_asset_category_changed_event,
    _process_proxy_creation_event,
)
from degenbot.cli.aave.extraction import extract_user_addresses_from_transaction
from degenbot.cli.aave.transaction_processor import _process_transaction
from degenbot.cli.aave.utils import _build_transaction_contexts, _get_all_scaled_token_addresses
from degenbot.cli.aave.verification import (
    cleanup_zero_balance_positions,
    verify_all_positions,
    verify_positions_for_users,
)
from degenbot.cli.utils import get_web3_from_config
from degenbot.database import db_session
from degenbot.database.models.aave import (
    AaveGhoToken,
    AaveV3CollateralPosition,
    AaveV3Contract,
    AaveV3DebtPosition,
    AaveV3Market,
    AaveV3User,
)
from degenbot.database.operations import backup_sqlite_database
from degenbot.exceptions import DegenbotValueError
from degenbot.functions import encode_function_calldata, get_number_for_block_identifier, raw_call
from degenbot.logging import logger
from degenbot.provider.interface import ProviderAdapter

if TYPE_CHECKING:
    from eth_typing.evm import BlockParams
    from web3.types import LogReceipt


# Forward imports from submodules - functions are defined in their respective modules
__all__ = [
    "aave",
    "aave_update",
    "activate",
    "activate_ethereum_aave_v3",
    "deactivate",
    "deactivate_mainnet_aave_v3",
    "market",
    "market_show",
    "position",
    "position_risk",
    "position_show",
    "update_aave_market",
]


@cli.group
def aave() -> None:
    """
    Aave commands
    """


@aave.group
def activate() -> None:
    """
    Activate an Aave market.

    Positions for activated markets are included when running `degenbot aave position update`.
    """


@activate.command("ethereum_aave_v3")
def activate_ethereum_aave_v3(chain_id: ChainId = ChainId.ETH) -> None:
    """
    Activate Aave V3 on Ethereum mainnet.
    """

    # GHO Token Address (Ethereum Mainnet) - only needed for market activation
    gho_token_address = get_checksum_address("0x40D16FC0246aD3160Ccc09B8D0D3A2cD28aE6C2f")

    pool_address_provider = EthereumMainnetAaveV3.pool_address_provider

    provider = get_web3_from_config(chain_id=chain_id)

    (market_name,) = raw_call(
        w3=provider,
        address=pool_address_provider,
        calldata=encode_function_calldata(
            function_prototype="getMarketId()",
            function_arguments=None,
        ),
        return_types=["string"],
    )

    with db_session() as session:
        market = session.scalar(
            select(AaveV3Market).where(
                AaveV3Market.chain_id == chain_id,
                AaveV3Market.name == market_name,
            )
        )

        if market is not None:
            market.active = True
        else:
            market = AaveV3Market(
                chain_id=chain_id,
                name=market_name,
                active=True,
                # The pool address provider was deployed on block 16,291,071 by TX
                # 0x75fb6e6be55226712f896ae81bbfc86005b2521adb7555d28ce6fe8ab495ef73
                last_update_block=16_291_070,
            )
            session.add(market)
            session.flush()
            session.add(
                AaveV3Contract(
                    market_id=market.id,
                    name="POOL_ADDRESS_PROVIDER",
                    address=EthereumMainnetAaveV3.pool_address_provider,
                )
            )

            # GHO tokens are chain-unique, so create a single entry that all markets on this chain
            # will share.
            gho_asset_token = get_or_create_erc20_token(
                provider=provider,
                session=session,
                chain_id=market.chain_id,
                token_address=gho_token_address,
            )

            if (
                session.scalar(
                    select(AaveGhoToken).where(
                        AaveGhoToken.token_id == gho_asset_token.id,
                    )
                )
                is None
            ):
                session.add(AaveGhoToken(token_id=gho_asset_token.id))

        session.commit()

    click.echo(f"Activated Aave V3 on Ethereum (chain ID {chain_id}).")


@aave.group
def deactivate() -> None:
    """
    Deactivate an Aave market.

    Positions for deactivated markets are not included when running `degenbot aave position update`.
    """


@deactivate.command("ethereum_aave_v3")
def deactivate_mainnet_aave_v3(
    chain_id: ChainId = ChainId.ETH,
    market_name: str = "Aave Ethereum Market",
) -> None:
    """
    Deactivate the Aave V3 Ethereum mainnet market.
    """

    with db_session() as session:
        market = session.scalar(
            select(AaveV3Market).where(
                AaveV3Market.chain_id == chain_id,
                AaveV3Market.name == market_name,
            )
        )

        if market is None:
            click.echo(f"The database has no entry for Aave V3 on Ethereum (chain ID {chain_id}).")
            return

        if not market.active:
            return
        market.active = False
        session.commit()

    click.echo(f"Deactivated Aave V3 on {chain_id.name} (chain ID {chain_id}).")


@aave.command(
    "update",
    help="Update positions for active Aave markets.",
)
@click.option(
    "--chunk",
    "chunk_size",
    default=10_000,
    show_default=True,
    help="The maximum number of blocks to process before committing changes to the database.",
    envvar="DEGENBOT_CHUNK_SIZE",
    show_envvar=True,
)
@click.option(
    "--to-block",
    "to_block",
    default="latest:-64",
    show_default=True,
    help=(
        "The last block in the update range. Must be a valid block identifier: "
        "'earliest', 'finalized', 'safe', 'latest', 'pending'. An identifier can be given with an "
        "optional offset, e.g. 'latest:-64' stops 64 blocks before the chain tip, "
        "'safe:128' stops 128 blocks after the last 'safe' block."
    ),
)
@click.option(
    "--verify-block/--no-verify-block",
    "verify_block",
    default=False,
    show_default=True,
    help="Verify positions at each block boundary.",
    envvar="DEGENBOT_VERIFY_BLOCK",
    show_envvar=True,
)
@click.option(
    "--verify-chunk/--no-verify-chunk",
    "verify_chunk",
    default=True,
    show_default=True,
    help="Verify positions at chunk boundaries.",
    envvar="DEGENBOT_VERIFY_CHUNK",
    show_envvar=True,
)
@click.option(
    "--verify-all/--no-verify-all",
    "verify_all",
    default=True,
    show_default=True,
    help="Verify all positions at full verification intervals.",
    envvar="DEGENBOT_VERIFY_ALL",
    show_envvar=True,
)
@click.option(
    "--one-chunk",
    "stop_after_one_chunk",
    is_flag=True,
    default=False,
    show_default=True,
    help="Stop processing after the first chunk.",
    envvar="DEGENBOT_ONE_CHUNK",
    show_envvar=True,
)
@click.option(
    "--progress-bar/--no-progress-bar",
    "show_progress",
    default=True,
    show_default=True,
    help="Show progress bars.",
    envvar="DEGENBOT_PROGRESS_BAR",
    show_envvar=True,
)
@click.option(
    "--dry-run",
    "dry_run",
    is_flag=True,
    default=False,
    show_default=True,
    help="Preview changes without committing to the database.",
    envvar="DEGENBOT_DRY_RUN",
    show_envvar=True,
)
@click.option(
    "--backup/--no-backup",
    "enable_backup",
    default=False,
    show_default=True,
    help="Enable or disable database backups at verification intervals.",
    envvar="DEGENBOT_BACKUP",
    show_envvar=True,
)
@click.option(
    "--backup-interval",
    "backup_interval",
    default=500_000,
    show_default=True,
    type=int,
    help="Number of blocks between database backups.",
    envvar="DEGENBOT_BACKUP_INTERVAL",
    show_envvar=True,
)
def aave_update(
    *,
    chunk_size: int,
    to_block: str,
    verify_block: bool,
    verify_chunk: bool,
    verify_all: bool,
    stop_after_one_chunk: bool,
    show_progress: bool,
    dry_run: bool,
    enable_backup: bool,
    backup_interval: int,
) -> None:
    """
    Update positions for active Aave markets.

    Processes blockchain events from the last updated block to the specified block,
    updating all user positions, interest rates, and indices in the database.

    Args:
        chunk_size: Maximum number of blocks to process before committing changes.
        to_block: Target block identifier (e.g., 'latest', 'latest:-64', 'finalized:128').
        verify_block: If True, verify positions at each block boundary.
        verify_chunk: If True, verify positions at chunk boundaries.
        verify_all: If True, verify all positions at full verification intervals.
        stop_after_one_chunk: If True, stop after processing the first chunk.
        show_progress: Toggle display of progress bars.
        dry_run: If True, preview changes without committing to the database.
        enable_backup: If True, create database backups at verification intervals.
        backup_interval: Number of blocks between database backups.
    """

    with (  # noqa:PLR1702
        logging_redirect_tqdm(
            loggers=[logger],
        ),
    ):
        with db_session() as session:
            active_chains = set(
                session.scalars(
                    select(AaveV3Market.chain_id).where(
                        AaveV3Market.active,
                        AaveV3Market.name.contains("aave"),
                    )
                ).all()
            )

        if not active_chains:
            msg = "No active Aave markets found."
            raise DegenbotValueError(msg)

        for chain_id in active_chains:
            provider = get_web3_from_config(chain_id=chain_id)

            with db_session() as session:
                active_markets = session.scalars(
                    select(AaveV3Market).where(
                        AaveV3Market.active,
                        AaveV3Market.chain_id == chain_id,
                        AaveV3Market.name.contains("aave"),
                    )
                ).all()

                if not active_markets:
                    click.echo(f"No active Aave markets on chain {chain_id}.")
                    continue

                initial_start_block = working_start_block = min(
                    0 if market.last_update_block is None else market.last_update_block + 1
                    for market in active_markets
                )

            if to_block.isdigit():
                last_block = int(to_block)
            else:
                if ":" in to_block:
                    parts = to_block.split(":", 1)
                    block_tag, offset = cast("tuple[BlockParams,str]", parts)
                    block_offset = int(offset.strip())
                else:
                    block_tag = cast("BlockParams", to_block)
                    block_offset = 0

                if block_tag not in {"latest", "earliest", "pending", "safe", "finalized"}:
                    msg = f"Invalid block tag: {block_tag}"
                    raise ValueError(msg)

                last_block = (
                    get_number_for_block_identifier(identifier=block_tag, w3=provider)
                    + block_offset
                )

            current_block_number = get_number_for_block_identifier(identifier="latest", w3=provider)
            if last_block > current_block_number:
                msg = f"{to_block} is ahead of the current chain tip."
                raise ValueError(msg)

            if initial_start_block > last_block:
                msg = (
                    f"Chain {chain_id}: --to-block must be greater than the "
                    f"market's last update block ({initial_start_block - 1})."
                )
                raise ValueError(msg)

            block_pbar = tqdm.tqdm(
                total=last_block - initial_start_block + 1,
                bar_format="{desc} {percentage:3.1f}% |{bar}|",
                leave=False,
                disable=not show_progress,
            )

            block_pbar.n = working_start_block - initial_start_block

            while True:
                with db_session() as session:
                    active_markets = session.scalars(
                        select(AaveV3Market).where(
                            AaveV3Market.active,
                            AaveV3Market.chain_id == chain_id,
                            AaveV3Market.name.contains("aave"),
                        )
                    ).all()

                    # Cap the working end block at the lowest of:
                    # - the safe block for the chain
                    # - the end of the working chunk size
                    # - all update blocks for active markets
                    working_end_block = min(
                        [last_block]
                        + [working_start_block + chunk_size - 1]
                        + [
                            market.last_update_block
                            for market in active_markets
                            if market.last_update_block is not None
                            if market.last_update_block > working_start_block
                        ],
                    )
                    assert working_end_block >= working_start_block

                    block_pbar.set_description(
                        f"Processing block range {working_start_block:,} -> {working_end_block:,}"
                    )
                    block_pbar.refresh()

                    markets_to_update = {
                        market
                        for market in active_markets
                        if (
                            market.last_update_block is None
                            or market.last_update_block + 1 == working_start_block
                        )
                    }

                    for market in markets_to_update:
                        try:
                            update_aave_market(
                                provider=provider,
                                start_block=working_start_block,
                                end_block=working_end_block,
                                market=market,
                                session=session,
                                verify_block=verify_block,
                                verify_chunk=verify_chunk,
                                show_progress=show_progress,
                            )
                        except Exception:  # noqa: BLE001
                            logger.exception("")
                            sys.exit(1)

                        if dry_run:
                            session.rollback()
                            click.echo(
                                f"Dry run: processed blocks {working_start_block:,} -> "
                                f"{working_end_block:,} for {market.name} (no changes committed)"
                            )
                            continue

                        market.last_update_block = working_end_block

                        if verify_all and (
                            working_end_block // backup_interval
                            != working_start_block // backup_interval
                            or working_end_block % backup_interval == 0
                        ):
                            verify_all_positions(
                                provider=provider,
                                market=market,
                                session=session,
                                block_number=working_end_block,
                                show_progress=show_progress,
                            )
                            cleanup_zero_balance_positions(
                                session=session,
                                market=market,
                            )

                            session.commit()
                            if enable_backup:
                                backup_sqlite_database(
                                    session=session,
                                    suffix=f"{working_end_block}",
                                    skip_confirmation=True,
                                )
                                logger.info(
                                    f"Created database backup at block {working_end_block:,}"
                                )
                            db_session.remove()
                        else:
                            session.commit()

                    if working_end_block == last_block or stop_after_one_chunk:
                        break
                    working_start_block = working_end_block + 1

                    block_pbar.n = working_end_block - initial_start_block

            block_pbar.close()


@aave.group()
def position() -> None:
    """
    Position commands
    """


@position.command("show")
@click.argument("address", type=str)
@click.option(
    "--market",
    type=str,
    default="aave_v3",
    show_default=True,
    help="Market name to query (default: aave_v3).",
)
@click.option(
    "--chain-id",
    type=int,
    default=1,
    show_default=True,
    help="Chain ID to query (default: 1 for Ethereum mainnet).",
)
def position_show(address: str, market: str, chain_id: int) -> None:
    """
    Display current Aave positions for a user.

    Shows collateral and debt positions for the specified address on the given market.
    """

    try:
        user_address = get_checksum_address(address)
    except Exception as exc:
        click.echo(f"Invalid address: {address}")
        raise click.Abort from exc

    with db_session() as session:
        # Find the market first
        market_obj = session.scalar(
            select(AaveV3Market).where(
                AaveV3Market.name == market,
                AaveV3Market.chain_id == chain_id,
            )
        )

        if market_obj is None:
            click.echo(f"No market found with name '{market}' on chain {chain_id}.")
            return

        # Find the user for this specific market
        user = session.scalar(
            select(AaveV3User).where(
                AaveV3User.address == user_address,
                AaveV3User.market_id == market_obj.id,
            )
        )

        if user is None:
            click.echo(
                f"No Aave user found for address {user_address} in market '{market}' "
                f"on chain {chain_id}."
            )
            return

        # Get collateral positions
        collateral_positions = session.scalars(
            select(AaveV3CollateralPosition)
            .where(AaveV3CollateralPosition.user_id == user.id)
            .options(joinedload(AaveV3CollateralPosition.asset))
        ).all()

        # Get debt positions
        debt_positions = session.scalars(
            select(AaveV3DebtPosition)
            .where(AaveV3DebtPosition.user_id == user.id)
            .options(joinedload(AaveV3DebtPosition.asset))
        ).all()

        click.echo(f"\nAave V3 Positions for {user_address}")
        click.echo(f"Market: {market} (Chain: {chain_id})")
        click.echo("=" * 60)

        # Display collateral positions
        if collateral_positions:
            click.echo("\nCollateral Positions:")
            click.echo("-" * 60)
            for collateral_pos in collateral_positions:
                asset_symbol = (
                    collateral_pos.asset.underlying_token.symbol
                    if collateral_pos.asset.underlying_token
                    else "Unknown"
                )
                click.echo(f"  {asset_symbol}: {collateral_pos.balance} (scaled)")
        else:
            click.echo("\nNo collateral positions found.")

        # Display debt positions
        if debt_positions:
            click.echo("\nDebt Positions:")
            click.echo("-" * 60)
            for debt_pos in debt_positions:
                asset_symbol = (
                    debt_pos.asset.underlying_token.symbol
                    if debt_pos.asset.underlying_token
                    else "Unknown"
                )
                click.echo(f"  {asset_symbol}: {debt_pos.balance} (scaled)")
        else:
            click.echo("\nNo debt positions found.")

        click.echo()


@position.command("risk")
@click.option(
    "--market",
    type=str,
    default="aave_v3",
    show_default=True,
    help="Market name to analyze (default: aave_v3).",
)
@click.option(
    "--chain-id",
    type=int,
    default=1,
    show_default=True,
    help="Chain ID (default: 1 for Ethereum mainnet).",
)
@click.option(
    "--threshold",
    type=float,
    default=1.1,
    show_default=True,
    help="Health factor threshold for 'at risk' classification.",
)
@click.option(
    "--limit",
    type=int,
    default=None,
    help="Maximum number of users to analyze (for testing).",
)
@click.option(
    "--show-positions",
    is_flag=True,
    default=False,
    help="Show detailed position information for at-risk users.",
)
@click.option(
    "--skip-prices",
    is_flag=True,
    default=False,
    help="Skip fetching prices from oracle (faster, but HF values are relative).",
)
def position_risk(  # noqa: PLR0917
    market: str,
    chain_id: int,
    threshold: float,
    limit: int | None,
    show_positions: bool,  # noqa: FBT001
    skip_prices: bool,  # noqa: FBT001
) -> None:
    """
    Analyze positions for liquidation risk.

    Identifies users with low health factors who are at risk of liquidation.
    Users are categorized as:
    - Liquidatable: Health factor < 1.0
    - At risk: Health factor < threshold (default 1.1)
    - Safe: Health factor >= threshold

    By default, prices are fetched from the Aave oracle for accurate health
    factor calculations. Use --skip-prices for faster analysis when you only
    need relative risk comparisons.
    """

    # Get provider for price fetching
    provider = None if skip_prices else get_web3_from_config(chain_id=chain_id)

    with db_session() as session:
        # Find the market
        market_obj = session.scalar(
            select(AaveV3Market).where(
                AaveV3Market.name == market,
                AaveV3Market.chain_id == chain_id,
            )
        )

        if market_obj is None:
            click.echo(f"No market found with name '{market}' on chain {chain_id}.")
            return

        click.echo(f"\nAnalyzing positions for market: {market} (Chain: {chain_id})")
        click.echo(f"Health factor threshold: {threshold}")
        click.echo("=" * 60)

        # Analyze positions
        result = analyze_positions_for_market(
            session=session,
            market_id=market_obj.id,
            health_factor_threshold=threshold,
            limit=limit,
            provider=provider,
        )

        # Display summary
        click.echo("\nAnalysis Summary:")
        click.echo(f"  Total users with debt: {result.total_users}")
        click.echo(f"  Safe users:            {len(result.safe_users)}")
        click.echo(f"  At risk (HF < {threshold}):  {result.at_risk_count}")
        click.echo(f"  Liquidatable (HF < 1): {result.liquidatable_count}")

        # Display liquidatable users
        if result.liquidatable_users:
            click.echo(f"\n{'=' * 60}")
            click.echo("LIQUIDATABLE POSITIONS (HF < 1.0)")
            click.echo("=" * 60)
            for user_summary in result.liquidatable_users[:POSITION_RISK_DISPLAY_LIMIT]:
                _display_user_risk(user_summary, show_positions=show_positions)

            if len(result.liquidatable_users) > POSITION_RISK_DISPLAY_LIMIT:
                remaining = len(result.liquidatable_users) - POSITION_RISK_DISPLAY_LIMIT
                click.echo(f"  ... and {remaining} more")

        # Display at-risk users
        if result.at_risk_users:
            click.echo(f"\n{'=' * 60}")
            click.echo(f"AT-RISK POSITIONS (HF < {threshold})")
            click.echo("=" * 60)
            for user_summary in result.at_risk_users[:POSITION_RISK_DISPLAY_LIMIT]:
                _display_user_risk(user_summary, show_positions=show_positions)

            if len(result.at_risk_users) > POSITION_RISK_DISPLAY_LIMIT:
                remaining = len(result.at_risk_users) - POSITION_RISK_DISPLAY_LIMIT
                click.echo(f"  ... and {remaining} more")


def _display_user_risk(
    user_summary: UserPositionSummary,
    *,
    show_positions: bool,
) -> None:
    """Display risk information for a single user."""
    hf_str = f"{user_summary.health_factor:.4f}" if user_summary.health_factor else "N/A"
    ltv_str = f"{user_summary.max_ltv_ratio:.2%}" if user_summary.max_ltv_ratio else "N/A"

    click.echo(f"\n  User: {user_summary.user_address}")
    click.echo(f"    Health Factor: {hf_str}")
    click.echo(f"    Max LTV Ratio: {ltv_str}")

    if user_summary.emode_category_id:
        click.echo(f"    eMode Category: {user_summary.emode_category_id}")
    if user_summary.is_isolation_mode:
        click.echo("    Isolation Mode: Yes")

    if show_positions:
        if user_summary.collateral_positions:
            click.echo("    Collateral:")
            for collateral_pos in user_summary.collateral_positions:
                if collateral_pos.actual_balance > 0:
                    enabled_str = "" if collateral_pos.is_enabled_as_collateral else " (disabled)"
                    emode_str = " [eMode]" if collateral_pos.in_emode else ""
                    click.echo(
                        f"      {collateral_pos.asset_symbol}: {collateral_pos.actual_balance:,} "
                        f"(LT: {collateral_pos.liquidation_threshold / 100:.0f}%)"
                        f"{enabled_str}{emode_str}"
                    )

        if user_summary.debt_positions:
            click.echo("    Debt:")
            for debt_pos in user_summary.debt_positions:
                if debt_pos.actual_balance > 0:
                    emode_str = " [eMode]" if debt_pos.in_emode else ""
                    click.echo(
                        f"      {debt_pos.asset_symbol}: {debt_pos.actual_balance:,}{emode_str}"
                    )


@aave.group()
def market() -> None:
    """
    Market commands
    """


@market.command("show")
@click.option(
    "--chain-id",
    type=int,
    default=None,
    show_default=True,
    help="Filter by chain ID (default: show all chains).",
)
@click.option(
    "--name",
    type=str,
    default=None,
    help="Filter by market name (default: show all markets).",
)
def market_show(chain_id: int | None, name: str | None) -> None:
    """
    Display Aave market information.

    Shows all markets or filters by chain ID and/or market name.
    """

    with db_session() as session:
        query = select(AaveV3Market)

        if chain_id is not None:
            query = query.where(AaveV3Market.chain_id == chain_id)
        if name is not None:
            query = query.where(AaveV3Market.name == name)

        markets = session.scalars(query).all()

        if not markets:
            filters = []
            if chain_id is not None:
                filters.append(f"chain_id={chain_id}")
            if name is not None:
                filters.append(f"name='{name}'")
            filter_str = ", ".join(filters) if filters else "no filters"
            click.echo(f"No Aave markets found ({filter_str}).")
            return

        click.echo("\nAave V3 Markets")
        click.echo("=" * 80)

        for market_obj in markets:
            status = "active" if market_obj.active else "inactive"
            last_update = (
                f"{market_obj.last_update_block:,}"
                if market_obj.last_update_block is not None
                else "never"
            )

            click.echo(f"\nMarket: {market_obj.name}")
            click.echo(f"  Chain ID: {market_obj.chain_id}")
            click.echo(f"  Status: {status}")
            click.echo(f"  Last Update Block: {last_update}")

            # Count users and assets
            user_count = len(market_obj.users) if market_obj.users else 0
            asset_count = len(market_obj.assets) if market_obj.assets else 0

            click.echo(f"  Users: {user_count}")
            click.echo(f"  Assets: {asset_count}")

            if market_obj.assets:
                click.echo("  Asset List:")
                for asset in market_obj.assets:
                    token_symbol = (
                        asset.underlying_token.symbol if asset.underlying_token else "Unknown"
                    )
                    click.echo(f"    - {token_symbol}")


def update_aave_market(
    *,
    provider: ProviderAdapter,
    start_block: int,
    end_block: int,
    market: AaveV3Market,
    session: Session,
    verify_block: bool,
    verify_chunk: bool,
    show_progress: bool,
) -> None:
    """
    Update the Aave V3 market.

    Processes events in three phases:
    1. Bootstrap: Fetch and process proxy creation events to discover Pool and PoolConfigurator
       contracts.
    2. Asset Discovery: Fetch all targeted events and build transaction contexts
    3. User Event Processing: Process transactions with assertions that classifying events exist
    """

    logger.debug(
        f"Updating {market.name} (chain {market.chain_id}): "
        f"block range {start_block:,} - {end_block:,}"
    )

    # Phase 1: Collect proxy events and config events
    # These events will be processed chronologically in Phase 3
    proxy_events: list[LogReceipt] = []
    config_events: list[LogReceipt] = []

    pool_address_provider = get_contract(
        session=session,
        market=market,
        contract_name="POOL_ADDRESS_PROVIDER",
    )
    assert pool_address_provider is not None

    for event in fetch_address_provider_events(
        provider=provider,
        provider_address=get_checksum_address(pool_address_provider.address),
        start_block=start_block,
        end_block=end_block,
    ):
        topic = event["topics"][0]

        if topic == AaveV3PoolConfigEvent.PROXY_CREATED.value:
            _process_proxy_creation_event(
                provider=provider,
                session=session,
                market=market,
                event=event,
                proxy_name="POOL",
                proxy_id=eth_abi.abi.encode(["bytes32"], [b"POOL"]),
                revision_function_prototype="POOL_REVISION",
            )
            _process_proxy_creation_event(
                provider=provider,
                session=session,
                market=market,
                event=event,
                proxy_name="POOL_CONFIGURATOR",
                proxy_id=eth_abi.abi.encode(["bytes32"], [b"POOL_CONFIGURATOR"]),
                revision_function_prototype="CONFIGURATOR_REVISION",
            )
        elif topic in {
            AaveV3PoolConfigEvent.POOL_UPDATED.value,
            AaveV3PoolConfigEvent.POOL_CONFIGURATOR_UPDATED.value,
        }:
            # Save event for chronological processing in Phase 3. The revision will be updated when
            # the event is processed chronologically
            proxy_events.append(event)
        elif topic in {
            AaveV3PoolConfigEvent.POOL_DATA_PROVIDER_UPDATED.value,
            AaveV3PoolConfigEvent.PRICE_ORACLE_UPDATED.value,
            AaveV3PoolConfigEvent.ADDRESS_SET.value,
        }:
            # Save event for chronological processing in Phase 3
            config_events.append(event)

    # Phase 2
    pool_configurator = get_contract(
        session=session,
        market=market,
        contract_name="POOL_CONFIGURATOR",
    )
    if pool_configurator is not None:
        for event in fetch_reserve_initialization_events(
            provider=provider,
            configurator_address=pool_configurator.address,
            start_block=start_block,
            end_block=end_block,
        ):
            topic = event["topics"][0]
            if topic == AaveV3PoolConfigEvent.RESERVE_INITIALIZED.value:
                _process_asset_initialization_event(
                    provider=provider,
                    event=event,
                    market=market,
                    session=session,
                )
            elif topic == AaveV3PoolConfigEvent.COLLATERAL_CONFIGURATION_CHANGED.value:
                _process_collateral_configuration_changed_event(
                    provider=provider,
                    session=session,
                    event=event,
                    market=market,
                )
            elif topic == AaveV3PoolConfigEvent.EMODE_CATEGORY_ADDED.value:
                _process_e_mode_category_added_event(
                    session=session,
                    event=event,
                    market_id=market.id,
                )
            elif topic == AaveV3PoolConfigEvent.EMODE_ASSET_CATEGORY_CHANGED.value:
                _process_emode_asset_category_changed_event(
                    session=session,
                    event=event,
                    market_id=market.id,
                )
            elif topic == AaveV3PoolConfigEvent.ASSET_COLLATERAL_IN_EMODE_CHANGED.value:
                _process_asset_collateral_in_emode_changed_event(
                    session=session,
                    event=event,
                    market_id=market.id,
                )

    # Phase 3
    all_events: list[LogReceipt] = []

    # Include proxy upgrade events for chronological processing
    all_events.extend(proxy_events)

    # Include config events for chronological processing
    all_events.extend(config_events)

    pool = get_contract(
        session=session,
        market=market,
        contract_name="POOL",
    )
    if pool is None:
        # Pool not initialized yet, skip to next chunk
        logger.warning(f"Pool not initialized for market {market.id}, skipping")
        return

    pool_events = fetch_pool_events(
        provider=provider,
        pool_address=pool.address,
        start_block=start_block,
        end_block=end_block,
    )
    all_events.extend(pool_events)

    # Fetch oracle events - discover oracle from events if not yet known
    oracle_contract = get_contract(
        session=session,
        market=market,
        contract_name="PRICE_ORACLE",
    )
    oracle_address = (
        get_checksum_address(oracle_contract.address) if oracle_contract is not None else None
    )
    oracle_events = fetch_oracle_events(
        provider=provider,
        oracle_address=oracle_address,
        start_block=start_block,
        end_block=end_block,
    )
    all_events.extend(oracle_events)

    known_scaled_token_addresses = set(
        _get_all_scaled_token_addresses(
            session=session,
            chain_id=provider.chain_id,
        )
    )

    scaled_token_events = fetch_scaled_token_events(
        provider=provider,
        token_addresses=list(known_scaled_token_addresses),
        start_block=start_block,
        end_block=end_block,
    )
    all_events.extend(scaled_token_events)

    discount_config_events = fetch_discount_config_events(
        provider=provider,
        start_block=start_block,
        end_block=end_block,
    )
    all_events.extend(discount_config_events)

    gho_asset = get_gho_asset(session=session, market=market)
    assert gho_asset is not None

    for event in discount_config_events:
        topic = event["topics"][0]
        if topic == AaveV3GhoDebtTokenEvent.DISCOUNT_TOKEN_UPDATED.value:
            _process_discount_token_updated_event(
                event=event,
                gho_asset=gho_asset,
            )

    all_events.extend(
        fetch_stk_aave_events(
            provider=provider,
            discount_token=gho_asset.v_gho_discount_token,
            start_block=start_block,
            end_block=end_block,
        )
    )

    # Group the events into transaction bundles with a shared context
    tx_contexts = _build_transaction_contexts(
        events=all_events,
        market=market,
        session=session,
        provider=provider,
        gho_asset=gho_asset,
        pool_contract=pool,
    )

    # Sort transaction contexts chronologically by (block_number, first_event_log_index)
    sorted_tx_contexts = sorted(
        tx_contexts.values(),
        key=lambda ctx: (
            (ctx.block_number, ctx.events[0]["logIndex"]) if ctx.events else (ctx.block_number, 0)
        ),
    )

    # Collect users modified for verification
    users_modified_this_block: set[ChecksumAddress] = set()
    users_modified_this_chunk: set[ChecksumAddress] = set()

    last_verified_block: int | None = None

    # Process transactions chronologically
    for tx_context in tqdm.tqdm(
        sorted_tx_contexts,
        desc="Processing transactions",
        leave=False,
        disable=not show_progress,
    ):
        if last_verified_block is None:
            last_verified_block = tx_context.block_number

        # Update last_block when transitioning to a new block
        if last_verified_block < tx_context.block_number:
            # Verify previous block before moving to new block
            if verify_block and users_modified_this_block:
                logger.debug(
                    f"Verifying {len(users_modified_this_block)} users at "
                    f"block {last_verified_block}"
                )
                verify_positions_for_users(
                    provider=provider,
                    market=market,
                    session=session,
                    gho_asset=gho_asset,
                    block_number=last_verified_block,
                    show_progress=show_progress,
                    user_addresses=users_modified_this_block,
                )
                users_modified_this_block.clear()

            last_verified_block = tx_context.block_number

        # Track users modified in this transaction
        users_in_transaction = extract_user_addresses_from_transaction(events=tx_context.events)
        users_modified_this_block.update(users_in_transaction)
        users_modified_this_chunk.update(users_in_transaction)

        # Process entire transaction atomically with full context
        _process_transaction(tx_context=tx_context)

    if verify_block and users_modified_this_block:
        assert last_verified_block is not None
        logger.debug(
            f"Verifying {len(users_modified_this_block)} users at block {last_verified_block}"
        )
        verify_positions_for_users(
            provider=provider,
            market=market,
            session=session,
            gho_asset=gho_asset,
            block_number=last_verified_block,
            show_progress=show_progress,
            user_addresses=users_modified_this_block,
        )

    if verify_chunk and not verify_block and users_modified_this_chunk:
        verify_positions_for_users(
            provider=provider,
            market=market,
            session=session,
            gho_asset=gho_asset,
            block_number=end_block,
            show_progress=show_progress,
            user_addresses=users_modified_this_chunk,
        )

    logger.info(f"{market} successfully updated to block {end_block:,}")
