import contextlib
import sys
from enum import Enum
from operator import itemgetter
from typing import TYPE_CHECKING, Protocol, cast

import click
import eth_abi.abi
import eth_abi.exceptions
import tqdm
from eth_typing import ChainId, ChecksumAddress
from hexbytes import HexBytes
from sqlalchemy import delete, select
from sqlalchemy.orm import Session, joinedload
from tqdm.contrib.logging import logging_redirect_tqdm
from web3.exceptions import ContractLogicError
from web3.types import LogReceipt, TxParams

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
from degenbot.aave.liquidation_patterns import (
    LiquidationPattern,
    detect_liquidation_patterns,
)
from degenbot.aave.models import EnrichedScaledTokenEvent
from degenbot.aave.position_analysis import (
    UserPositionSummary,
    analyze_positions_for_market,
)
from degenbot.aave.processors import (
    CollateralBurnEvent,
    CollateralMintEvent,
    DebtBurnEvent,
    DebtMintEvent,
    TokenProcessorFactory,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.cli import cli
from degenbot.cli.aave_transaction_operations import (
    Operation,
    OperationType,
    ScaledTokenEvent,
    TransactionOperationsParser,
    TransactionValidationError,
)
from degenbot.cli.aave_types import TokenType, TransactionContext
from degenbot.cli.aave_utils import decode_address
from degenbot.cli.utils import get_web3_from_config
from degenbot.constants import DEAD_ADDRESS, ERC_1967_IMPLEMENTATION_SLOT, MAX_UINT256, ZERO_ADDRESS
from degenbot.database import db_session
from degenbot.database.models.aave import (
    AaveGhoToken,
    AaveV3Asset,
    AaveV3AssetConfig,
    AaveV3CollateralPosition,
    AaveV3Contract,
    AaveV3DebtPosition,
    AaveV3EModeCategory,
    AaveV3Market,
    AaveV3User,
    AaveV3UserCollateralConfig,
)
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.operations import backup_sqlite_database
from degenbot.exceptions import DegenbotValueError
from degenbot.functions import (
    encode_function_calldata,
    fetch_logs_retrying,
    get_number_for_block_identifier,
    raw_call,
)
from degenbot.logging import logger
from degenbot.provider.interface import ProviderAdapter

if TYPE_CHECKING:
    from eth_typing.evm import BlockParams

    from degenbot.aave.processors.base import (
        ScaledTokenBurnResult,
        ScaledTokenMintResult,
    )

# Module-level cache: topic -> category name for Aave events
_AAVE_EVENT_TOPIC_TO_CATEGORY: dict[HexBytes, str] = {
    **{e.value: e.name for e in AaveV3PoolEvent},
    **{e.value: e.name for e in AaveV3StkAaveEvent},
    **{e.value: e.name for e in AaveV3ScaledTokenEvent},
    **{e.value: e.name for e in AaveV3GhoDebtTokenEvent},
    **{e.value: e.name for e in AaveV3PoolConfigEvent},
    **{e.value: e.name for e in AaveV3OracleEvent},
}


class UserOperation(Enum):
    """User operation types for Aave V3 token events."""

    AAVE_REDEEM = "AAVE REDEEM"
    AAVE_STAKED = "AAVE STAKED"
    BORROW = "BORROW"
    DEPOSIT = "DEPOSIT"
    GHO_BORROW = "GHO BORROW"
    GHO_INTEREST_ACCRUAL = "GHO INTEREST ACCRUAL"
    GHO_REPAY = "GHO REPAY"
    REPAY = "REPAY"
    STKAAVE_TRANSFER = "stkAAVE TRANSFER"
    WITHDRAW = "WITHDRAW"


GHO_DISCOUNT_DEPRECATION_REVISION = 4
SCALED_AMOUNT_POOL_REVISION = 9

# Display limit for position risk analysis output
POSITION_RISK_DISPLAY_LIMIT = 20

# Liquidation operation types (used to identify liquidation operations in multiple places)
LIQUIDATION_OPERATION_TYPES = {OperationType.LIQUIDATION, OperationType.GHO_LIQUIDATION}


class WadRayMathLibrary(Protocol):
    def ray_div(self, a: int, b: int) -> int: ...
    def ray_mul(self, a: int, b: int) -> int: ...


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
                            _verify_all_positions(
                                provider=provider,
                                market=market,
                                session=session,
                                block_number=working_end_block,
                                show_progress=show_progress,
                            )
                            _cleanup_zero_balance_positions(
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
                        f"(LT: {collateral_pos.liquidation_threshold / 100:.0f}%){enabled_str}{emode_str}"
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


def _decode_reserve_configuration_bitmap(config_bitmap: int) -> dict[str, int | bool | None]:
    """
    Decode a ReserveConfigurationMap bitmap from the Pool contract.

    Based on Aave V3 ReserveConfiguration library bit positions:
    - bits 0-15: LTV
    - bits 16-31: Liquidation threshold
    - bits 32-47: Liquidation bonus
    - bits 56: Active
    - bits 57: Frozen
    - bits 58: Borrowing enabled
    - bits 60: Paused
    - bits 61: Borrowable in isolation (can this asset be borrowed against isolated collateral)
    - bits 63: Flashloan enabled
    - bits 168-175: eMode category (deprecated in v3.4+, but kept for compatibility)
    - bits 212-251: Debt ceiling (isolation mode debt ceiling)

    Note: isolation_mode is determined by debt_ceiling > 0, not by a flag bit.
    An asset is in isolation mode if it has a debt ceiling set.
    """

    e_mode_category = (config_bitmap >> 168) & 0xFF
    debt_ceiling = (config_bitmap >> 212) & 0xFFFFFFFFFF
    return {
        "ltv": config_bitmap & 0xFFFF,
        "liquidation_threshold": (config_bitmap >> 16) & 0xFFFF,
        "liquidation_bonus": (config_bitmap >> 32) & 0xFFFF,
        "borrowing_enabled": bool((config_bitmap >> 58) & 1),
        "flash_loan_enabled": bool((config_bitmap >> 63) & 1),
        "borrowable_in_isolation": bool((config_bitmap >> 61) & 1),
        "isolation_mode": debt_ceiling > 0,
        "debt_ceiling": debt_ceiling,
        "e_mode_category_id": e_mode_category if e_mode_category > 0 else None,
    }


def _process_collateral_configuration_changed_event(
    *,
    provider: ProviderAdapter,
    session: Session,
    event: LogReceipt,
    market: AaveV3Market,
) -> AaveV3AssetConfig | None:
    """
    Process a CollateralConfigurationChanged event to update asset configuration.

    Fetches full configuration from the Pool contract via getConfiguration()
    and decodes the bitmap to populate all config fields.

    Reference:
    ```
    event CollateralConfigurationChanged(
        address indexed asset,
        uint256 ltv,
        uint256 liquidationThreshold,
        uint256 liquidationBonus
    );
    ```
    """

    asset_address = decode_address(event["topics"][1])

    # Find the asset in the database
    asset = session.scalar(
        select(AaveV3Asset).where(
            AaveV3Asset.underlying_token.has(address=get_checksum_address(asset_address))
        )
    )

    if asset is None:
        logger.warning(
            f"Received CollateralConfigurationChanged for unknown asset: {asset_address}"
        )
        return None

    # Get the Pool contract address
    pool_contract = _get_contract(
        session=session,
        market=market,
        contract_name="POOL",
    )

    if pool_contract is None:
        logger.warning(
            f"Pool contract not found for market {market.id}, cannot fetch full configuration"
        )
        return None

    # Fetch full configuration from Pool contract
    (config_bitmap,) = raw_call(
        w3=provider,
        address=pool_contract.address,
        calldata=encode_function_calldata(
            function_prototype="getConfiguration(address)",
            function_arguments=[asset_address],
        ),
        return_types=["uint256"],
        block_identifier=event["blockNumber"],
    )

    # Decode the configuration bitmap
    decoded = _decode_reserve_configuration_bitmap(config_bitmap)

    # Get or create the asset config
    config = session.scalar(select(AaveV3AssetConfig).where(AaveV3AssetConfig.asset_id == asset.id))

    if config is None:
        config = AaveV3AssetConfig(
            asset_id=asset.id,
            ltv=decoded["ltv"],
            liquidation_threshold=decoded["liquidation_threshold"],
            liquidation_bonus=decoded["liquidation_bonus"],
            borrowing_enabled=decoded["borrowing_enabled"],
            stable_borrowing_enabled=False,
            flash_loan_enabled=decoded["flash_loan_enabled"],
            borrowable_in_isolation=decoded["borrowable_in_isolation"],
            isolation_mode=decoded["isolation_mode"],
            debt_ceiling=decoded["debt_ceiling"],
            e_mode_category_id=decoded["e_mode_category_id"],
        )
        session.add(config)
        logger.info(f"Created AaveV3AssetConfig for {asset_address}")
    else:
        config.ltv = decoded["ltv"]
        config.liquidation_threshold = decoded["liquidation_threshold"]
        config.liquidation_bonus = decoded["liquidation_bonus"]
        config.borrowing_enabled = decoded["borrowing_enabled"]
        config.flash_loan_enabled = decoded["flash_loan_enabled"]
        config.borrowable_in_isolation = decoded["borrowable_in_isolation"]
        config.isolation_mode = decoded["isolation_mode"]
        config.debt_ceiling = decoded["debt_ceiling"]
        config.e_mode_category_id = decoded["e_mode_category_id"]
        logger.info(f"Updated AaveV3AssetConfig for {asset_address}")

    return config


def _process_e_mode_category_added_event(
    *,
    session: Session,
    event: LogReceipt,
    market_id: int,
) -> AaveV3EModeCategory | None:
    """
    Process an EModeCategoryAdded event to update eMode category configuration.

    Reference:
    ```
    event EModeCategoryAdded(
        uint8 indexed categoryId,
        uint256 ltv,
        uint256 liquidationThreshold,
        uint256 liquidationBonus,
        address oracle,
        string label
    );
    ```
    """

    category_id = int.from_bytes(event["topics"][1], "big")

    # Decode non-indexed parameters
    ltv, liquidation_threshold, liquidation_bonus, oracle, label = eth_abi.abi.decode(
        types=["uint256", "uint256", "uint256", "address", "string"],
        data=HexBytes(event["data"]),
    )

    # Check if category already exists
    category = session.scalar(
        select(AaveV3EModeCategory).where(
            AaveV3EModeCategory.market_id == market_id,
            AaveV3EModeCategory.category_id == category_id,
        )
    )

    if category is None:
        category = AaveV3EModeCategory(
            market_id=market_id,
            category_id=category_id,
            label=label,
            ltv=int(ltv),
            liquidation_threshold=int(liquidation_threshold),
            liquidation_bonus=int(liquidation_bonus),
            price_source=get_checksum_address(oracle) if oracle else None,
        )
        session.add(category)
        logger.info(f"Created AaveV3EModeCategory: {label} (ID: {category_id})")
    else:
        category.ltv = int(ltv)
        category.liquidation_threshold = int(liquidation_threshold)
        category.liquidation_bonus = int(liquidation_bonus)
        category.price_source = get_checksum_address(oracle) if oracle else None
        category.label = label
        logger.info(f"Updated AaveV3EModeCategory: {label} (ID: {category_id})")

    return category


def _process_emode_asset_category_changed_event(
    *,
    session: Session,
    event: LogReceipt,
    market_id: int,
) -> AaveV3AssetConfig | None:
    """
    Process an EModeAssetCategoryChanged event (older Aave versions).

    Updates the asset's eMode category assignment.

    Reference:
    ```
    event EModeAssetCategoryChanged(
        address indexed asset,
        uint8 oldCategoryId,
        uint8 newCategoryId
    );
    ```
    """
    asset_address = decode_address(event["topics"][1])

    # Decode non-indexed parameters
    old_category_id, new_category_id = eth_abi.abi.decode(
        types=["uint8", "uint8"],
        data=HexBytes(event["data"]),
    )

    # Find the asset
    asset = session.scalar(
        select(AaveV3Asset).where(
            AaveV3Asset.market_id == market_id,
            AaveV3Asset.underlying_token.has(address=get_checksum_address(asset_address)),
        )
    )
    assert asset is not None

    # Get or create the asset config
    config = session.scalar(select(AaveV3AssetConfig).where(AaveV3AssetConfig.asset_id == asset.id))

    if config is None:
        config = AaveV3AssetConfig(
            asset_id=asset.id,
            ltv=0,
            liquidation_threshold=0,
            liquidation_bonus=0,
            borrowing_enabled=False,
            stable_borrowing_enabled=False,
            flash_loan_enabled=False,
            borrowable_in_isolation=False,
            isolation_mode=False,
            debt_ceiling=None,
            e_mode_category_id=int(new_category_id) if new_category_id > 0 else None,
        )
        session.add(config)
        logger.info(
            f"Created AaveV3AssetConfig for {asset_address} with eMode category {new_category_id}"
        )
    else:
        config.e_mode_category_id = int(new_category_id) if new_category_id > 0 else None
        logger.info(
            f"Updated eMode category for {asset_address}: {old_category_id} -> {new_category_id}"
        )

    return config


def _process_asset_collateral_in_emode_changed_event(
    *,
    session: Session,
    event: LogReceipt,
    market_id: int,
) -> AaveV3AssetConfig | None:
    """
    Process an AssetCollateralInEModeChanged event (newer Aave versions v3.4+).

    This event is emitted when an asset is added or removed as collateral
    in an eMode category. The category_id in the event is the asset's primary
    eMode category.

    Reference:
    ```
    event AssetCollateralInEModeChanged(
        address indexed asset,
        uint8 categoryId,
        bool collateral
    );
    ```
    """
    asset_address = decode_address(event["topics"][1])

    # Decode non-indexed parameters
    category_id, is_collateral = eth_abi.abi.decode(
        types=["uint8", "bool"],
        data=HexBytes(event["data"]),
    )

    # Find the asset
    asset = session.scalar(
        select(AaveV3Asset).where(
            AaveV3Asset.market_id == market_id,
            AaveV3Asset.underlying_token.has(address=get_checksum_address(asset_address)),
        )
    )

    if asset is None:
        logger.warning(f"AssetCollateralInEModeChanged for unknown asset: {asset_address}")
        return None

    # Get or create the asset config
    config = session.scalar(select(AaveV3AssetConfig).where(AaveV3AssetConfig.asset_id == asset.id))

    if config is None:
        # Only set e_mode_category_id if this asset is being added as collateral
        e_mode_cat = int(category_id) if is_collateral and category_id > 0 else None
        config = AaveV3AssetConfig(
            asset_id=asset.id,
            ltv=0,
            liquidation_threshold=0,
            liquidation_bonus=0,
            borrowing_enabled=False,
            stable_borrowing_enabled=False,
            flash_loan_enabled=False,
            borrowable_in_isolation=False,
            isolation_mode=False,
            debt_ceiling=None,
            e_mode_category_id=e_mode_cat,
        )
        session.add(config)
        logger.info(
            f"Created AaveV3AssetConfig for {asset_address} with eMode category {e_mode_cat}"
        )
    # Update category if asset is being added as collateral, clear if removed
    elif is_collateral and category_id > 0:
        config.e_mode_category_id = int(category_id)
        logger.info(f"Set eMode category for {asset_address} to {category_id} (collateral enabled)")
    elif not is_collateral and config.e_mode_category_id == category_id:
        # Only clear if removing from the current category
        config.e_mode_category_id = None
        logger.info(f"Cleared eMode category for {asset_address} (collateral disabled)")

    return config


def _process_reserve_used_as_collateral_enabled_event(
    *,
    session: Session,
    event: LogReceipt,
    market_id: int,
) -> AaveV3UserCollateralConfig | None:
    """
    Process a ReserveUsedAsCollateralEnabled event.

    Reference:
    ```
    event ReserveUsedAsCollateralEnabled(
        address indexed reserve,
        address indexed user
    );
    ```
    """

    asset_address = decode_address(event["topics"][1])
    user_address = decode_address(event["topics"][2])

    # Find the asset
    asset = session.scalar(
        select(AaveV3Asset).where(
            AaveV3Asset.market_id == market_id,
            AaveV3Asset.underlying_token.has(address=get_checksum_address(asset_address)),
        )
    )

    if asset is None:
        logger.warning(f"ReserveUsedAsCollateralEnabled for unknown asset: {asset_address}")
        return None

    # Find or create the user
    user = session.scalar(
        select(AaveV3User).where(
            AaveV3User.market_id == market_id,
            AaveV3User.address == get_checksum_address(user_address),
        )
    )

    if user is None:
        user = AaveV3User(
            market_id=market_id,
            address=get_checksum_address(user_address),
            e_mode=0,
            gho_discount=0,
        )
        session.add(user)
        session.flush([user])
        logger.debug(f"Created AaveV3User: {user_address}")

    # Get or create the collateral config
    config = session.scalar(
        select(AaveV3UserCollateralConfig).where(
            AaveV3UserCollateralConfig.user_id == user.id,
            AaveV3UserCollateralConfig.asset_id == asset.id,
        )
    )

    if config is None:
        config = AaveV3UserCollateralConfig(
            user_id=user.id,
            asset_id=asset.id,
            enabled=True,
        )
        session.add(config)
    else:
        config.enabled = True

    logger.debug(f"Collateral enabled for user {user_address} asset {asset_address}")
    return config


def _process_reserve_used_as_collateral_disabled_event(
    *,
    session: Session,
    event: LogReceipt,
    market_id: int,
) -> AaveV3UserCollateralConfig | None:
    """
    Process a ReserveUsedAsCollateralDisabled event.

    Reference:
    ```
    event ReserveUsedAsCollateralDisabled(
        address indexed reserve,
        address indexed user
    );
    ```
    """

    asset_address = decode_address(event["topics"][1])
    user_address = decode_address(event["topics"][2])

    # Find the asset
    asset = session.scalar(
        select(AaveV3Asset).where(
            AaveV3Asset.market_id == market_id,
            AaveV3Asset.underlying_token.has(address=get_checksum_address(asset_address)),
        )
    )

    if asset is None:
        logger.warning(f"ReserveUsedAsCollateralDisabled for unknown asset: {asset_address}")
        return None

    # Find the user
    user = session.scalar(
        select(AaveV3User).where(
            AaveV3User.market_id == market_id,
            AaveV3User.address == get_checksum_address(user_address),
        )
    )

    if user is None:
        logger.warning(f"ReserveUsedAsCollateralDisabled for unknown user: {user_address}")
        return None

    # Get the collateral config
    config = session.scalar(
        select(AaveV3UserCollateralConfig).where(
            AaveV3UserCollateralConfig.user_id == user.id,
            AaveV3UserCollateralConfig.asset_id == asset.id,
        )
    )

    if config is None:
        # No existing config - create one disabled
        config = AaveV3UserCollateralConfig(
            user_id=user.id,
            asset_id=asset.id,
            enabled=False,
        )
        session.add(config)
    else:
        config.enabled = False

    logger.debug(f"Collateral disabled for user {user_address} asset {asset_address}")
    return config


def _process_asset_initialization_event(
    provider: ProviderAdapter,
    event: LogReceipt,
    market: AaveV3Market,
    session: Session,
) -> None:
    """
    Process a ReserveInitialized event to add a new Aave asset to the database.

    Reference:
    ```
    event ReserveInitialized(
        address indexed asset,
        address indexed aToken,
        address stableDebtToken,
        address variableDebtToken,
        address interestRateStrategyAddress
    );
    ```
    """

    logger.debug(f"Processing asset initialization event at block {event['blockNumber']}")

    asset_address = decode_address(event["topics"][1])
    a_token_address = decode_address(event["topics"][2])

    # Note: stableDebtToken is deprecated in Aave V3 and no longer used, so is ignored
    v_token_address: str
    (_, v_token_address, _) = eth_abi.abi.decode(
        types=["address", "address", "address"], data=event["data"]
    )
    v_token_address = get_checksum_address(v_token_address)

    erc20_token_in_db = _get_or_create_erc20_token(
        provider=provider,
        session=session,
        chain_id=market.chain_id,
        token_address=asset_address,
    )
    a_token = _get_or_create_erc20_token(
        provider=provider,
        session=session,
        chain_id=market.chain_id,
        token_address=a_token_address,
    )
    v_token = _get_or_create_erc20_token(
        provider=provider,
        session=session,
        chain_id=market.chain_id,
        token_address=v_token_address,
    )

    # Per EIP-1967, the implementation address is stored at a known storage slot
    (atoken_implementation_address,) = eth_abi.abi.decode(
        types=["address"],
        data=provider.get_storage_at(
            address=a_token_address,
            position=ERC_1967_IMPLEMENTATION_SLOT,
            block=event["blockNumber"],
        ),
    )
    atoken_implementation_address = get_checksum_address(atoken_implementation_address)

    (vtoken_implementation_address,) = eth_abi.abi.decode(
        types=["address"],
        data=provider.get_storage_at(
            address=v_token_address,
            position=ERC_1967_IMPLEMENTATION_SLOT,
            block=event["blockNumber"],
        ),
    )
    vtoken_implementation_address = get_checksum_address(vtoken_implementation_address)

    (atoken_revision,) = raw_call(
        w3=provider,
        address=atoken_implementation_address,
        calldata=encode_function_calldata(
            function_prototype="ATOKEN_REVISION()",
            function_arguments=None,
        ),
        return_types=["uint256"],
    )
    (vtoken_revision,) = raw_call(
        w3=provider,
        address=vtoken_implementation_address,
        calldata=encode_function_calldata(
            function_prototype="DEBT_TOKEN_REVISION()",
            function_arguments=None,
        ),
        return_types=["uint256"],
    )

    asset = AaveV3Asset(
        market_id=market.id,
        underlying_asset_id=erc20_token_in_db.id,
        a_token_id=a_token.id,
        a_token_revision=atoken_revision,
        v_token_id=v_token.id,
        v_token_revision=vtoken_revision,
        liquidity_index=0,
        liquidity_rate=0,
        borrow_index=0,
        borrow_rate=0,
    )
    session.add(asset)
    session.flush([asset])
    logger.info(f"Added new Aave V3 asset: {asset.underlying_token!r}")

    # Fetch and set the initial price source from the oracle
    oracle_contract = _get_contract(
        session=session,
        market=market,
        contract_name="PRICE_ORACLE",
    )
    assert oracle_contract is not None

    (price_source,) = raw_call(
        w3=provider,
        address=oracle_contract.address,
        calldata=encode_function_calldata(
            function_prototype="getSourceOfAsset(address)",
            function_arguments=[asset_address],
        ),
        return_types=["address"],
        block_identifier=event["blockNumber"],
    )
    if price_source != ZERO_ADDRESS:
        asset.price_source = get_checksum_address(price_source)
        logger.info(f"Set initial price source for {asset_address} to {price_source}")

    # If this is the GHO asset, update the GHO token entry with the vToken reference
    gho_asset = _get_gho_asset(session, market)
    if asset_address == gho_asset.token.address:
        gho_token_entry = session.scalar(
            select(AaveGhoToken).where(AaveGhoToken.token_id == erc20_token_in_db.id)
        )
        if gho_token_entry is not None and gho_token_entry.v_token_id is None:
            gho_token_entry.v_token_id = v_token.id
            logger.info(f"Updated AaveGhoToken v_token_id to {v_token.id} ({v_token_address})")


def _process_user_e_mode_set_event(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
) -> None:
    """
    Process a UserEModeSet event to update a user's E-Mode category.

    Reference:
    ```
    event UserEModeSet(
        address indexed user,
        uint8 categoryId
    );
    ```
    """

    logger.debug(f"Processing user E-mode set event for user at block {event['blockNumber']}")

    user_address = decode_address(event["topics"][1])

    (e_mode,) = eth_abi.abi.decode(types=["uint8"], data=event["data"])

    user = _get_or_create_user(
        tx_context=tx_context,
        user_address=user_address,
        block_number=event["blockNumber"],
    )
    user.e_mode = e_mode


def _process_discount_token_updated_event(
    *,
    event: LogReceipt,
    gho_asset: AaveGhoToken,
) -> None:
    """
    Process a DiscountTokenUpdated event to set the GHO vToken discount token.

    Reference:
    ```
    event DiscountTokenUpdated(
        address indexed oldDiscountToken,
        address indexed newDiscountToken
    );
    ```
    """

    # Ignore the event if it didn't come from the GHO VariableDebtToken contract
    if gho_asset.v_token is None or gho_asset.v_token.address != event["address"]:
        logger.debug(
            "Ignoring DiscountTokenUpdated event, not from canonical GHO VariableDebtToken contract"
        )
        return

    logger.debug(f"Processing discount token updated event at block {event['blockNumber']}")

    old_discount_token_address = decode_address(event["topics"][1])
    new_discount_token_address = decode_address(event["topics"][2])

    gho_asset.v_gho_discount_token = new_discount_token_address

    logger.info(
        f"SET NEW DISCOUNT TOKEN: {old_discount_token_address} -> {new_discount_token_address}"
    )


def _process_discount_rate_strategy_updated_event(
    *,
    event: LogReceipt,
    gho_asset: AaveGhoToken,
) -> None:
    """
    Process a DiscountRateStrategyUpdated event to set the GHO vToken attribute

    Reference:
    ```
    event DiscountRateStrategyUpdated(
        address indexed oldDiscountRateStrategy,
        address indexed newDiscountRateStrategy
    );
    ```
    """

    # Ignore the event if it didn't come from the GHO VariableDebtToken contract
    if gho_asset.v_token is None or gho_asset.v_token.address != event["address"]:
        logger.debug(
            "Ignoring DiscountRateStrategyUpdated event, not from canonical GHO VariableDebtToken "
            "contract"
        )
        return

    logger.debug(f"Processing discount rate strategy updated event at block {event['blockNumber']}")

    old_discount_rate_strategy_address = decode_address(event["topics"][1])
    new_discount_rate_strategy_address = decode_address(event["topics"][2])

    gho_asset.v_gho_discount_rate_strategy = new_discount_rate_strategy_address

    logger.info(
        f"SET NEW DISCOUNT RATE STRATEGY: {old_discount_rate_strategy_address} -> "
        f"{new_discount_rate_strategy_address}"
    )


def _get_or_init_stk_aave_balance(
    *,
    user: AaveV3User,
    tx_context: TransactionContext,
    log_index: int | None = None,
) -> int:
    """
    Get user's last-known stkAAVE balance.

    If the balance is unknown, perform a contract call at the previous block to ensure
    the balance check is performed before any events in the current block are processed.

    When log_index is provided and there are pending stkAAVE transfers for this user
    (transfers with log_index > current log_index), returns the predicted balance
    including the pending delta. This handles the reentrancy case where the GHO
    debt token contract sees the post-transfer balance before the Transfer event is emitted.
    """

    discount_token = tx_context.gho_asset.v_gho_discount_token

    # If discount_token is None (revision 4+), return 0
    if discount_token is None:
        return 0

    if user.stk_aave_balance is None:
        balance: int
        (balance,) = raw_call(
            w3=tx_context.provider,
            address=discount_token,
            calldata=encode_function_calldata(
                function_prototype="balanceOf(address)",
                function_arguments=[user.address],
            ),
            return_types=["uint256"],
            block_identifier=tx_context.block_number - 1,
        )
        user.stk_aave_balance = balance

    assert user.stk_aave_balance is not None

    # Check if we need to account for pending transfers due to reentrancy
    # This happens when stkAAVE is transferred during GHO discount updates,
    # and the GHO contract sees the post-transfer balance before the Transfer event
    if log_index is not None and user.address in tx_context.stk_aave_transfer_users:
        pending_delta = tx_context.get_pending_stk_aave_delta_at_log_index(
            user_address=user.address,
            log_index=log_index,
            discount_token=discount_token,
        )
        return user.stk_aave_balance + pending_delta

    return user.stk_aave_balance


def _process_stk_aave_transfer_event(
    *,
    event: LogReceipt,
    contract_address: ChecksumAddress,
    tx_context: TransactionContext,
) -> None:
    """
    Process a Transfer event on the stkAAVE token.

    This function updates the stkAAVE balance for Aave V3 users only. If either user is not in
    `AaveV3UsersTable` at the time, it will be skipped.

    Reference:
    ```
    event Transfer(
        address indexed from,
        address indexed to,
        uint256 value
    );
    ```
    """

    logger.debug(f"Processing stkAAVE transfer event at block {event['blockNumber']}")

    if tx_context.gho_asset.v_gho_discount_token is None:
        # Ignore stkAAVE transfers until the discount token has been set
        return

    assert contract_address == tx_context.gho_asset.v_gho_discount_token

    from_address = decode_address(event["topics"][1])
    to_address = decode_address(event["topics"][2])

    if from_address == to_address:
        return

    (value,) = eth_abi.abi.decode(types=["uint256"], data=event["data"])

    logger.debug(f"stkAAVE transfer: {from_address} -> {to_address}, value={value}")

    # Get or create users involved in the transfer
    block_number = event["blockNumber"]

    from_user = (
        _get_or_create_user(
            tx_context=tx_context,
            user_address=from_address,
            block_number=block_number,
        )
        if from_address != ZERO_ADDRESS
        else None
    )
    to_user = (
        _get_or_create_user(
            tx_context=tx_context,
            user_address=to_address,
            block_number=block_number,
        )
        if to_address != ZERO_ADDRESS
        else None
    )

    # Ensure balances are known for both users
    # Skip initialization if there's a stkAAVE transfer for this user in this transaction
    # (the transfer event will set the balance correctly)
    if from_user is not None and from_user.stk_aave_balance is None:
        _get_or_init_stk_aave_balance(
            user=from_user,
            tx_context=tx_context,
        )
    if to_user is not None and to_user.stk_aave_balance is None:
        _get_or_init_stk_aave_balance(
            user=to_user,
            tx_context=tx_context,
        )

    # Apply balance changes
    if from_user is not None:
        assert from_user.stk_aave_balance is not None
        assert from_user.stk_aave_balance >= 0, f"{from_user.address} stkAAVE balance < 0!"
        from_user_old_balance = from_user.stk_aave_balance
        from_user.stk_aave_balance -= value

        logger.debug(
            f"stkAAVE balance update: {from_address}, "
            f"before: {from_user_old_balance}, "
            f"after: {from_user.stk_aave_balance}, "
            f"delta: -{value}"
        )
    if to_user is not None:
        assert to_user.stk_aave_balance is not None
        assert to_user.stk_aave_balance >= 0, f"{to_user.address} stkAAVE balance < 0!"
        to_user_old_balance = to_user.stk_aave_balance
        to_user.stk_aave_balance += value

        logger.debug(
            f"stkAAVE balance update: {to_address}"
            f"before: {to_user_old_balance}, "
            f"after: {to_user.stk_aave_balance}, "
            f"delta: +{value}"
        )

    # Mark this transfer as processed to prevent double-counting in pending delta calculations
    tx_context.processed_stk_aave_transfers.add(event["logIndex"])


def _process_reserve_data_update_event(
    *,
    session: Session,
    event: LogReceipt,
    market: AaveV3Market,
) -> None:
    """
    Process a ReserveDataUpdated event to update asset rates and indices.

    Reference:
    ```
    event ReserveDataUpdated(
        address indexed reserve,
        uint256 liquidityRate,
        uint256 stableBorrowRate,
        uint256 variableBorrowRate,
        uint256 liquidityIndex,
        uint256 variableBorrowIndex
    );
    ```
    """

    logger.debug(f"Processing reserve data update event at block {event['blockNumber']}")

    reserve_asset_address = decode_address(event["topics"][1])

    asset_in_db = session.scalar(
        select(AaveV3Asset)
        .join(Erc20TokenTable, AaveV3Asset.underlying_asset_id == Erc20TokenTable.id)
        .where(
            AaveV3Asset.market_id == market.id,
            Erc20TokenTable.address == reserve_asset_address,
        )
    )
    assert asset_in_db is not None

    if asset_in_db.last_update_block is not None:
        assert asset_in_db.last_update_block <= event["blockNumber"]

    liquidity_rate: int

    variable_borrow_rate: int
    liquidity_index: int
    variable_borrow_index: int
    (
        liquidity_rate,
        _,  # stable borrow rate is deprecated on Aave V3
        variable_borrow_rate,
        liquidity_index,
        variable_borrow_index,
    ) = eth_abi.abi.decode(
        types=["uint256", "uint256", "uint256", "uint256", "uint256"],
        data=event["data"],
    )

    asset_in_db.liquidity_rate = liquidity_rate
    asset_in_db.borrow_rate = variable_borrow_rate
    asset_in_db.liquidity_index = liquidity_index
    asset_in_db.borrow_index = variable_borrow_index
    asset_in_db.last_update_block = event["blockNumber"]


def _process_scaled_token_upgrade_event(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
) -> None:
    """
    Process an Upgraded event to update the aToken or vToken revision.

    Reference:
    ```
    event Upgraded(
        address indexed implementation
    );
    ```
    """

    logger.debug(f"Processing scaled token upgrade event at block {event['blockNumber']}")

    new_implementation_address = decode_address(event["topics"][1])

    if (
        aave_collateral_asset := _get_asset_by_token_type(
            session=tx_context.session,
            market=tx_context.market,
            token_address=event["address"],
            token_type=TokenType.A_TOKEN,
        )
    ) is not None:
        (atoken_revision,) = raw_call(
            w3=tx_context.provider,
            address=new_implementation_address,
            calldata=encode_function_calldata(
                function_prototype="ATOKEN_REVISION()",
                function_arguments=None,
            ),
            return_types=["uint256"],
        )
        aave_collateral_asset.a_token_revision = atoken_revision
        logger.info(
            f"Upgraded aToken revision for {aave_collateral_asset.a_token} to {atoken_revision}"
        )
    elif (
        aave_debt_asset := _get_asset_by_token_type(
            session=tx_context.session,
            market=tx_context.market,
            token_address=event["address"],
            token_type=TokenType.V_TOKEN,
        )
    ) is not None:
        (vtoken_revision,) = raw_call(
            w3=tx_context.provider,
            address=new_implementation_address,
            calldata=encode_function_calldata(
                function_prototype="DEBT_TOKEN_REVISION()",
                function_arguments=None,
            ),
            return_types=["uint256"],
        )
        aave_debt_asset.v_token_revision = vtoken_revision
        logger.info(f"Upgraded vToken revision for {aave_debt_asset.v_token} to {vtoken_revision}")

        # Handle GHO discount deprecation on upgrade to revision 4+
        gho_asset = _get_gho_asset(tx_context.session, tx_context.market)
        if (
            gho_asset.v_token is not None
            and aave_debt_asset.v_token.address == gho_asset.v_token.address
            and vtoken_revision >= GHO_DISCOUNT_DEPRECATION_REVISION
        ):
            gho_asset.v_gho_discount_token = None
            gho_asset.v_gho_discount_rate_strategy = None

            # Reset all users' GHO discount to 0 since discount mechanism is deprecated
            for user in (
                tx_context.session
                .execute(
                    select(AaveV3User).where(
                        AaveV3User.market_id == tx_context.market.id,
                        AaveV3User.gho_discount != 0,
                    )
                )
                .scalars()
                .all()
            ):
                user.gho_discount = 0
            logger.info(
                f"GHO discount mechanism deprecated at revision {vtoken_revision}. "
                "Set GHO discounts to 0"
            )

    else:
        token_address = event["address"]
        msg = f"Unknown token type for address {token_address}. Expected aToken or vToken."
        raise ValueError(msg)


def _get_gho_vtoken_revision(
    session: Session,
    market: AaveV3Market,
) -> int | None:
    """
    Get the GHO vToken revision from market assets.
    """

    gho_asset = _get_gho_asset(session, market)
    if gho_asset.v_token is None:
        return None

    return session.scalar(
        select(AaveV3Asset.v_token_revision)
        .join(Erc20TokenTable, AaveV3Asset.v_token_id == Erc20TokenTable.id)
        .where(
            AaveV3Asset.market_id == market.id,
            Erc20TokenTable.address == gho_asset.v_token.address,
        )
    )


def _is_discount_supported(
    session: Session,
    market: AaveV3Market,
) -> bool:
    """
    Check if GHO discount mechanism is supported
    """

    revision = _get_gho_vtoken_revision(session, market)
    return revision is not None and revision < GHO_DISCOUNT_DEPRECATION_REVISION


def _get_or_create_user(
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
        and _is_discount_supported(
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


def _fetch_erc20_token_metadata(
    provider: ProviderAdapter,
    token_address: ChecksumAddress,
) -> tuple[str | None, str | None, int | None]:
    """
    Fetch ERC20 token metadata (name, symbol, decimals) from the blockchain.

    Attempts to fetch using standard ERC20 function signatures, falling back
    to uppercase versions and bytes32 decoding as needed.

    Args:
        provider: ProviderAdapter for blockchain calls
        token_address: The token contract address

    Returns:
        Tuple of (name, symbol, decimals) or (None, None, None) if all fetch attempts fail
    """

    name = _try_fetch_token_string(
        provider=provider,
        token_address=token_address,
        lower_func="name()",
        upper_func="NAME()",
    )
    symbol = _try_fetch_token_string(
        provider=provider,
        token_address=token_address,
        lower_func="symbol()",
        upper_func="SYMBOL()",
    )
    decimals = _try_fetch_token_uint256(
        provider=provider,
        token_address=token_address,
        lower_func="decimals()",
        upper_func="DECIMALS()",
    )

    return name, symbol, decimals


def _try_fetch_token_string(
    provider: ProviderAdapter,
    token_address: ChecksumAddress,
    lower_func: str,
    upper_func: str,
) -> str | None:
    """
    Try to fetch a string value from an ERC20 token, with fallback to bytes32.
    """

    for func_prototype in (lower_func, upper_func):
        with contextlib.suppress(Exception):
            result = provider.call(
                to=token_address,
                data=encode_function_calldata(
                    function_prototype=func_prototype,
                    function_arguments=None,
                ),
            )

            with contextlib.suppress(eth_abi.exceptions.DecodingError):
                (value,) = eth_abi.abi.decode(types=["string"], data=result)
                return str(value)

            # Fallback for older tokens that return bytes32
            (value,) = eth_abi.abi.decode(types=["bytes32"], data=result)
            return (
                value.decode("utf-8", errors="ignore").strip("\x00")
                if isinstance(value, (bytes, HexBytes))
                else str(value)
            )

    return None


def _try_fetch_token_uint256(
    provider: ProviderAdapter,
    token_address: ChecksumAddress,
    lower_func: str,
    upper_func: str,
) -> int | None:
    """
    Try to fetch a uint256 value from an ERC20 token.
    """

    for func_prototype in (lower_func, upper_func):
        with contextlib.suppress(Exception):
            result: int
            (result,) = raw_call(
                w3=provider,
                address=token_address,
                calldata=encode_function_calldata(
                    function_prototype=func_prototype,
                    function_arguments=None,
                ),
                return_types=["uint256"],
            )
            return result

    return None


def _get_or_create_erc20_token(
    provider: ProviderAdapter,
    session: Session,
    chain_id: int,
    token_address: ChecksumAddress,
) -> Erc20TokenTable:
    """
    Get existing ERC20 token or create new one.

    When creating a new token, attempts to fetch name, symbol, and decimals
    from the blockchain and populate the database record.
    """

    if (
        token := session.scalar(
            select(Erc20TokenTable).where(
                Erc20TokenTable.chain == chain_id,
                Erc20TokenTable.address == token_address,
            )
        )
    ) is None:
        token = Erc20TokenTable(chain=chain_id, address=token_address)

        # Attempt to fetch metadata from blockchain
        name, symbol, decimals = _fetch_erc20_token_metadata(
            provider=provider,
            token_address=token_address,
        )

        if name is not None:
            token.name = name
        if symbol is not None:
            token.symbol = symbol
        if decimals is not None:
            token.decimals = decimals

        session.add(token)
        session.flush()

        if name is not None or symbol is not None or decimals is not None:
            logger.debug(
                f"Created ERC20 token {token_address} with metadata: "
                f"name='{name}', symbol='{symbol}', decimals={decimals}"
            )

    return token


def _get_or_create_position[T: AaveV3CollateralPosition | AaveV3DebtPosition](
    *,
    tx_context: TransactionContext,
    user: AaveV3User,
    asset_id: int,
    position_table: type[T],
) -> T:
    """
    Get existing position or create new one with zero balance.
    """

    # Query database - SQLAlchemy's identity map handles caching
    existing_position = tx_context.session.scalar(
        select(position_table).where(
            position_table.user_id == user.id,
            position_table.asset_id == asset_id,
        )
    )

    if existing_position is not None:
        # INVARIANT: Found position must match the user we queried for
        assert existing_position.user_id == user.id, (
            f"Database returned position with wrong user_id: "
            f"expected {user.id}, got {existing_position.user_id}. "
            f"This indicates a SQL error or database corruption."
        )

        return existing_position

    # Create new position
    new_position = position_table(user_id=user.id, asset_id=asset_id, balance=0)
    tx_context.session.add(new_position)
    tx_context.session.flush()

    return cast("T", new_position)


def _get_or_create_collateral_position(
    *,
    tx_context: TransactionContext,
    user: AaveV3User,
    asset_id: int,
) -> AaveV3CollateralPosition:
    """
    Get existing collateral position or create new one with zero balance.

    Uses tx_context.modified_positions cache to avoid repeated database queries.
    """

    return _get_or_create_position(
        tx_context=tx_context,
        user=user,
        asset_id=asset_id,
        position_table=AaveV3CollateralPosition,
    )


def _get_or_create_debt_position(
    *,
    tx_context: TransactionContext,
    user: AaveV3User,
    asset_id: int,
) -> AaveV3DebtPosition:
    """
    Get existing debt position or create new one with zero balance.

    Uses tx_context.modified_positions cache to avoid repeated database queries.
    """

    return _get_or_create_position(
        tx_context=tx_context,
        user=user,
        asset_id=asset_id,
        position_table=AaveV3DebtPosition,
    )


def _get_asset_identifier(asset: AaveV3Asset) -> str:
    """
    Get a human-readable identifier for an asset.

    This provides consistent asset identification in debug logs and error messages.
    """

    return asset.underlying_token.symbol or asset.underlying_token.address


def _get_gho_asset(
    session: Session,
    market: AaveV3Market,
) -> AaveGhoToken:
    """
    Get GHO token asset for a given market.
    """

    gho_asset = session.scalar(
        select(AaveGhoToken)
        .join(AaveGhoToken.token)
        .where(Erc20TokenTable.chain == market.chain_id)
    )
    if gho_asset is None:
        msg = (
            f"GHO token not found for chain {market.chain_id}. "
            "Ensure that market has been activated."
        )
        raise ValueError(msg)

    return gho_asset


def _fetch_discount_token_from_contract(
    provider: ProviderAdapter,
    gho_asset: AaveGhoToken,
    block_number: int,
) -> ChecksumAddress | None:
    """
    Fetch the discount token address from the GHO vToken contract.

    This is used to initialize v_gho_discount_token when it's not set in the database
    and no DISCOUNT_TOKEN_UPDATED events exist in the current block range.
    """

    # vToken not deployed yet
    if gho_asset.v_token is None:
        return None

    try:
        # GHO vToken has a getDiscountToken() function
        discount_token: str
        (discount_token,) = raw_call(
            w3=provider,
            address=gho_asset.v_token.address,
            calldata=encode_function_calldata(
                function_prototype="getDiscountToken()",
                function_arguments=[],
            ),
            return_types=["address"],
            block_identifier=block_number,
        )
        return get_checksum_address(discount_token)
    except (
        ValueError,
        RuntimeError,
        eth_abi.exceptions.DecodingError,
        ContractLogicError,
    ):
        # Function may not exist in older revisions or other errors
        return None


def _get_contract(
    session: Session,
    market: AaveV3Market,
    contract_name: str,
) -> AaveV3Contract | None:
    """
    Get contract by name for a given market.
    """

    return session.scalar(
        select(AaveV3Contract).where(
            AaveV3Contract.market_id == market.id,
            AaveV3Contract.name == contract_name,
        )
    )


def _get_asset_by_token_type(
    session: Session,
    market: AaveV3Market,
    token_address: ChecksumAddress,
    token_type: TokenType,
) -> AaveV3Asset | None:
    """
    Get AaveV3 asset by aToken (collateral) or vToken (debt) address.
    """

    match token_type:
        case TokenType.A_TOKEN:
            return session.scalar(
                select(AaveV3Asset)
                .join(Erc20TokenTable, AaveV3Asset.a_token_id == Erc20TokenTable.id)
                .where(
                    AaveV3Asset.market_id == market.id,
                    Erc20TokenTable.address == token_address,
                )
                .options(joinedload(AaveV3Asset.a_token))
            )
        case TokenType.V_TOKEN:
            return session.scalar(
                select(AaveV3Asset)
                .join(Erc20TokenTable, AaveV3Asset.v_token_id == Erc20TokenTable.id)
                .where(
                    AaveV3Asset.market_id == market.id,
                    Erc20TokenTable.address == token_address,
                )
                .options(joinedload(AaveV3Asset.v_token))
            )
        case _:
            msg = f"Invalid token type: {token_type}"
            raise ValueError(msg)


def _update_debt_position_index(
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
    pool_contract = _get_contract(
        session=tx_context.session,
        market=tx_context.market,
        contract_name="POOL",
    )
    assert pool_contract is not None

    fetched_index = _get_current_borrow_index_from_pool(
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


def _get_current_borrow_index_from_pool(
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

    try:
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
    except (ValueError, RuntimeError, ContractLogicError):
        return None
    else:
        return borrow_index


def _verify_gho_discount_amounts(
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
    revision = _get_gho_vtoken_revision(session=session, market=market)
    logger.debug(f"Verifying GHO discounts: revision={revision}, market.id={market.id}")
    if revision is None or revision >= GHO_DISCOUNT_DEPRECATION_REVISION:
        logger.debug(
            f"Skipping GHO discount verification for GHO VariableDebtToken revision {revision}"
        )
        return

    if gho_asset.v_gho_discount_token is None:
        return

    if user_addresses is not None and len(user_addresses) == 0:
        return

    # Skip verification if GHO vToken is not deployed
    if gho_asset.v_token is None:
        return

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
        try:
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
        except (RuntimeError, eth_abi.exceptions.DecodingError, ContractLogicError):
            # Function may not exist (revision 4+ after upgrade in same block)
            # Verify that our tracked discount is 0 (the new default)
            discount_percent = 0

        assert user.gho_discount == discount_percent, (
            f"User {user.address}: GHO discount {user.gho_discount} "
            f"does not match GHO vDebtToken contract ({discount_percent}) "
            f"@ {gho_vtoken_address} at block {block_number}"
        )


def _verify_stk_aave_balances(
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

    if user_addresses is not None and len(user_addresses) == 0:
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


def _cleanup_zero_balance_positions(
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


def _verify_positions_for_users(
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

    _verify_scaled_token_positions(
        provider=provider,
        market=market,
        session=session,
        position_table=AaveV3CollateralPosition,
        block_number=block_number,
        show_progress=show_progress,
        user_addresses=user_addresses,
    )
    _verify_scaled_token_positions(
        provider=provider,
        market=market,
        session=session,
        position_table=AaveV3DebtPosition,
        block_number=block_number,
        show_progress=show_progress,
        user_addresses=user_addresses,
    )
    _verify_stk_aave_balances(
        provider=provider,
        session=session,
        market=market,
        gho_asset=gho_asset,
        block_number=block_number,
        show_progress=show_progress,
        user_addresses=user_addresses,
    )
    _verify_gho_discount_amounts(
        provider=provider,
        session=session,
        market=market,
        gho_asset=gho_asset,
        block_number=block_number,
        show_progress=show_progress,
        user_addresses=user_addresses,
    )


def _verify_all_positions(
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
        no_progress: If True, disable progress bars
    """

    logger.info(f"Performing full verification of all positions at block {block_number:,}")

    gho_asset = _get_gho_asset(session=session, market=market)

    _verify_positions_for_users(
        provider=provider,
        market=market,
        session=session,
        gho_asset=gho_asset,
        block_number=block_number,
        show_progress=show_progress,
        user_addresses=None,
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


def _verify_scaled_token_positions(
    *,
    provider: ProviderAdapter,
    market: AaveV3Market,
    session: Session,
    position_table: type[AaveV3CollateralPosition | AaveV3DebtPosition],
    block_number: int,
    show_progress: bool,
    user_addresses: set[ChecksumAddress] | None = None,
) -> None:
    """
    Verify that database position balances match the contract.

    If user_addresses is provided, only verifies positions for those specific users.
    Otherwise, verifies all users in the market.
    """

    if user_addresses is not None and len(user_addresses) == 0:
        return

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
            msg = f"Unknown position table type: {position_table}"
            raise ValueError(msg)

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


def _process_transaction(tx_context: TransactionContext) -> None:
    """
    Process transaction using operation-based parsing.
    """

    # Cache GHO vToken address for reuse
    gho_vtoken_address = tx_context.gho_vtoken_address

    # First, build a map of discount updates in this transaction to get old values
    # Track all updates with their log indices to handle multiple updates per transaction
    for event in tx_context.events:
        topic = event["topics"][0]
        if topic == AaveV3GhoDebtTokenEvent.DISCOUNT_PERCENT_UPDATED.value:
            user_address = decode_address(event["topics"][1])
            (old_discount_percent,) = eth_abi.abi.decode(types=["uint256"], data=event["data"])
            if user_address not in tx_context.discount_updates_by_log_index:
                tx_context.discount_updates_by_log_index[user_address] = []
            tx_context.discount_updates_by_log_index[user_address].append((
                event["logIndex"],
                old_discount_percent,
            ))

    # Sort updates by log index for each user
    for user_address in tx_context.discount_updates_by_log_index:
        tx_context.discount_updates_by_log_index[user_address].sort(key=itemgetter(0))

    # Pre-fetch all users from GHO mint/burn events to avoid N+1 queries
    gho_user_addresses: set[ChecksumAddress] = set()
    for event in tx_context.events:
        topic = event["topics"][0]
        event_address = event["address"]

        if (
            topic
            in {
                AaveV3ScaledTokenEvent.MINT.value,
                AaveV3ScaledTokenEvent.BURN.value,
            }
            and gho_vtoken_address is not None
            and event_address == gho_vtoken_address
        ):
            # Mint event: topics[1] = caller, topics[2] = onBehalfOf (user)
            # Burn event: topics[1] = from (user), topics[2] = target
            if topic == AaveV3ScaledTokenEvent.MINT.value:
                user_address = decode_address(event["topics"][2])
            else:  # SCALED_TOKEN_BURN
                user_address = decode_address(event["topics"][1])
            gho_user_addresses.add(user_address)

    # Fetch all GHO users in a single query
    gho_users = (
        {
            user.address: user
            for user in tx_context.session.scalars(
                select(AaveV3User).where(
                    AaveV3User.address.in_(gho_user_addresses),
                    AaveV3User.market_id == tx_context.market.id,
                )
            )
        }
        if gho_user_addresses
        else {}
    )

    # Capture user discount percents before processing events
    # This ensures calculations use the discount in effect at the start of the transaction
    for event in tx_context.events:
        topic = event["topics"][0]
        event_address = event["address"]

        # Capture GHO user discount percents for mint/burn events
        if (
            topic
            in {
                AaveV3ScaledTokenEvent.MINT.value,
                AaveV3ScaledTokenEvent.BURN.value,
            }
            and gho_vtoken_address is not None
            and event_address == gho_vtoken_address
        ):
            # Mint event: topics[1] = caller, topics[2] = onBehalfOf (user)
            # Burn event: topics[1] = from (user), topics[2] = target
            if topic == AaveV3ScaledTokenEvent.MINT.value:
                user_address = decode_address(event["topics"][2])
            else:  # SCALED_TOKEN_BURN
                user_address = decode_address(event["topics"][1])
            if user_address not in tx_context.user_discounts:
                # If there are DiscountPercentUpdated events for this user in this
                # transaction, use the OLD discount value that was in effect at the
                # start of the transaction (before any updates in this tx)
                user = gho_users.get(user_address)
                if user is not None:
                    # Get discount that was in effect at this specific log index
                    tx_context.user_discounts[user_address] = (
                        tx_context.get_effective_discount_at_log_index(
                            user_address=user_address,
                            log_index=event["logIndex"],
                            default_discount=user.gho_discount,
                        )
                    )
                    continue

                # User doesn't exist in database yet - fetch discount from contract
                # This happens when a user with an existing GHO debt position
                # is first encountered during event processing
                if gho_vtoken_address is None or not _is_discount_supported(
                    session=tx_context.session,
                    market=tx_context.market,
                ):
                    # Discount mechanism not available (no vToken or revision 4+)
                    tx_context.user_discounts[user_address] = 0
                    continue

                try:
                    (discount_percent,) = raw_call(
                        w3=tx_context.provider,
                        address=gho_vtoken_address,
                        calldata=encode_function_calldata(
                            function_prototype="getDiscountPercent(address)",
                            function_arguments=[user_address],
                        ),
                        return_types=["uint256"],
                        block_identifier=tx_context.block_number,
                    )
                    tx_context.user_discounts[user_address] = discount_percent
                except (
                    RuntimeError,
                    eth_abi.exceptions.DecodingError,
                    ContractLogicError,
                ):
                    # Function may not exist (revision 4+), default to 0
                    tx_context.user_discounts[user_address] = 0

    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing transaction at block "
        f"{tx_context.block_number}"
    )

    # Parse events into operations
    pool_contract = _get_contract(
        session=tx_context.session,
        market=tx_context.market,
        contract_name="POOL",
    )
    assert pool_contract is not None

    parser = TransactionOperationsParser(
        market=tx_context.market,
        session=tx_context.session,
        pool_address=get_checksum_address(pool_contract.address),
    )
    tx_operations = parser.parse(
        events=tx_context.events,
        tx_hash=tx_context.tx_hash,
    )

    # Strict validation - fail immediately on any issue
    try:
        tx_operations.validate(tx_context.events)
    except TransactionValidationError as e:
        logger.error(f"Transaction validation failed: {e}")
        raise

    logger.debug(f"\n=== OPERATIONS FOR TX {tx_context.tx_hash.to_0x_hex()} ===")
    for op in tx_operations.operations:
        logger.debug(f"Operation {op.operation_id}: {op.operation_type.name}")
        if op.pool_event:
            logger.debug(f"  Pool event: logIndex={op.pool_event['logIndex']}")
        for scaled_ev in op.scaled_token_events:
            logger.debug(
                f"  Scaled event: logIndex={scaled_ev.event['logIndex']}, "
                f"type={scaled_ev.event_type}, user={scaled_ev.user_address}, "
                f"amount={scaled_ev.amount}, index={scaled_ev.index}"
            )
        for transfer_ev in op.transfer_events:
            logger.debug(f"  Transfer event: logIndex={transfer_ev['logIndex']}")
        for balance_transfer_ev in op.balance_transfer_events:
            logger.debug(f"  BalanceTransfer event: logIndex={balance_transfer_ev['logIndex']}")
    logger.debug("=== END OPERATIONS ===\n")

    # Collect all scaled token events from operations for pattern detection
    tx_context.scaled_token_events = [
        scaled_ev for op in tx_operations.operations for scaled_ev in op.scaled_token_events
    ]

    # Preprocess liquidations to detect patterns for multi-liquidation scenarios
    # This handles both combined burn (Issue 0056) and separate burns (Issue 0065) patterns.
    _preprocess_liquidation_aggregates(tx_context, tx_operations.operations)

    # Process stkAAVE transfers BEFORE operations to ensure stkAAVE balances
    # are up-to-date when GHO debt operations calculate discount rates.
    # This handles cases where stkAAVE transfers (e.g., rewards claims) occur
    # before GHO mint/burn events in the same transaction.
    if tx_context.gho_asset and tx_context.gho_asset.v_gho_discount_token:
        discount_token = tx_context.gho_asset.v_gho_discount_token
        for event in tx_context.events:
            topic = event["topics"][0]
            event_address = event["address"]
            if topic == ERC20Event.TRANSFER.value and event_address == discount_token:
                _process_stk_aave_transfer_event(
                    event=event,
                    contract_address=event_address,
                    tx_context=tx_context,
                )

    # Pre-process WITHDRAW operations to extract withdrawAmounts before processing scaled events.
    # This ensures INTEREST_ACCRUAL collateral burns can use the original withdrawAmount to avoid
    # 1 wei rounding errors from reverse-calculating from Burn event values.
    # Note: REPAY paybackAmounts are now extracted during operation processing via extraction_data
    for operation in tx_operations.operations:
        if (
            operation.operation_type == OperationType.WITHDRAW
            and operation.pool_event is not None
            and operation.pool_event.get("data")
        ):
            decoded = eth_abi.abi.decode(["uint256"], operation.pool_event["data"])
            tx_context.last_withdraw_amount = decoded[0]
            # Store the token and user addresses for matching with INTEREST_ACCRUAL burns
            if operation.scaled_token_events:
                first_event = operation.scaled_token_events[0]
                tx_context.last_withdraw_token_address = first_event.event["address"]
                tx_context.last_withdraw_user_address = first_event.user_address
            logger.debug(
                f"Pre-processed WITHDRAW amount: {tx_context.last_withdraw_amount} "
                f"for operation {operation.operation_id}"
            )

    # Process each operation in chronological order (sorted by pool event or minimum scaled event
    # log index)
    # This ensures events are processed in the order they appear in the transaction.
    # Operations with pool events are sorted by pool event log index.
    # Operations without pool events (INTEREST_ACCRUAL, etc.) are sorted by minimum scaled event
    # log index.
    def _get_operation_sort_key(op: Operation) -> int:
        if op.pool_event is not None:
            # Use pool event log index for operations with pool events
            return op.pool_event["logIndex"]
        if op.scaled_token_events:
            # Use minimum scaled event log index for operations without pool events
            return min(ev.event["logIndex"] for ev in op.scaled_token_events)
        # Fallback for operations with no events (place at the end)
        return MAX_UINT256

    sorted_operations = sorted(tx_operations.operations, key=_get_operation_sort_key)
    for operation in sorted_operations:
        logger.debug(
            f"Processing operation {operation.operation_id}: {operation.operation_type.name}"
        )
        _process_operation(
            operation=operation,
            tx_context=tx_context,
        )

    # Build a map of assigned log indices and liquidation operations for deferred processing
    # This handles cases where debt burn events are emitted BEFORE LiquidationCall events
    # See debug/aave/0060 for details on out-of-order event emission
    assigned_log_indices: set[int] = set()
    liquidation_operations: list[Operation] = []
    for op in tx_operations.operations:
        assigned_log_indices.update(
            scaled_ev.event["logIndex"] for scaled_ev in op.scaled_token_events
        )
        assigned_log_indices.update(transfer_ev["logIndex"] for transfer_ev in op.transfer_events)
        assigned_log_indices.update(
            balance_transfer_ev["logIndex"] for balance_transfer_ev in op.balance_transfer_events
        )
        if op.pool_event:
            assigned_log_indices.add(op.pool_event["logIndex"])
        # Track liquidation operations for deferred burn matching
        if op.operation_type in LIQUIDATION_OPERATION_TYPES:
            liquidation_operations.append(op)

    # Process deferred debt burns that couldn't be matched during initial parsing
    # This handles the case where Burn events are emitted before LiquidationCall events
    _process_deferred_debt_burns(
        tx_context=tx_context,
        liquidation_operations=liquidation_operations,
        assigned_log_indices=assigned_log_indices,
    )

    for event in tx_context.events:
        if event["logIndex"] in assigned_log_indices:
            continue

        topic = event["topics"][0]
        event_address = event["address"]

        # Dispatch to appropriate handler for non-operation events
        if topic == AaveV3PoolEvent.RESERVE_DATA_UPDATED.value:
            _process_reserve_data_update_event(
                session=tx_context.session,
                event=event,
                market=tx_context.market,
            )
        elif topic == AaveV3PoolEvent.USER_E_MODE_SET.value:
            _process_user_e_mode_set_event(
                event=event,
                tx_context=tx_context,
            )
        elif topic == AaveV3PoolConfigEvent.POOL_UPDATED.value:
            _update_contract_revision(
                session=tx_context.session,
                provider=tx_context.provider,
                market=tx_context.market,
                contract_name="POOL",
                new_address=decode_address(event["topics"][2]),
                revision_function_prototype="POOL_REVISION",
            )
            pool_contract = _get_contract(
                session=tx_context.session,
                market=tx_context.market,
                contract_name="POOL",
            )
            assert pool_contract is not None
            assert pool_contract.revision is not None
            tx_context.pool_revision = pool_contract.revision
        elif topic == AaveV3PoolConfigEvent.POOL_CONFIGURATOR_UPDATED.value:
            _update_contract_revision(
                session=tx_context.session,
                provider=tx_context.provider,
                market=tx_context.market,
                contract_name="POOL_CONFIGURATOR",
                new_address=decode_address(event["topics"][2]),
                revision_function_prototype="CONFIGURATOR_REVISION",
            )
        elif topic == AaveV3PoolConfigEvent.POOL_DATA_PROVIDER_UPDATED.value:
            _process_pool_data_provider_updated_event(
                session=tx_context.session,
                market=tx_context.market,
                event=event,
            )
        elif topic == AaveV3PoolConfigEvent.ADDRESS_SET.value:
            _process_address_set_event(
                session=tx_context.session,
                market=tx_context.market,
                event=event,
            )
        elif topic == AaveV3PoolConfigEvent.PRICE_ORACLE_UPDATED.value:
            _process_price_oracle_updated_event(
                session=tx_context.session,
                market=tx_context.market,
                event=event,
            )
        elif topic == AaveV3OracleEvent.ASSET_SOURCE_UPDATED.value:
            _process_asset_source_updated_event(
                session=tx_context.session,
                market=tx_context.market,
                event=event,
            )
        elif topic == AaveV3PoolConfigEvent.UPGRADED.value:
            _process_scaled_token_upgrade_event(
                event=event,
                tx_context=tx_context,
            )
        elif topic == AaveV3GhoDebtTokenEvent.DISCOUNT_PERCENT_UPDATED.value:
            _process_discount_percent_updated_event(
                event=event,
                tx_context=tx_context,
            )
        elif topic == AaveV3GhoDebtTokenEvent.DISCOUNT_TOKEN_UPDATED.value:
            _process_discount_token_updated_event(
                event=event,
                gho_asset=tx_context.gho_asset,
            )
        elif topic == AaveV3GhoDebtTokenEvent.DISCOUNT_RATE_STRATEGY_UPDATED.value:
            _process_discount_rate_strategy_updated_event(
                event=event,
                gho_asset=tx_context.gho_asset,
            )
        elif topic == AaveV3PoolEvent.RESERVE_USED_AS_COLLATERAL_ENABLED.value:
            _process_reserve_used_as_collateral_enabled_event(
                session=tx_context.session,
                event=event,
                market_id=tx_context.market.id,
            )
        elif topic == AaveV3PoolEvent.RESERVE_USED_AS_COLLATERAL_DISABLED.value:
            _process_reserve_used_as_collateral_disabled_event(
                session=tx_context.session,
                event=event,
                market_id=tx_context.market.id,
            )


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
    user = _get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get collateral position
    collateral_position = _get_or_create_collateral_position(
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

    asset_identifier = _get_asset_identifier(collateral_asset)
    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing {asset_identifier} collateral mint "
        f"at block {event['blockNumber']}"
    )

    user = _get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get or create collateral position
    collateral_position = _get_or_create_collateral_position(
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

    asset_identifier = _get_asset_identifier(collateral_asset)
    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing {asset_identifier} collateral burn "
        f"at block {event['blockNumber']}"
    )

    # Get user
    user = _get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get collateral position
    collateral_position = _get_or_create_collateral_position(
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

    asset_identifier = _get_asset_identifier(debt_asset)
    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing {asset_identifier} debt mint "
        f"at block {event['blockNumber']}"
    )

    user = _get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get or create debt position
    debt_position = _get_or_create_debt_position(
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
        _update_debt_position_index(
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
            discount_token_balance = _get_or_init_stk_aave_balance(
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

        _update_debt_position_index(
            tx_context=tx_context,
            debt_asset=debt_asset,
            debt_position=debt_position,
            event_index=scaled_event.index,
            event_block_number=scaled_event.event["blockNumber"],
        )


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

    asset_identifier = _get_asset_identifier(debt_asset)
    logger.debug(
        f"[Pool rev {tx_context.pool_revision}] Processing {asset_identifier} debt burn "
        f"at block {event['blockNumber']}"
    )

    user = _get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get debt position
    debt_position = _get_or_create_debt_position(
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
        _update_debt_position_index(
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
            discount_token_balance = _get_or_init_stk_aave_balance(
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

        _update_debt_position_index(
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
    sender = _get_or_create_user(
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

    sender_position = _get_or_create_collateral_position(
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
        recipient = _get_or_create_user(
            tx_context=tx_context,
            user_address=scaled_event.target_address,
            block_number=scaled_event.event["blockNumber"],
        )
        recipient_position = _get_or_create_collateral_position(
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
    sender = _get_or_create_user(
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
    sender_position = _get_or_create_debt_position(
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
        recipient = _get_or_create_user(
            tx_context=tx_context,
            user_address=scaled_event.target_address,
            block_number=scaled_event.event["blockNumber"],
        )

        recipient_position = _get_or_create_debt_position(
            tx_context=tx_context,
            user=recipient,
            asset_id=debt_asset.id,
        )
        recipient_position.balance += transfer_amount

        if transfer_index > 0:
            recipient_position.last_index = transfer_index


def _get_scaled_token_asset_by_address(
    session: Session,
    market: AaveV3Market,
    token_address: ChecksumAddress,
) -> tuple[AaveV3Asset | None, AaveV3Asset | None]:
    """
    Get collateral and debt assets by token address.
    """

    collateral_asset = _get_asset_by_token_type(
        session=session,
        market=market,
        token_address=token_address,
        token_type=TokenType.A_TOKEN,
    )

    debt_asset = _get_asset_by_token_type(
        session=session,
        market=market,
        token_address=token_address,
        token_type=TokenType.V_TOKEN,
    )

    if collateral_asset is not None and debt_asset is not None:
        assert collateral_asset.id != debt_asset.id

    return collateral_asset, debt_asset


def _get_all_scaled_token_addresses(
    session: Session,
    chain_id: int,
) -> list[ChecksumAddress]:
    """
    Get all aToken and vToken addresses for a given chain.
    """

    a_token_addresses = list(
        session.scalars(
            select(Erc20TokenTable.address)
            .join(
                AaveV3Asset,
                AaveV3Asset.a_token_id == Erc20TokenTable.id,
            )
            .where(Erc20TokenTable.chain == chain_id)
        ).all()
    )

    v_token_addresses = list(
        session.scalars(
            select(Erc20TokenTable.address)
            .join(
                AaveV3Asset,
                AaveV3Asset.v_token_id == Erc20TokenTable.id,
            )
            .where(Erc20TokenTable.chain == chain_id)
        ).all()
    )

    return a_token_addresses + v_token_addresses


def _update_contract_revision(
    *,
    session: Session,
    provider: ProviderAdapter,
    market: AaveV3Market,
    contract_name: str,
    new_address: ChecksumAddress,
    revision_function_prototype: str,
) -> None:
    """
    Update contract revision in database.
    """

    revision: int
    (revision,) = raw_call(
        w3=provider,
        address=new_address,
        calldata=encode_function_calldata(
            function_prototype=f"{revision_function_prototype}()",
            function_arguments=None,
        ),
        return_types=["uint256"],
    )

    contract = _get_contract(
        session=session,
        market=market,
        contract_name=contract_name,
    )
    assert contract is not None

    contract.revision = revision

    logger.info(f"Upgraded revision for {contract.name} to {revision}")


def _process_proxy_creation_event(
    *,
    provider: ProviderAdapter,
    session: Session,
    market: AaveV3Market,
    event: LogReceipt,
    proxy_name: str,
    proxy_id: bytes,
    revision_function_prototype: str,
) -> None:
    """
    Process a proxy creation event (POOL or POOL_CONFIGURATOR).
    """

    logger.debug(f"Processing _process_proxy_creation_event at block {event['blockNumber']}")

    (decoded_proxy_id,) = eth_abi.abi.decode(types=["bytes32"], data=event["topics"][1])

    if decoded_proxy_id != proxy_id:
        return

    proxy_address = decode_address(event["topics"][2])
    implementation_address = decode_address(event["topics"][3])

    if (
        session.scalar(select(AaveV3Contract).where(AaveV3Contract.address == proxy_address))
        is not None
    ):
        return

    (revision,) = raw_call(
        w3=provider,
        address=implementation_address,
        calldata=encode_function_calldata(
            function_prototype=f"{revision_function_prototype}()",
            function_arguments=None,
        ),
        return_types=["uint256"],
    )

    market.contracts.append(
        AaveV3Contract(
            market_id=market.id,
            name=proxy_name,
            address=proxy_address,
            revision=revision,
        )
    )


def _process_pool_data_provider_updated_event(
    *,
    session: Session,
    market: AaveV3Market,
    event: LogReceipt,
) -> None:
    """
    Process a PoolDataProviderUpdated event chronologically.

    Event structure:
    - topics[1]: oldAddress (address indexed)
    - topics[2]: newAddress (address indexed)
    """
    old_pool_data_provider_address = decode_address(event["topics"][1])
    new_pool_data_provider_address = decode_address(event["topics"][2])

    if old_pool_data_provider_address == ZERO_ADDRESS:
        session.add(
            AaveV3Contract(
                market_id=market.id,
                name="POOL_DATA_PROVIDER",
                address=new_pool_data_provider_address,
            )
        )
    else:
        pool_data_provider = session.scalar(
            select(AaveV3Contract).where(AaveV3Contract.address == old_pool_data_provider_address)
        )
        assert pool_data_provider is not None
        pool_data_provider.address = new_pool_data_provider_address


def _process_address_set_event(
    *,
    session: Session,
    market: AaveV3Market,
    event: LogReceipt,
) -> None:
    """
    Process an AddressSet event chronologically.

    Event structure:
    - topics[1]: id (bytes32 indexed) - contract identifier
    - topics[2]: oldAddress (address indexed)
    - topics[3]: newAddress (address indexed)
    """

    # Decode the contract id from bytes32
    contract_id_bytes: bytes
    (contract_id_bytes,) = eth_abi.abi.decode(types=["bytes32"], data=event["topics"][1])
    contract_name = contract_id_bytes.decode("ascii").strip("\x00")

    old_address = decode_address(event["topics"][2])
    new_address = decode_address(event["topics"][3])

    if old_address == ZERO_ADDRESS:
        # New contract registration
        session.add(
            AaveV3Contract(
                market_id=market.id,
                name=contract_name,
                address=new_address,
            )
        )
        logger.info(f"Registered contract {contract_name}: @ {new_address}")
    else:
        # Contract address update
        contract = session.scalar(
            select(AaveV3Contract).where(AaveV3Contract.address == old_address)
        )
        assert contract is not None
        contract.address = new_address
        logger.info(f"Updated contract {contract_name}: {old_address} -> {new_address}")


def _process_price_oracle_updated_event(
    *,
    session: Session,
    market: AaveV3Market,
    event: LogReceipt,
) -> None:
    """
    Process a PriceOracleUpdated event from the PoolAddressesProvider.

    Event structure:
    - topics[1]: oldAddress (address indexed)
    - topics[2]: newAddress (address indexed)

    This event is emitted when the price oracle address is updated in the
    PoolAddressesProvider. We use it to track the canonical oracle address.

    Event definition:
        event PriceOracleUpdated(
            address indexed oldAddress,
            address indexed newAddress
        );
    """

    old_address = decode_address(event["topics"][1])
    new_address = decode_address(event["topics"][2])

    logger.info(
        f"PriceOracleUpdated: oldAddress={old_address}, newAddress={new_address} "
        f"(block {event['blockNumber']})"
    )

    # Register or update the PRICE_ORACLE in the database
    existing_oracle = session.scalar(
        select(AaveV3Contract).where(
            AaveV3Contract.market_id == market.id,
            AaveV3Contract.name == "PRICE_ORACLE",
        )
    )

    if existing_oracle is None:
        session.add(
            AaveV3Contract(
                market_id=market.id,
                name="PRICE_ORACLE",
                address=new_address,
            )
        )
        logger.info(f"Registered PRICE_ORACLE at {new_address} from PriceOracleUpdated event")
    elif existing_oracle.address != new_address:
        # Update to the new oracle address
        existing_oracle.address = new_address
        logger.info(f"Updated PRICE_ORACLE: {existing_oracle.address} -> {new_address}")


def _process_asset_source_updated_event(
    *,
    session: Session,
    market: AaveV3Market,
    event: LogReceipt,
) -> None:
    """
    Process an AssetSourceUpdated event from the AaveOracle.

    Event structure:
    - topics[1]: asset (address indexed)
    - topics[2]: source (address indexed)

    Updates the asset's price_source field in the database.

    Event definition:
        event AssetSourceUpdated(
            address indexed asset,
            address indexed source
        );
    """

    asset_address = decode_address(event["topics"][1])
    source_address = decode_address(event["topics"][2])

    # Find the asset by underlying token address
    asset = session.scalar(
        select(AaveV3Asset).where(
            AaveV3Asset.market_id == market.id,
            AaveV3Asset.underlying_token.has(address=get_checksum_address(asset_address)),
        )
    )

    if asset is None:
        logger.warning(
            f"AssetSourceUpdated for unknown asset: {asset_address}, "
            f"source={source_address} (block {event['blockNumber']})"
        )
        return

    asset.price_source = get_checksum_address(source_address)
    logger.info(
        f"AssetSourceUpdated: asset={asset_address}, source={source_address} "
        f"(block {event['blockNumber']})"
    )


def _process_discount_percent_updated_event(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
) -> None:
    """
    Process a GHO discount percent update event.

    With transaction-level processing, this is called BEFORE Mint/Burn
    events in the same transaction, so the discount rate is already updated.

    Reference:
    ```
    event DiscountPercentUpdated(
        address indexed user,
        uint256 oldDiscountPercent,
        uint256 indexed newDiscountPercent
    );
    ```
    """

    logger.debug(
        f"Processing _process_discount_percent_updated_event at block {event['blockNumber']}"
    )

    user_address = decode_address(event["topics"][1])

    (_old_discount_percent,) = eth_abi.abi.decode(types=["uint256"], data=event["data"])
    (new_discount_percent,) = eth_abi.abi.decode(types=["uint256"], data=event["topics"][2])

    user = _get_or_create_user(
        tx_context=tx_context,
        user_address=user_address,
        block_number=event["blockNumber"],
    )

    # With transaction-level processing, the discount is updated here
    # and subsequent Mint/Burn events in the same transaction will see
    # the updated value
    logger.debug(
        f"DiscountPercentUpdated for {user_address}: "
        f"{user.gho_discount} -> {new_discount_percent} "
        f"(user.id={user.id})"
    )
    user.gho_discount = new_discount_percent


def _fetch_pool_events(
    provider: ProviderAdapter,
    pool_address: ChecksumAddress,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch Pool contract events for assertions and config updates.
    """

    return fetch_logs_retrying(
        w3=provider,
        start_block=start_block,
        end_block=end_block,
        address=[pool_address],
        topic_signature=[
            [
                AaveV3PoolEvent.BORROW.value,
                AaveV3PoolEvent.DEFICIT_CREATED.value,
                AaveV3PoolEvent.LIQUIDATION_CALL.value,
                AaveV3PoolEvent.MINTED_TO_TREASURY.value,
                AaveV3PoolEvent.REPAY.value,
                AaveV3PoolEvent.RESERVE_DATA_UPDATED.value,
                AaveV3PoolEvent.RESERVE_USED_AS_COLLATERAL_DISABLED.value,
                AaveV3PoolEvent.RESERVE_USED_AS_COLLATERAL_ENABLED.value,
                AaveV3PoolEvent.SUPPLY.value,
                AaveV3PoolEvent.USER_E_MODE_SET.value,
                AaveV3PoolEvent.WITHDRAW.value,
            ]
        ],
    )


def _fetch_reserve_initialization_events(
    provider: ProviderAdapter,
    configurator_address: ChecksumAddress,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch Pool Configurator events for reserve initialization and configuration changes.
    """

    return fetch_logs_retrying(
        w3=provider,
        start_block=start_block,
        end_block=end_block,
        address=[configurator_address],
        topic_signature=[
            [
                AaveV3PoolConfigEvent.ASSET_COLLATERAL_IN_EMODE_CHANGED.value,
                AaveV3PoolConfigEvent.COLLATERAL_CONFIGURATION_CHANGED.value,
                AaveV3PoolConfigEvent.EMODE_ASSET_CATEGORY_CHANGED.value,
                AaveV3PoolConfigEvent.EMODE_CATEGORY_ADDED.value,
                AaveV3PoolConfigEvent.RESERVE_INITIALIZED.value,
            ]
        ],
    )


def _fetch_scaled_token_events(
    provider: ProviderAdapter,
    token_addresses: list[ChecksumAddress],
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch events from all scaled tokens (aTokens, vTokens).
    """

    if not token_addresses:
        return []

    return fetch_logs_retrying(
        w3=provider,
        start_block=start_block,
        end_block=end_block,
        address=token_addresses,
        topic_signature=[
            [
                AaveV3GhoDebtTokenEvent.DISCOUNT_PERCENT_UPDATED.value,
                AaveV3PoolConfigEvent.UPGRADED.value,
                AaveV3ScaledTokenEvent.BALANCE_TRANSFER.value,
                AaveV3ScaledTokenEvent.BURN.value,
                AaveV3ScaledTokenEvent.MINT.value,
                # Include ERC20 Transfer events for proper paired transfer matching
                ERC20Event.TRANSFER.value,
            ]
        ],
    )


def _fetch_stk_aave_events(
    provider: ProviderAdapter,
    discount_token: ChecksumAddress | None,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch stkAAVE events including STAKED and REDEEM for classification.
    """

    if not discount_token:
        return []
    return fetch_logs_retrying(
        w3=provider,
        start_block=start_block,
        end_block=end_block,
        address=[discount_token],
        topic_signature=[
            [
                AaveV3StkAaveEvent.REDEEM.value,
                AaveV3StkAaveEvent.STAKED.value,
                ERC20Event.TRANSFER.value,
            ]
        ],
    )


def _fetch_address_provider_events(
    provider: ProviderAdapter,
    provider_address: ChecksumAddress,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch Pool Address Provider events for contract updates.
    """

    return fetch_logs_retrying(
        w3=provider,
        start_block=start_block,
        end_block=end_block,
        address=[provider_address],
        topic_signature=[
            [
                AaveV3PoolConfigEvent.ADDRESS_SET.value,
                AaveV3PoolConfigEvent.POOL_CONFIGURATOR_UPDATED.value,
                AaveV3PoolConfigEvent.POOL_DATA_PROVIDER_UPDATED.value,
                AaveV3PoolConfigEvent.POOL_UPDATED.value,
                AaveV3PoolConfigEvent.PRICE_ORACLE_UPDATED.value,
                AaveV3PoolConfigEvent.PROXY_CREATED.value,
            ]
        ],
    )


def _fetch_discount_config_events(
    provider: ProviderAdapter,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch discount-related events from any contract (not address-specific).
    """

    return fetch_logs_retrying(
        w3=provider,
        start_block=start_block,
        end_block=end_block,
        topic_signature=[
            [
                AaveV3GhoDebtTokenEvent.DISCOUNT_RATE_STRATEGY_UPDATED.value,
                AaveV3GhoDebtTokenEvent.DISCOUNT_TOKEN_UPDATED.value,
            ]
        ],
    )


def _fetch_oracle_events(
    provider: ProviderAdapter,
    oracle_address: ChecksumAddress | None,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch AaveOracle events for oracle configuration changes.

    If oracle_address is None, fetches events from all contracts (discovery mode).
    """

    return fetch_logs_retrying(
        w3=provider,
        start_block=start_block,
        end_block=end_block,
        address=[oracle_address] if oracle_address is not None else None,
        topic_signature=[
            [
                AaveV3OracleEvent.ASSET_SOURCE_UPDATED.value,
            ]
        ],
    )


def _log_event_categorization(
    *,
    topic: HexBytesLike,
    event_address: ChecksumAddress,
    gho_asset: AaveGhoToken,
) -> None:
    """
    Validate event topic is recognized and log its categorization.

    Raises:
        ValueError: If the topic is not recognized.
    """

    # Check module-level cache for Aave events
    if topic in _AAVE_EVENT_TOPIC_TO_CATEGORY:
        category = _AAVE_EVENT_TOPIC_TO_CATEGORY[topic]
    elif topic == ERC20Event.TRANSFER.value:
        if event_address == (gho_asset.v_gho_discount_token if gho_asset else None):
            category = "stkAAVE_TRANSFER"
        else:
            category = "ERC20_TRANSFER"
    else:
        msg = f"Could not identify topic: {topic.to_0x_hex()}"
        logger.error(f"_build_transaction_contexts: {msg}")
        raise ValueError(msg)

    logger.debug(f"_build_transaction_contexts: categorized as {category} event")


def _build_transaction_contexts(
    *,
    events: list[LogReceipt],
    market: AaveV3Market,
    session: Session,
    provider: ProviderAdapter,
    gho_asset: AaveGhoToken,
    pool_contract: AaveV3Contract,
) -> dict[HexBytes, TransactionContext]:
    """
    Group events by transaction with full categorization.
    """

    assert pool_contract.revision is not None

    contexts: dict[HexBytes, TransactionContext] = {}

    for event in sorted(events, key=itemgetter("blockNumber", "logIndex")):
        tx_hash = event["transactionHash"]
        block_num = event["blockNumber"]
        topic = event["topics"][0]
        event_address = event["address"]

        logger.debug(
            f"_build_transaction_contexts: processing event "
            f"block={block_num} tx={tx_hash.to_0x_hex()} "
            f"topic={topic.to_0x_hex()} addr={event_address}"
        )

        if tx_hash not in contexts:
            logger.debug(
                f"_build_transaction_contexts: creating new context for tx={tx_hash.to_0x_hex()}"
            )
            contexts[tx_hash] = TransactionContext(
                provider=provider,
                tx_hash=tx_hash,
                block_number=block_num,
                events=[],
                market=market,
                session=session,
                gho_asset=gho_asset,
                pool_revision=pool_contract.revision,
            )

        ctx = contexts[tx_hash]
        ctx.events.append(event)

        # Track users involved in stkAAVE transfers (needed for discount calculations)
        if topic == ERC20Event.TRANSFER.value and event_address == (
            gho_asset.v_gho_discount_token if gho_asset else None
        ):
            from_addr = decode_address(event["topics"][1])
            to_addr = decode_address(event["topics"][2])
            if from_addr != ZERO_ADDRESS:
                ctx.stk_aave_transfer_users.add(from_addr)
            if to_addr != ZERO_ADDRESS:
                ctx.stk_aave_transfer_users.add(to_addr)

        # Validate and log event categorization
        _log_event_categorization(
            topic=topic,
            event_address=event_address,
            gho_asset=gho_asset,
        )

    return contexts


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

    pool_address_provider = _get_contract(
        session=session,
        market=market,
        contract_name="POOL_ADDRESS_PROVIDER",
    )
    assert pool_address_provider is not None

    for event in _fetch_address_provider_events(
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
    pool_configurator = _get_contract(
        session=session,
        market=market,
        contract_name="POOL_CONFIGURATOR",
    )
    if pool_configurator is not None:
        for event in _fetch_reserve_initialization_events(
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

    pool = _get_contract(
        session=session,
        market=market,
        contract_name="POOL",
    )
    if pool is None:
        # Pool not initialized yet, skip to next chunk
        logger.warning(f"Pool not initialized for market {market.id}, skipping")
        return

    pool_events = _fetch_pool_events(
        provider=provider,
        pool_address=pool.address,
        start_block=start_block,
        end_block=end_block,
    )
    all_events.extend(pool_events)

    # Fetch oracle events - discover oracle from events if not yet known
    oracle_contract = _get_contract(
        session=session,
        market=market,
        contract_name="PRICE_ORACLE",
    )
    oracle_address = (
        get_checksum_address(oracle_contract.address) if oracle_contract is not None else None
    )
    oracle_events = _fetch_oracle_events(
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

    scaled_token_events = _fetch_scaled_token_events(
        provider=provider,
        token_addresses=list(known_scaled_token_addresses),
        start_block=start_block,
        end_block=end_block,
    )
    all_events.extend(scaled_token_events)

    discount_config_events = _fetch_discount_config_events(
        provider=provider,
        start_block=start_block,
        end_block=end_block,
    )
    all_events.extend(discount_config_events)

    gho_asset = _get_gho_asset(session=session, market=market)

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
            _fetch_stk_aave_events(
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
                _verify_positions_for_users(
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
        _verify_positions_for_users(
            provider=provider,
            market=market,
            session=session,
            gho_asset=gho_asset,
            block_number=last_verified_block,
            show_progress=show_progress,
            user_addresses=users_modified_this_block,
        )

    if verify_chunk and not verify_block and users_modified_this_chunk:
        _verify_positions_for_users(
            provider=provider,
            market=market,
            session=session,
            gho_asset=gho_asset,
            block_number=end_block,
            show_progress=show_progress,
            user_addresses=users_modified_this_chunk,
        )

    logger.info(f"{market} successfully updated to block {end_block:,}")
