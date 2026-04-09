import sys
from typing import TYPE_CHECKING, cast

import click
import eth_abi.abi
import tqdm
from eth_typing import ChainId, ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session, joinedload
from tqdm.contrib.logging import logging_redirect_tqdm
from web3.types import LogReceipt

from degenbot.aave.deployments import EthereumMainnetAaveV3
from degenbot.aave.enrichment import ScaledEventEnricher
from degenbot.aave.events import (
    AaveV3GhoDebtTokenEvent,
    AaveV3OracleEvent,
    AaveV3PoolConfigEvent,
    AaveV3PoolEvent,
    AaveV3ScaledTokenEvent,
    AaveV3StkAaveEvent,
    ERC20Event,
    ScaledTokenEventType,
)
from degenbot.aave.libraries.gho_math import GhoMath
from degenbot.aave.libraries.pool_math import PoolMath
from degenbot.aave.libraries.token_math import TokenMathFactory
from degenbot.aave.liquidation_patterns import detect_liquidation_patterns
from degenbot.aave.models import EnrichedScaledTokenEvent
from degenbot.aave.operation_types import OperationType
from degenbot.aave.pattern_types import LiquidationPattern
from degenbot.aave.position_analysis import (
    UserPositionSummary,
    analyze_positions_for_market,
)
from degenbot.aave.processors.base import (
    CollateralBurnEvent,
    CollateralMintEvent,
    DebtBurnEvent,
    DebtMintEvent,
    ScaledTokenBurnResult,
    ScaledTokenMintResult,
    WadRayMathLibrary,
)
from degenbot.aave.processors.factory import TokenProcessorFactory
from degenbot.checksum_cache import get_checksum_address
from degenbot.cli import cli
from degenbot.cli.aave.db_assets import (
    get_asset_identifier,
    get_contract,
    get_gho_asset,
)
from degenbot.cli.aave.constants import (
    LIQUIDATION_OPERATION_TYPES,
    POSITION_RISK_DISPLAY_LIMIT,
    UserOperation,
)
from degenbot.cli.aave.db_positions import (
    get_or_create_collateral_position,
    get_or_create_debt_position,
    update_debt_position_index,
)
from degenbot.cli.aave.db_users import get_or_create_user
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
from degenbot.cli.aave.stkaave import get_or_init_stk_aave_balance
from degenbot.cli.aave.transaction_processor import _process_transaction
from degenbot.cli.aave.types import TransactionContext
from degenbot.cli.aave.utils import (
    _build_transaction_contexts,
    _fetch_discount_token_from_contract,
    _get_all_scaled_token_addresses,
    _get_scaled_token_asset_by_address,
)
from degenbot.cli.aave.verification import (
    cleanup_zero_balance_positions,
    verify_all_positions,
    verify_positions_for_users,
)
from degenbot.cli.aave.erc20_utils import _get_or_create_erc20_token
from degenbot.cli.aave_transaction_operations import Operation, ScaledTokenEvent
from degenbot.cli.aave_utils import decode_address
from degenbot.cli.utils import get_web3_from_config
from degenbot.constants import ZERO_ADDRESS
from degenbot.database import db_session
from degenbot.database.models.aave import (
    AaveGhoToken,
    AaveV3Asset,
    AaveV3CollateralPosition,
    AaveV3DebtPosition,
    AaveV3Contract,
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
            gho_asset_token = _get_or_create_erc20_token(
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
    market_name: str = "aave_v3",
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


def _process_scaled_token_operation(
    event: CollateralMintEvent | CollateralBurnEvent | DebtMintEvent | DebtBurnEvent,
    scaled_token_revision: int,
    position: AaveV3CollateralPosition | AaveV3DebtPosition,
) -> UserOperation:
    """
    Determine the user operation for scaled token events and apply the appropriate delta to the
    position balance.

    This function delegates to revision-specific processors for handling token events.

    Args:
        event: The scaled token event data
        scaled_token_revision: The token contract revision
        position: The user's position to update
    """

    # Determine token type for logging
    token_type = (
        "aToken" if isinstance(event, (CollateralMintEvent, CollateralBurnEvent)) else "vToken"
    )
    logger.debug(
        f"Processing scaled token operation ({type(event).__name__}) for {token_type} revision "
        f"{scaled_token_revision}"
    )
    logger.debug(position)

    match event:
        case CollateralMintEvent():
            assert isinstance(position, AaveV3CollateralPosition)
            collateral_processor = TokenProcessorFactory.get_collateral_processor(
                scaled_token_revision
            )
            mint_result: ScaledTokenMintResult = collateral_processor.process_mint_event(
                event_data=event,
                previous_balance=position.balance,
                previous_index=position.last_index or 0,
                scaled_delta=event.scaled_amount,
            )
            position.balance += mint_result.balance_delta
            # Only update last_index if the new index is greater than current
            # This prevents earlier events (in log index order) from overwriting
            # later events' indices when operations are processed out of order
            if mint_result.new_index > (position.last_index or 0):
                position.last_index = mint_result.new_index
            return UserOperation.WITHDRAW if mint_result.is_repay else UserOperation.DEPOSIT

        case CollateralBurnEvent():
            assert isinstance(position, AaveV3CollateralPosition)
            collateral_processor = TokenProcessorFactory.get_collateral_processor(
                scaled_token_revision
            )
            burn_result: ScaledTokenBurnResult = collateral_processor.process_burn_event(
                event_data=event,
                previous_balance=position.balance,
                previous_index=position.last_index or 0,
                scaled_delta=event.scaled_amount,
            )
            logger.debug(
                f"_process_scaled_token_operation burn: delta={burn_result.balance_delta}, "
                f"new_balance={position.balance + burn_result.balance_delta}"
            )
            position.balance += burn_result.balance_delta
            # Only update last_index if the new index is greater than current
            # This prevents earlier events (in log index order) from overwriting
            # later events' indices when operations are processed out of order
            if burn_result.new_index > (position.last_index or 0):
                position.last_index = burn_result.new_index
            return UserOperation.WITHDRAW

        case DebtMintEvent():
            assert isinstance(position, AaveV3DebtPosition)
            debt_processor = TokenProcessorFactory.get_debt_processor(scaled_token_revision)
            debt_mint_result: ScaledTokenMintResult = debt_processor.process_mint_event(
                event_data=event,
                previous_balance=position.balance,
                previous_index=position.last_index or 0,
                scaled_delta=event.scaled_amount,
            )
            position.balance += debt_mint_result.balance_delta
            # Only update last_index if the new index is greater than current
            # This prevents earlier events (in log index order) from overwriting
            # later events' indices when operations are processed out of order
            if debt_mint_result.new_index > (position.last_index or 0):
                position.last_index = debt_mint_result.new_index
            return UserOperation.REPAY if debt_mint_result.is_repay else UserOperation.BORROW

        case DebtBurnEvent():
            assert isinstance(position, AaveV3DebtPosition)
            debt_processor = TokenProcessorFactory.get_debt_processor(scaled_token_revision)
            debt_burn_result: ScaledTokenBurnResult = debt_processor.process_burn_event(
                event_data=event,
                previous_balance=position.balance,
                previous_index=position.last_index or 0,
                scaled_delta=event.scaled_amount,
            )
            position.balance += debt_burn_result.balance_delta
            # Only update last_index if the new index is greater than current
            # This prevents earlier events (in log index order) from overwriting
            # later events' indices when operations are processed out of order
            if debt_burn_result.new_index > (position.last_index or 0):
                position.last_index = debt_burn_result.new_index
            return UserOperation.REPAY


def calculate_gho_discount_rate(
    debt_balance: int,
    discount_token_balance: int,
) -> int:
    """
    Calculate the GHO discount rate locally.

    Delegates to GhoMath.calculate_discount_rate which mirrors the logic from
    the GhoDiscountRateStrategy contract at mainnet address
    0x4C38Ec4D1D2068540DfC11DFa4de41F733DDF812.

    Returns the discount rate in basis points (10000 = 100.00%).
    """
    return GhoMath.calculate_discount_rate(
        debt_balance=debt_balance,
        discount_token_balance=discount_token_balance,
    )


def _refresh_discount_rate(
    *,
    user: AaveV3User,
    has_discount_rate_strategy: bool,
    discount_token_balance: int,
    scaled_debt_balance: int,
    debt_index: int,
    wad_ray_math: WadRayMathLibrary,
) -> None:
    """
    Calculate and update the user's GHO discount rate.

    Calculates the debt token balance from scaled balance and index, then
    computes the discount rate locally using the same logic as the GhoDiscountRateStrategy
    contract.
    """

    # Skip if discount mechanism is not supported (revision 4+)
    if not has_discount_rate_strategy:
        return

    debt_token_balance = wad_ray_math.ray_mul(
        a=scaled_debt_balance,
        b=debt_index,
    )
    user.gho_discount = calculate_gho_discount_rate(
        debt_balance=debt_token_balance,
        discount_token_balance=discount_token_balance,
    )


def _preprocess_liquidation_aggregates(
    tx_context: TransactionContext,
    operations: list["Operation"],
) -> None:
    """
    Preprocess liquidations to detect patterns and prepare for processing.

    Detects whether multiple liquidations share the same debt asset and
    determines if they use combined or separate burn events.

    See debug/aave/0056 and debug/aave/0065 for pattern details.
    """
    tx_context.liquidation_patterns = detect_liquidation_patterns(
        operations=operations,
        scaled_token_events=tx_context.scaled_token_events,
        get_v_token_for_underlying=lambda addr: _get_v_token_for_underlying(
            session=tx_context.session,
            market=tx_context.market,
            underlying_address=addr,
        ),
    )


def _get_v_token_for_underlying(
    session: Session,
    market: AaveV3Market,
    underlying_address: ChecksumAddress,
) -> ChecksumAddress | None:
    """Get vToken address for an underlying asset."""
    asset = session.scalar(
        select(AaveV3Asset).where(
            AaveV3Asset.market_id == market.id,
            AaveV3Asset.underlying_token.has(address=underlying_address),
        )
    )
    if asset is None or asset.v_token is None:
        return None
    return asset.v_token.address


def _process_operation(
    *,
    operation: Operation,
    tx_context: TransactionContext,
) -> None:
    """
    Process a single operation.
    """

    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing operation {operation.operation_id}: "
        f"{operation.operation_type.name}"
    )

    # Skip stkAAVE transfers - they're pre-processed separately before operations
    # to ensure stkAAVE balances are up-to-date when GHO operations calculate
    # discount rates. They should not be processed again here.
    if operation.operation_type == OperationType.STKAAVE_TRANSFER:
        return

    # Handle DEFICIT_COVERAGE operations specially
    # These have paired Transfer + Burn events that must be processed atomically
    if operation.operation_type == OperationType.DEFICIT_COVERAGE:
        _process_deficit_coverage_operation(
            operation=operation,
            tx_context=tx_context,
        )
        return

    # Create enricher for this operation
    enricher = ScaledEventEnricher(
        pool_revision=tx_context.pool_revision,
        token_revisions={},
        session=tx_context.session,
    )

    # Process each scaled token event in the operation
    # Sort by log index to ensure events are processed in chronological order
    sorted_scaled_events = sorted(
        operation.scaled_token_events,
        key=lambda e: e.event["logIndex"],
    )
    for scaled_event in sorted_scaled_events:
        event = scaled_event.event

        # Enrich the scaled event with calculated amounts
        enriched_event = enricher.enrich(scaled_event, operation)
        if scaled_event.event_type == ScaledTokenEventType.COLLATERAL_MINT:
            # Special case: When interest exceeds withdrawal amount, the aToken contract
            # emits a Mint event instead of a Burn event (AToken rev_4.sol:2836-2839).
            # This happens when nextBalance > previousBalance after burning.
            # Detection: amount < balance_increase indicates the withdrawal/repayment was less
            # than interest. In this case, we should treat it as a burn (subtract from balance),
            # not a mint.
            if (
                operation.operation_type == OperationType.WITHDRAW
                and scaled_event.balance_increase is not None
                and scaled_event.amount < scaled_event.balance_increase
            ):
                logger.debug(
                    f"WITHDRAW: Treating COLLATERAL_MINT as burn - interest exceeds withdrawal "
                    f"(amount={scaled_event.amount}, "
                    f"balance_increase={scaled_event.balance_increase})"
                )
                _process_collateral_burn_with_match(
                    event=event,
                    tx_context=tx_context,
                    scaled_event=scaled_event,
                    enriched_event=enriched_event,
                )
            elif (
                # Special case: In REPAY_WITH_ATOKENS, when interest exceeds repayment,
                # the Mint event's amount field represents net interest
                # (balance_increase - repay_amount). Treat as burn.
                operation.operation_type == OperationType.REPAY_WITH_ATOKENS
                and scaled_event.balance_increase is not None
                and scaled_event.amount < scaled_event.balance_increase
            ):
                logger.debug(
                    f"REPAY_WITH_ATOKENS: Treating COLLATERAL_MINT as burn - "
                    f"interest exceeds repayment (amount={scaled_event.amount}, "
                    f"balance_increase={scaled_event.balance_increase})"
                )
                _process_collateral_burn_with_match(
                    event=event,
                    tx_context=tx_context,
                    scaled_event=scaled_event,
                    enriched_event=enriched_event,
                )
            else:
                _process_collateral_mint_with_match(
                    event=event,
                    tx_context=tx_context,
                    operation=operation,
                    scaled_event=scaled_event,
                    enriched_event=enriched_event,
                )
        elif scaled_event.event_type == ScaledTokenEventType.COLLATERAL_BURN:
            _process_collateral_burn_with_match(
                event=event,
                tx_context=tx_context,
                scaled_event=scaled_event,
                enriched_event=enriched_event,
            )
        elif scaled_event.event_type in {
            ScaledTokenEventType.DEBT_MINT,
            ScaledTokenEventType.GHO_DEBT_MINT,
        }:
            _process_debt_mint_with_match(
                event=event,
                tx_context=tx_context,
                operation=operation,
                scaled_event=scaled_event,
                enriched_event=enriched_event,
            )
        elif scaled_event.event_type in {
            ScaledTokenEventType.DEBT_BURN,
            ScaledTokenEventType.GHO_DEBT_BURN,
        }:
            _process_debt_burn_with_match(
                event=event,
                tx_context=tx_context,
                operation=operation,
                scaled_event=scaled_event,
                enriched_event=enriched_event,
            )
        elif scaled_event.event_type in {
            ScaledTokenEventType.COLLATERAL_TRANSFER,
            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
        }:
            _process_collateral_transfer(
                tx_context=tx_context,
                operation=operation,
                scaled_event=scaled_event,
            )
        elif scaled_event.event_type in {
            ScaledTokenEventType.DEBT_TRANSFER,
            ScaledTokenEventType.GHO_DEBT_TRANSFER,
            ScaledTokenEventType.ERC20_DEBT_TRANSFER,
        }:
            _process_debt_transfer(
                event=event,
                tx_context=tx_context,
                operation=operation,
                scaled_event=scaled_event,
            )
        elif scaled_event.event_type == ScaledTokenEventType.DISCOUNT_TRANSFER:
            # stkAAVE transfers are processed separately to update user balances
            # before GHO debt operations calculate discount rates. They don't
            # affect Aave market positions directly.
            pass
        else:
            msg = f"Unknown event type: {scaled_event.event_type}"
            raise ValueError(msg)


def _calculate_mint_to_treasury_scaled_amount(
    scaled_event: ScaledTokenEvent,
    operation: Operation,
) -> int:
    """Calculate scaled amount for MINT_TO_TREASURY operations.

    Delegates to PoolMath for revision-aware calculation. This ensures
    the correct rounding mode is used based on Pool revision.

    Args:
        scaled_event: The scaled token Mint event
        operation: The operation containing minted_to_treasury_amount

    Returns:
        The calculated scaled amount to add to the treasury position
    """
    assert scaled_event.balance_increase is not None
    assert scaled_event.index is not None

    # Get the underlying amount to mint
    # Use MintedToTreasury event if available, otherwise calculate from Mint event
    if operation.minted_to_treasury_amount is not None:
        minted_amount = operation.minted_to_treasury_amount
    else:
        # Mint event value = actual_amount + balance_increase (interest on existing balance)
        minted_amount = scaled_event.amount - scaled_event.balance_increase

    # When minted_amount is 0, only interest accrued (no new tokens minted)
    if minted_amount == 0:
        return 0

    # Use PoolMath for revision-aware calculation
    return PoolMath.underlying_to_scaled_collateral(
        underlying_amount=minted_amount,
        liquidity_index=scaled_event.index,
        pool_revision=operation.pool_revision,
    )


def _process_deficit_coverage_operation(
    *,
    operation: Operation,
    tx_context: TransactionContext,
) -> None:
    """
    Process DEFICIT_COVERAGE operations atomically.

    DEFICIT_COVERAGE operations contain paired Transfer + Burn events that occur
    during Umbrella protocol's deficit coverage operations. These must be processed
    atomically (credit then debit) to maintain correct balances.

    The pattern is:
    1. Transfer/BalanceTransfer credits user's collateral position
    2. Burn debits user's collateral position (including accrued interest)
    3. Net effect should be zero or the interest amount
    """
    # Sort events by log index to ensure chronological processing
    sorted_events = sorted(
        operation.scaled_token_events,
        key=lambda e: e.event["logIndex"],
    )

    # Process transfer events first (credit the user)
    for scaled_event in sorted_events:
        if scaled_event.event_type in {
            ScaledTokenEventType.COLLATERAL_TRANSFER,
            ScaledTokenEventType.ERC20_COLLATERAL_TRANSFER,
        }:
            _process_collateral_transfer(
                tx_context=tx_context,
                operation=operation,
                scaled_event=scaled_event,
            )

    # Process burn events last (debit the user)
    for scaled_event in sorted_events:
        if scaled_event.event_type == ScaledTokenEventType.COLLATERAL_BURN:
            # Skip enrichment validation for deficit coverage burns
            # The burn amount may not match standard calculations because
            # it includes interest accrued during the deficit coverage
            _process_deficit_coverage_burn(
                tx_context=tx_context,
                scaled_event=scaled_event,
            )


def _process_deficit_coverage_burn(
    *,
    tx_context: TransactionContext,
    scaled_event: ScaledTokenEvent,
) -> None:
    """
    Process a burn event within a DEFICIT_COVERAGE operation.

    Unlike regular burns, deficit coverage burns don't need enrichment validation
    because the amount includes interest that was accrued between the transfer
    and the burn within the same transaction.
    """
    # Skip if user address is missing
    if scaled_event.user_address is None:
        return

    # Get collateral asset
    token_address = scaled_event.event["address"]
    collateral_asset, _ = _get_scaled_token_asset_by_address(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
    )

    assert collateral_asset

    # Get user
    user = get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get collateral position
    collateral_position = get_or_create_collateral_position(
        tx_context=tx_context,
        user=user,
        asset_id=collateral_asset.id,
    )

    # Calculate scaled amount directly without enrichment validation
    # The raw amount needs to be converted to scaled amount
    assert scaled_event.index is not None
    token_math = TokenMathFactory.get_token_math_for_token_revision(
        collateral_asset.a_token_revision
    )
    scaled_amount = token_math.get_collateral_burn_scaled_amount(
        amount=scaled_event.amount,
        liquidity_index=scaled_event.index,
    )

    # Process the burn directly
    assert scaled_event.balance_increase is not None
    _process_scaled_token_operation(
        event=CollateralBurnEvent(
            value=scaled_event.amount,
            balance_increase=scaled_event.balance_increase,
            index=scaled_event.index,
            scaled_amount=scaled_amount,
        ),
        scaled_token_revision=collateral_asset.a_token_revision,
        position=collateral_position,
    )

    # Update last_index if the new index is greater
    current_index = collateral_position.last_index or 0
    if scaled_event.index > current_index:
        collateral_position.last_index = scaled_event.index


def _process_deferred_debt_burns(
    *,
    tx_context: TransactionContext,
    liquidation_operations: list[Operation],
    assigned_log_indices: set[int],
) -> None:
    """
    Process debt burns that couldn't be matched during initial operation parsing.

    This handles the case where Burn events are emitted BEFORE LiquidationCall events
    in Aave V3. The protocol emits events in this order:
    1. Reserve state update
    2. Debt token burn
    3. Collateral token operations
    4. LiquidationCall event (at the end)

    Since operations are parsed from Pool events, the burn event may not be matched
    to a liquidation if the burn has a lower log index than the LiquidationCall.

    This function finds unassigned debt burns and matches them to liquidation operations
    retrospectively using semantic matching (user + debt asset).

    See debug/aave/0060 for detailed analysis.
    """
    if not liquidation_operations:
        return

    # Find all unassigned debt burn events
    for event in tx_context.events:
        if event["logIndex"] in assigned_log_indices:
            continue

        topic = event["topics"][0]
        event_address = event["address"]

        # Check if this is a debt burn event
        if topic != AaveV3ScaledTokenEvent.BURN.value:
            continue

        # Decode burn event to get user and amount
        from_addr = decode_address(event["topics"][1])
        target = decode_address(event["topics"][2])
        amount, balance_increase, index = eth_abi.abi.decode(
            types=["uint256", "uint256", "uint256"],
            data=event["data"],
        )

        # Find matching liquidation operation
        matching_operation = _find_matching_liquidation_for_burn(
            user_address=from_addr,
            burn_token_address=event_address,
            liquidation_operations=liquidation_operations,
            tx_context=tx_context,
        )

        if matching_operation is None:
            continue

        # Create scaled event from the burn
        scaled_event = ScaledTokenEvent(
            event=event,
            event_type=ScaledTokenEventType.DEBT_BURN,
            user_address=from_addr,
            caller_address=None,
            from_address=from_addr,
            target_address=target,
            amount=amount,
            balance_increase=balance_increase,
            index=index,
        )

        # Enrich and process the burn
        enricher = ScaledEventEnricher(
            pool_revision=tx_context.pool_revision,
            token_revisions={},
            session=tx_context.session,
        )
        enriched_event = enricher.enrich(scaled_event, matching_operation)

        pool_log_idx = (
            matching_operation.pool_event["logIndex"] if matching_operation.pool_event else "N/A"
        )
        logger.debug(
            f"Processing deferred debt burn at logIndex {event['logIndex']} "
            f"for liquidation at logIndex {pool_log_idx}"
        )

        _process_debt_burn_with_match(
            event=event,
            tx_context=tx_context,
            operation=matching_operation,
            scaled_event=scaled_event,
            enriched_event=enriched_event,
        )

        # Mark as assigned
        assigned_log_indices.add(event["logIndex"])


def _find_matching_liquidation_for_burn(
    *,
    user_address: ChecksumAddress,
    burn_token_address: ChecksumAddress,
    liquidation_operations: list[Operation],
    tx_context: TransactionContext,
) -> Operation | None:
    """
    Find a liquidation operation that matches a debt burn event.

    Matching is based on:
    1. User address must match
    2. Debt asset (vToken) must match

    This uses semantic matching since the burn event may have been emitted
    before the LiquidationCall event.
    """
    # Get the debt asset for this burn token
    _, debt_asset = _get_scaled_token_asset_by_address(
        session=tx_context.session,
        market=tx_context.market,
        token_address=burn_token_address,
    )

    if debt_asset is None:
        return None

    # Find matching liquidation operation
    for op in liquidation_operations:
        if op.pool_event is None:
            continue

        # Check if user matches
        liquidation_user = decode_address(op.pool_event["topics"][3])
        if liquidation_user != user_address:
            continue

        # Check if debt asset matches
        debt_asset_addr = decode_address(op.pool_event["topics"][2])

        # Get vToken address for the debt asset
        debt_v_token = _get_v_token_for_underlying(
            session=tx_context.session,
            market=tx_context.market,
            underlying_address=debt_asset_addr,
        )

        if debt_v_token is None:
            continue

        if debt_v_token == burn_token_address:
            return op

    return None


def _process_collateral_mint_with_match(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
    operation: Operation,
    scaled_event: ScaledTokenEvent,
    enriched_event: EnrichedScaledTokenEvent,
) -> None:
    """
    Process collateral (aToken) mint with operation match.
    """

    token_address = scaled_event.event["address"]
    collateral_asset, _ = _get_scaled_token_asset_by_address(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
    )

    assert collateral_asset

    asset_identifier = get_asset_identifier(collateral_asset)
    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing {asset_identifier} collateral mint "
        f"at block {event['blockNumber']}"
    )

    user = get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get or create collateral position
    collateral_position = get_or_create_collateral_position(
        tx_context=tx_context,
        user=user,
        asset_id=collateral_asset.id,
    )

    # Use enriched event data for scaled amount, or calculate for MINT_TO_TREASURY
    if operation.operation_type == OperationType.MINT_TO_TREASURY:
        # MINT_TO_TREASURY uses MintedToTreasury event amount
        scaled_amount = _calculate_mint_to_treasury_scaled_amount(
            scaled_event=scaled_event,
            operation=operation,
        )
    else:
        assert enriched_event.scaled_amount is not None
        scaled_amount = enriched_event.scaled_amount

    # Ensure required fields are present for CollateralMintEvent
    assert scaled_event.balance_increase is not None
    assert scaled_event.index is not None
    assert scaled_amount is not None

    _process_scaled_token_operation(
        event=CollateralMintEvent(
            value=scaled_event.amount,
            balance_increase=scaled_event.balance_increase,
            index=scaled_event.index,
            scaled_amount=scaled_amount,
        ),
        scaled_token_revision=collateral_asset.a_token_revision,
        position=collateral_position,
    )

    # Update last_index
    current_index = collateral_position.last_index or 0
    if scaled_event.index > 0 and scaled_event.index > current_index:
        collateral_position.last_index = scaled_event.index


def _process_collateral_burn_with_match(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
    scaled_event: ScaledTokenEvent,
    enriched_event: EnrichedScaledTokenEvent,
) -> None:
    """
    Process collateral (aToken) burn with operation match.
    """

    # Skip if user address is missing
    if scaled_event.user_address is None:
        return

    # Get collateral asset first for logging
    token_address = scaled_event.event["address"]
    collateral_asset, _ = _get_scaled_token_asset_by_address(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
    )

    assert collateral_asset

    asset_identifier = get_asset_identifier(collateral_asset)
    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing {asset_identifier} collateral burn "
        f"at block {event['blockNumber']}"
    )

    # Get user
    user = get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get collateral position
    collateral_position = get_or_create_collateral_position(
        tx_context=tx_context,
        user=user,
        asset_id=collateral_asset.id,
    )

    # Use enriched event data for scaled amount
    # Enrichment layer reliably calculates scaled_amount for all burn events
    scaled_amount: int | None = enriched_event.scaled_amount
    raw_amount = enriched_event.raw_amount

    # Fallback calculation only if enrichment didn't provide scaled_amount
    # This should not happen for normal burns, but provides a safety net
    if scaled_amount is None and raw_amount is not None:
        token_math = TokenMathFactory.get_token_math_for_token_revision(
            collateral_asset.a_token_revision
        )
        assert scaled_event.index is not None
        scaled_amount = token_math.get_collateral_burn_scaled_amount(
            amount=raw_amount,
            liquidity_index=scaled_event.index,
        )

    assert scaled_event.balance_increase is not None
    assert scaled_event.index is not None
    _process_scaled_token_operation(
        event=CollateralBurnEvent(
            value=scaled_event.amount,
            balance_increase=scaled_event.balance_increase,
            index=scaled_event.index,
            scaled_amount=scaled_amount,
        ),
        scaled_token_revision=collateral_asset.a_token_revision,
        position=collateral_position,
    )
    logger.debug(
        f"After burn position id={id(collateral_position)}, balance={collateral_position.balance}"
    )

    # Only update last_index if the new index is greater than current
    # This prevents earlier events (in log index order) from overwriting
    # later events' indices when operations are processed out of order
    current_index = collateral_position.last_index or 0
    if scaled_event.index > current_index:
        collateral_position.last_index = scaled_event.index


def _process_debt_mint_with_match(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
    operation: Operation,
    scaled_event: ScaledTokenEvent,
    enriched_event: EnrichedScaledTokenEvent,
) -> None:
    """
    Process debt (vToken) mint with operation match.

    Note: In REPAY operations, a Mint event is emitted when interest > repayment.
    In this case, the Mint event represents the net effect of:
    1. Interest accrual (increasing debt)
    2. Debt repayment (burning scaled tokens)
    The actual scaled burn amount = balance_increase - amount.
    """

    # Get debt asset first for logging
    token_address = scaled_event.event["address"]
    _, debt_asset = _get_scaled_token_asset_by_address(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
    )

    assert debt_asset

    asset_identifier = get_asset_identifier(debt_asset)
    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing {asset_identifier} debt mint "
        f"at block {event['blockNumber']}"
    )

    user = get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get or create debt position
    debt_position = get_or_create_debt_position(
        tx_context=tx_context,
        user=user,
        asset_id=debt_asset.id,
    )

    # Check if this is a GHO token first (needed for INTEREST_ACCRUAL handling)
    is_gho = tx_context.is_gho_vtoken(token_address)

    # INTEREST_ACCRUAL operations: For non-GHO tokens, Mint events are tracking-only
    # For GHO tokens, we still need to process through the GHO processor to apply discounts
    if operation.operation_type == OperationType.INTEREST_ACCRUAL and not is_gho:
        logger.debug(
            "_process_debt_mint_with_match: INTEREST_ACCRUAL - skipping balance change, "
            "updating index"
        )
        if scaled_event.index is not None:
            current_index = debt_position.last_index or 0
            if scaled_event.index > current_index:
                debt_position.last_index = scaled_event.index
                logger.debug(f"Updated last_index for INTEREST_ACCRUAL: {debt_position.last_index}")
        return

    # Use enriched event data for scaled amount
    scaled_amount: int | None = enriched_event.scaled_amount

    # Process GHO tokens through GHO-specific processor (handles discounts for all operations)
    if is_gho:
        # Use the effective discount from transaction context
        effective_discount = tx_context.user_discounts.get(user.address, user.gho_discount)

        # Process using GHO-specific processor
        gho_processor = TokenProcessorFactory.get_gho_debt_processor(debt_asset.v_token_revision)
        assert scaled_event.balance_increase is not None
        assert scaled_event.index is not None

        # For GHO_REPAY operations, extract the actual repay amount from the Repay event
        # to avoid 1 wei rounding errors from deriving from Mint event fields.
        # The processor will handle the full logic including discount calculations.
        # See debug/aave/0037 and 0038 for details.
        actual_repay_amount: int | None = None
        if operation.operation_type == OperationType.GHO_REPAY:
            assert operation.pool_event is not None
            # Repay event: Repay(address indexed reserve, address indexed user,
            #   address indexed repayer, uint256 amount, bool useATokens)
            repay_amount_data, _ = eth_abi.abi.decode(
                types=["uint256", "bool"],
                data=operation.pool_event["data"],
            )
            actual_repay_amount = repay_amount_data

        gho_result = gho_processor.process_mint_event(
            event_data=DebtMintEvent(
                caller=scaled_event.caller_address or scaled_event.user_address,
                on_behalf_of=scaled_event.user_address,
                value=scaled_event.amount,
                balance_increase=scaled_event.balance_increase,
                index=scaled_event.index,
                scaled_amount=scaled_amount,
            ),
            previous_balance=debt_position.balance,
            previous_index=debt_position.last_index or 0,
            previous_discount=effective_discount,
            actual_repay_amount=actual_repay_amount,
        )

        # Apply the calculated balance delta
        debt_position.balance += gho_result.balance_delta
        update_debt_position_index(
            tx_context=tx_context,
            debt_asset=debt_asset,
            debt_position=debt_position,
            event_index=scaled_event.index,
            event_block_number=scaled_event.event["blockNumber"],
        )

        # Refresh discount if needed
        if (
            gho_result.should_refresh_discount
            and tx_context.gho_asset.v_gho_discount_token is not None
        ):
            discount_token_balance = get_or_init_stk_aave_balance(
                user=user,
                tx_context=tx_context,
                log_index=scaled_event.event["logIndex"],
            )
            assert debt_position.last_index is not None
            _refresh_discount_rate(
                user=user,
                has_discount_rate_strategy=tx_context.gho_asset.v_gho_discount_rate_strategy
                is not None,
                discount_token_balance=discount_token_balance,
                scaled_debt_balance=debt_position.balance,
                debt_index=debt_position.last_index,
                wad_ray_math=gho_processor.get_math_libraries()["wad_ray"],
            )
    else:
        # Use standard debt processor for non-GHO tokens
        assert scaled_event.balance_increase is not None
        assert scaled_event.index is not None

        # Check if this Mint event is part of a REPAY or LIQUIDATION operation
        # In REPAY/LIQUIDATION, Mint is emitted when interest > repayment, but the net effect
        # is still a burn of scaled tokens
        if operation.operation_type in {
            OperationType.GHO_REPAY,
            OperationType.REPAY,
            OperationType.REPAY_WITH_ATOKENS,
            OperationType.LIQUIDATION,
            OperationType.GHO_LIQUIDATION,
        }:
            # For liquidations, check pattern to determine if Mint events should be skipped.
            # COMBINED_BURN (Issue 0056): Multiple liquidations share one burn event.
            #   Skip Mint events - the aggregated burn handles all debt reduction.
            # SEPARATE_BURNS (Issue 0065): Each liquidation has its own burn event.
            #   Process Mint events normally as they represent individual liquidations.
            # SINGLE: Standard single liquidation, process Mint normally.
            if operation.operation_type in LIQUIDATION_OPERATION_TYPES:
                liquidation_key = (user.address, token_address)
                pattern = tx_context.liquidation_patterns.get_pattern(user.address, token_address)

                # Only skip Mint events for COMBINED_BURN pattern
                if pattern == LiquidationPattern.COMBINED_BURN:
                    logger.debug(
                        f"_process_debt_mint_with_match: COMBINED_BURN pattern - "
                        f"skipping Mint event for {liquidation_key} (handled by aggregated burn)"
                    )
                    return
                # For SINGLE and SEPARATE_BURNS, process the Mint event normally

            # Treat as burn: calculate actual scaled burn amount from Pool event
            # Use TokenMath to match on-chain calculation
            assert operation.pool_event is not None

            # Decode the amount based on operation type
            # REPAY: (uint256 amount, bool useATokens)
            # LIQUIDATION: (uint256 debtToCover, uint256 liquidatedCollateralAmount,
            #              address liquidator, bool receiveAToken)
            if operation.operation_type in {
                OperationType.GHO_REPAY,
                OperationType.REPAY_WITH_ATOKENS,
                OperationType.REPAY,
            }:
                repay_amount, _ = eth_abi.abi.decode(
                    types=["uint256", "bool"],
                    data=operation.pool_event["data"],
                )
            elif operation.operation_type in {
                OperationType.GHO_LIQUIDATION,
                OperationType.LIQUIDATION,
            }:
                repay_amount, _, _, _ = eth_abi.abi.decode(
                    types=["uint256", "uint256", "address", "bool"],
                    data=operation.pool_event["data"],
                )
            else:
                msg = f"Unhandled operation type {operation.operation_type}"
                raise ValueError(msg)

            # Use token revision (not pool revision) to get correct TokenMath
            token_math = TokenMathFactory.get_token_math_for_token_revision(
                debt_asset.v_token_revision
            )
            actual_scaled_burn = token_math.get_debt_burn_scaled_amount(
                repay_amount, scaled_event.index
            )
            logger.debug(
                f"{operation.operation_type.name} with Mint event: treating as burn, "
                f"amount={repay_amount}, scaled_burn={actual_scaled_burn}"
            )
            _process_scaled_token_operation(
                event=DebtBurnEvent(
                    from_=scaled_event.user_address,
                    target=ZERO_ADDRESS,
                    value=actual_scaled_burn,
                    balance_increase=scaled_event.balance_increase,
                    index=scaled_event.index,
                    scaled_amount=actual_scaled_burn,  # Pass the correctly calculated scaled burn
                ),
                scaled_token_revision=debt_asset.v_token_revision,
                position=debt_position,
            )
        else:
            logger.debug("_process_debt_mint_with_match: handling as borrow/mint")
            _process_scaled_token_operation(
                event=DebtMintEvent(
                    caller=scaled_event.caller_address or scaled_event.user_address,
                    on_behalf_of=scaled_event.user_address,
                    value=scaled_event.amount,
                    balance_increase=scaled_event.balance_increase,
                    index=scaled_event.index,
                    scaled_amount=scaled_amount,
                ),
                scaled_token_revision=debt_asset.v_token_revision,
                position=debt_position,
            )

        update_debt_position_index(
            tx_context=tx_context,
            debt_asset=debt_asset,
            debt_position=debt_position,
            event_index=scaled_event.index,
            event_block_number=scaled_event.event["blockNumber"],
        )


def _is_bad_debt_liquidation(user: AaveV3User, tx_context: TransactionContext) -> bool:
    """
    Check if this transaction contains a bad debt liquidation for the user.

    Bad debt liquidations emit a DEFICIT_CREATED event for the user, indicating
    the protocol is writing off debt that cannot be covered by collateral.

    Event definition:
        event DeficitCreated(
            address indexed user,
            address indexed debtAsset,
            uint256 amountCreated
        );
    """

    for evt in tx_context.events:
        if evt["topics"][0] == AaveV3PoolEvent.DEFICIT_CREATED.value:
            deficit_user = decode_address(evt["topics"][1])

            if deficit_user == user.address:
                return True
    return False


def _process_debt_burn_with_match(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
    operation: Operation,
    scaled_event: ScaledTokenEvent,
    enriched_event: EnrichedScaledTokenEvent,
) -> None:
    """
    Process debt (vToken) burn with operation match.
    """

    # Get debt asset first for logging
    token_address = scaled_event.event["address"]
    _, debt_asset = _get_scaled_token_asset_by_address(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
    )

    assert debt_asset is not None

    asset_identifier = get_asset_identifier(debt_asset)
    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing {asset_identifier} debt burn "
        f"at block {event['blockNumber']}"
    )

    user = get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get debt position
    debt_position = get_or_create_debt_position(
        tx_context=tx_context,
        user=user,
        asset_id=debt_asset.id,
    )

    # Use enriched event data for scaled amount
    scaled_amount: int | None = enriched_event.scaled_amount

    # Check for bad debt liquidation first - applies to both GHO and non-GHO tokens
    # Bad debt liquidations emit a DEFICIT_CREATED event and burn the FULL debt balance
    if (
        operation
        and operation.operation_type in LIQUIDATION_OPERATION_TYPES
        and _is_bad_debt_liquidation(user, tx_context)
    ):
        # Bad debt liquidation: The contract burns the ENTIRE debt balance
        # not just the debtToCover amount. The debt position should be set to 0.
        # This is because the protocol writes off the bad debt.
        old_balance = debt_position.balance
        debt_position.balance = 0
        logger.debug(
            f"_process_debt_burn_with_match: BAD DEBT LIQUIDATION - setting balance to 0 "
            f"(was {old_balance})"
        )
        # Skip the normal processing since we've already set the balance
        # Only update last_index if the new index is greater than current
        if scaled_event.index is not None:
            current_index = debt_position.last_index or 0
            if scaled_event.index > current_index:
                debt_position.last_index = scaled_event.index
        return

    # Check if this is a GHO token and use GHO-specific processing
    if tx_context.is_gho_vtoken(token_address):
        # Use the effective discount from transaction context
        effective_discount = tx_context.user_discounts.get(user.address, user.gho_discount)

        # Process using GHO-specific processor
        gho_processor = TokenProcessorFactory.get_gho_debt_processor(debt_asset.v_token_revision)
        assert scaled_event.balance_increase is not None
        assert scaled_event.index is not None
        gho_result = gho_processor.process_burn_event(
            event_data=DebtBurnEvent(
                from_=scaled_event.from_address or scaled_event.user_address,
                target=scaled_event.target_address or scaled_event.user_address,
                value=scaled_event.amount,
                balance_increase=scaled_event.balance_increase,
                index=scaled_event.index,
                scaled_amount=scaled_amount,
            ),
            previous_balance=debt_position.balance,
            previous_index=debt_position.last_index or 0,
            previous_discount=effective_discount,
        )

        # Apply the calculated balance delta
        debt_position.balance += gho_result.balance_delta
        update_debt_position_index(
            tx_context=tx_context,
            debt_asset=debt_asset,
            debt_position=debt_position,
            event_index=scaled_event.index,
            event_block_number=scaled_event.event["blockNumber"],
        )

        # Refresh discount if needed
        if (
            gho_result.should_refresh_discount
            and tx_context.gho_asset.v_gho_discount_token is not None
        ):
            discount_token_balance = get_or_init_stk_aave_balance(
                user=user,
                tx_context=tx_context,
                log_index=scaled_event.event["logIndex"],
            )
            assert debt_position.last_index is not None
            current_index = debt_position.last_index
            _refresh_discount_rate(
                user=user,
                has_discount_rate_strategy=tx_context.gho_asset.v_gho_discount_rate_strategy
                is not None,
                discount_token_balance=discount_token_balance,
                scaled_debt_balance=debt_position.balance,
                debt_index=current_index,
                wad_ray_math=gho_processor.get_math_libraries()["wad_ray"],
            )
    else:
        # Use standard debt processor for non-GHO tokens
        assert scaled_event.balance_increase is not None
        assert scaled_event.index is not None
        logger.debug("_process_debt_burn_with_match: handling with standard debt processor")
        logger.debug(f"_process_debt_burn_with_match: scaled_event.amount = {scaled_event.amount}")
        logger.debug(
            f"_process_debt_burn_with_match: scaled_event.balance_increase = "
            f"{scaled_event.balance_increase}"
        )
        logger.debug(f"_process_debt_burn_with_match: scaled_event.index = {scaled_event.index}")

        if operation and operation.operation_type in LIQUIDATION_OPERATION_TYPES:
            liquidation_key = (user.address, token_address)
            pattern = tx_context.liquidation_patterns.get_pattern(user.address, token_address)

            if pattern is None:
                # Not in a liquidation group - shouldn't happen, but handle gracefully
                logger.warning(
                    f"_process_debt_burn_with_match: Burn event in liquidation "
                    f"but no pattern detected for {liquidation_key}"
                )
                burn_value = scaled_event.amount

            elif pattern == LiquidationPattern.SINGLE:
                # Standard single liquidation - use operation's debt_to_cover
                assert operation.debt_to_cover is not None
                debt_to_cover = operation.debt_to_cover

                token_math = TokenMathFactory.get_token_math_for_token_revision(
                    debt_asset.v_token_revision
                )
                burn_value = token_math.get_debt_burn_scaled_amount(
                    debt_to_cover, scaled_event.index
                )
                scaled_amount = burn_value

                logger.debug(
                    f"_process_debt_burn_with_match: SINGLE liquidation using "
                    f"debtToCover={debt_to_cover}, scaled_burn={burn_value}"
                )

            elif pattern == LiquidationPattern.COMBINED_BURN:
                # Issue 0056: Multiple liquidations share one burn event
                # Process once with aggregated amount
                if tx_context.liquidation_patterns.is_processed(user.address, token_address):
                    logger.debug(
                        f"_process_debt_burn_with_match: COMBINED_BURN already processed "
                        f"for {liquidation_key} - skipping"
                    )
                    return

                tx_context.liquidation_patterns.mark_processed(user.address, token_address)

                # Get aggregated amount from group
                group = tx_context.liquidation_patterns.get_group(user.address, token_address)
                assert group is not None
                total_debt = group.total_debt_to_cover

                token_math = TokenMathFactory.get_token_math_for_token_revision(
                    debt_asset.v_token_revision
                )
                burn_value = token_math.get_debt_burn_scaled_amount(total_debt, scaled_event.index)
                scaled_amount = burn_value

                logger.debug(
                    f"_process_debt_burn_with_match: COMBINED_BURN ({group.liquidation_count}x) "
                    f"using aggregated debtToCover={total_debt}, scaled_burn={burn_value}"
                )

            elif pattern == LiquidationPattern.SEPARATE_BURNS:
                # Issue 0065: Each liquidation has its own burn event
                # Process each burn individually using operation's debt_to_cover
                assert operation.debt_to_cover is not None
                debt_to_cover = operation.debt_to_cover

                token_math = TokenMathFactory.get_token_math_for_token_revision(
                    debt_asset.v_token_revision
                )
                burn_value = token_math.get_debt_burn_scaled_amount(
                    debt_to_cover, scaled_event.index
                )
                scaled_amount = burn_value

                logger.debug(
                    f"_process_debt_burn_with_match: SEPARATE_BURNS using "
                    f"debtToCover={debt_to_cover}, scaled_burn={burn_value}"
                )

            else:
                # Unknown pattern - fallback to event amount
                logger.error(
                    f"_process_debt_burn_with_match: Unknown pattern {pattern} "
                    f"for {liquidation_key}"
                )
                burn_value = scaled_event.amount
        else:
            # Standard REPAY: use Burn event value
            burn_value = scaled_event.amount
            logger.debug(f"_process_debt_burn_with_match: REPAY - using burn_value={burn_value}")

        _process_scaled_token_operation(
            event=DebtBurnEvent(
                from_=scaled_event.from_address or scaled_event.user_address,
                target=scaled_event.target_address or scaled_event.user_address,
                value=burn_value,
                balance_increase=scaled_event.balance_increase,
                index=scaled_event.index,
                scaled_amount=scaled_amount,
            ),
            scaled_token_revision=debt_asset.v_token_revision,
            position=debt_position,
        )

        update_debt_position_index(
            tx_context=tx_context,
            debt_asset=debt_asset,
            debt_position=debt_position,
            event_index=scaled_event.index,
            event_block_number=scaled_event.event["blockNumber"],
        )


def _should_skip_collateral_transfer(
    scaled_event: ScaledTokenEvent,
    operation: Operation | None,
    tx_context: TransactionContext,
) -> bool:
    """
    Determine if this collateral transfer event should be skipped.

    Returns True if:
    1. This is a paired BalanceTransfer handled by its paired ERC20 Transfer
    2. This is part of a REPAY_WITH_ATOKENS operation (burn handles it)
    3. This is a protocol mint (from zero address)
    4. This is a direct burn handled by Burn event
    5. This is an ERC20 Transfer in a liquidation operation (BalanceTransfer handles it)

    Special handling for liquidations:
    - BalanceTransfer events are NOT skipped (they contain the liquidation fees to treasury)
    - ERC20 Transfer events ARE skipped (only the BalanceTransfer represents the actual
      scaled balance movement to the treasury)
    """
    # Skip paired BalanceTransfer events - handled by their paired ERC20 Transfer
    # BUT: Don't skip for liquidation operations - the BalanceTransfer IS the transfer to treasury
    if (
        scaled_event.index is not None
        and scaled_event.index > 0
        and operation
        and operation.balance_transfer_events
        and operation.operation_type not in LIQUIDATION_OPERATION_TYPES
    ):
        for bt_event in operation.balance_transfer_events:
            if bt_event["logIndex"] == scaled_event.event["logIndex"]:
                return True

    # Skip REPAY_WITH_ATOKENS transfers (handled by burn event)
    if operation and operation.operation_type == OperationType.REPAY_WITH_ATOKENS:
        return True

    # Skip protocol mints (from zero address) - except standalone BALANCE_TRANSFER operations
    if scaled_event.from_address == ZERO_ADDRESS:
        # Keep BALANCE_TRANSFER operations (e.g., treasury fee collection), skip others
        return not (operation and operation.operation_type == OperationType.BALANCE_TRANSFER)

    # Skip ERC20 Transfers for liquidation operations - only process BalanceTransfer events
    # The BalanceTransfer events contain the liquidation fees to the treasury
    if (
        scaled_event.index is None
        and operation
        and operation.operation_type in LIQUIDATION_OPERATION_TYPES
    ):
        return True

    # Skip ERC20 transfers corresponding to direct burns (handled by Burn event)
    if scaled_event.index is None and scaled_event.target_address == ZERO_ADDRESS:
        gho_vtoken_address = tx_context.gho_vtoken_address
        for evt in tx_context.events:
            if evt["topics"][0] != AaveV3ScaledTokenEvent.BURN.value:
                continue
            # Skip GHO debt burns - collateral burns are all other burns
            if gho_vtoken_address is not None and evt["address"] == gho_vtoken_address:
                continue
            if evt["address"] == scaled_event.event["address"]:
                burn_user = decode_address(evt["topics"][1])
                if burn_user == scaled_event.from_address:
                    burn_amount = int.from_bytes(evt["data"][:32], "big")
                    if burn_amount == scaled_event.amount:
                        return True

    return False


def _match_paired_balance_transfer(
    scaled_event: ScaledTokenEvent,
    operation: Operation | None,
    token_address: ChecksumAddress,
) -> tuple[LogReceipt | None, int | None, int | None]:
    """
    Find a paired BalanceTransfer event for this ERC20 Transfer.

    BalanceTransfer events contain the actual scaled balance being moved,
    while ERC20 Transfer events show aToken amounts (scaled * index / RAY).

    Args:
        scaled_event: The scaled token event being processed
        operation: The operation context (may contain paired BalanceTransfer events)
        token_address: The checksum address of the token contract

    Returns:
        Tuple of (matched_event, scaled_amount, index) or (None, None, None)
    """
    if not operation or not operation.balance_transfer_events:
        return None, None, None

    # For liquidation operations, don't match ERC20 Transfers with BalanceTransfers
    # They represent different movements and should be processed separately
    if operation.operation_type in LIQUIDATION_OPERATION_TYPES:
        return None, None, None

    for bt_event in operation.balance_transfer_events:
        bt_from = decode_address(bt_event["topics"][1])
        bt_to = decode_address(bt_event["topics"][2])
        bt_token = bt_event["address"]

        # Match by token, from, and to addresses (semantic matching)
        # Log index proximity is not reliable in batch transactions
        if (
            bt_token == token_address
            and bt_from == scaled_event.from_address
            and bt_to == scaled_event.target_address
        ):
            decoded_amount, decoded_index = eth_abi.abi.decode(
                types=["uint256", "uint256"],
                data=bt_event["data"],
            )
            return bt_event, int(decoded_amount), int(decoded_index)

    return None, None, None


def _process_collateral_transfer(
    *,
    tx_context: TransactionContext,
    operation: Operation,
    scaled_event: ScaledTokenEvent,
) -> None:
    """
    Process collateral (aToken) transfer between users.

    This function handles the movement of scaled balances when aTokens are transferred
    between users. It accounts for:
    - Paired ERC20 Transfer + BalanceTransfer events
    - Standalone BalanceTransfer events (liquidations)
    - Protocol mints and burns
    """

    assert scaled_event.from_address is not None
    assert scaled_event.target_address is not None

    # Skip events that are handled elsewhere (paired BalanceTransfers, mints, burns)
    if _should_skip_collateral_transfer(scaled_event, operation, tx_context):
        return

    # Get sender and their position
    sender = get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.from_address,
        block_number=scaled_event.event["blockNumber"],
    )

    token_address = scaled_event.event["address"]
    collateral_asset, _ = _get_scaled_token_asset_by_address(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
    )
    assert collateral_asset is not None

    sender_position = get_or_create_collateral_position(
        tx_context=tx_context,
        user=sender,
        asset_id=collateral_asset.id,
    )

    # Determine the scaled amount and index for this transfer
    # For paired events, use BalanceTransfer data (scaled balance)
    # For standalone events, use the event data directly
    _, scaled_amount, transfer_index = _match_paired_balance_transfer(
        scaled_event=scaled_event,
        operation=operation,
        token_address=token_address,
    )

    if scaled_amount is None:
        # No paired BalanceTransfer found - use event data directly
        if scaled_event.index is not None and scaled_event.index > 0:
            # Standalone BalanceTransfer - already in scaled units
            scaled_amount = scaled_event.amount
            transfer_index = scaled_event.index
        else:
            # Standalone ERC20 Transfer - convert to scaled units using liquidity index
            if scaled_event.from_address == ZERO_ADDRESS:
                # Protocol mint (e.g., treasury fee collection) - convert from underlying to scaled
                scaled_amount = scaled_event.amount * 10**27 // collateral_asset.liquidity_index
            else:
                scaled_amount = scaled_event.amount
            transfer_index = collateral_asset.liquidity_index

    # Update sender's scaled balance
    sender_position.balance -= scaled_amount

    # Update sender's last_index only if the new index is higher and valid
    # This prevents older transfer indices from overwriting newer ones
    if transfer_index is not None and transfer_index > 0:
        current_sender_index = sender_position.last_index or 0
        if transfer_index > current_sender_index:
            sender_position.last_index = transfer_index

    # Handle recipient
    if scaled_event.target_address != ZERO_ADDRESS:
        recipient = get_or_create_user(
            tx_context=tx_context,
            user_address=scaled_event.target_address,
            block_number=scaled_event.event["blockNumber"],
        )
        recipient_position = get_or_create_collateral_position(
            tx_context=tx_context,
            user=recipient,
            asset_id=collateral_asset.id,
        )
        recipient_position.balance += scaled_amount

        if transfer_index is not None and transfer_index > 0:
            recipient_position.last_index = transfer_index


def _process_debt_transfer(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
    operation: Operation,
    scaled_event: ScaledTokenEvent,
) -> None:
    """
    Process debt (vToken) transfer between users.
    """

    logger.debug(f"Processing _process_debt_transfer at block {event['blockNumber']}")

    assert scaled_event.from_address is not None
    assert scaled_event.target_address is not None

    # Skip transfers to zero address (burns) - these are handled by Burn events
    # Processing both Transfer(to=0) and Burn would result in double-counting
    if scaled_event.target_address == ZERO_ADDRESS:
        return

    # Skip transfers from zero address (mints) - these are handled by Mint events
    # Processing both Transfer(from=0) and Mint would result in double-counting
    # This occurs during _burnScaled when interest > repayment amount
    if scaled_event.from_address == ZERO_ADDRESS:
        return

    # Get sender
    sender = get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.from_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get debt asset
    token_address = scaled_event.event["address"]
    _, debt_asset = _get_scaled_token_asset_by_address(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
    )

    assert debt_asset

    # Get sender's position
    sender_position = get_or_create_debt_position(
        tx_context=tx_context,
        user=sender,
        asset_id=debt_asset.id,
    )

    # Determine the scaled amount and index for this transfer
    # For paired events, use BalanceTransfer data (scaled balance)
    # For standalone events, use the event data directly
    _, transfer_amount, transfer_index = _match_paired_balance_transfer(
        scaled_event=scaled_event,
        operation=operation,
        token_address=token_address,
    )

    if transfer_amount is None:
        # No paired BalanceTransfer found - use event data directly
        if scaled_event.index is not None and scaled_event.index > 0:
            # Standalone BalanceTransfer - already in scaled units
            transfer_amount = scaled_event.amount
            transfer_index = scaled_event.index
        else:
            # Standalone ERC20 Transfer - use current borrow index
            transfer_amount = scaled_event.amount
            transfer_index = debt_asset.borrow_index

    assert transfer_index is not None

    # Update sender's balance
    sender_position.balance -= transfer_amount

    if transfer_index > 0:
        sender_position.last_index = transfer_index

    # Handle recipient
    if scaled_event.target_address != ZERO_ADDRESS:
        recipient = get_or_create_user(
            tx_context=tx_context,
            user_address=scaled_event.target_address,
            block_number=scaled_event.event["blockNumber"],
        )

        recipient_position = get_or_create_debt_position(
            tx_context=tx_context,
            user=recipient,
            asset_id=debt_asset.id,
        )
        recipient_position.balance += transfer_amount

        if transfer_index > 0:
            recipient_position.last_index = transfer_index


def _extract_user_addresses_from_transaction(events: list[LogReceipt]) -> set[ChecksumAddress]:
    """
    Extract all unique user addresses from a list of transaction events.

    This is used for batch prefetching users to avoid N+1 queries during
    transaction processing.
    """
    return {address for event in events for address in _extract_user_addresses_from_event(event)}


def _extract_user_addresses_from_event(event: LogReceipt) -> set[ChecksumAddress]:
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
        msg = f"Unknown topic: {topic.to_0x_hex()}"
        raise ValueError(msg)

    return user_addresses


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

    # ---
    # TODO: check and refactor this whole block, may not be necessary
    # ---
    # Process discount config events BEFORE fetching stkAAVE events
    # This ensures gho_asset.v_gho_discount_token is set correctly
    if gho_asset is not None:
        for event in discount_config_events:
            topic = event["topics"][0]
            if topic == AaveV3GhoDebtTokenEvent.DISCOUNT_TOKEN_UPDATED.value:
                _process_discount_token_updated_event(
                    event=event,
                    gho_asset=gho_asset,
                )

        # If v_gho_discount_token is still None, try to fetch it from the contract
        # This handles the case where we're processing blocks before any
        # DISCOUNT_TOKEN_UPDATED event or when the database hasn't been initialized
        if gho_asset.v_gho_discount_token is None:
            try:
                discount_token_from_contract = _fetch_discount_token_from_contract(
                    provider=provider,
                    gho_asset=gho_asset,
                    block_number=start_block,
                )
                if discount_token_from_contract:
                    gho_asset.v_gho_discount_token = discount_token_from_contract
                    logger.info(
                        f"Fetched discount token from contract: {discount_token_from_contract}"
                    )
            except Exception as e:  # noqa:BLE001
                logger.debug(f"Could not fetch discount token from contract: {e}")

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
        users_in_transaction = _extract_user_addresses_from_transaction(events=tx_context.events)
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
