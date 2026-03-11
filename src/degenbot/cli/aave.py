import sys
from enum import Enum
from operator import itemgetter
from pathlib import Path
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
from web3 import Web3
from web3.exceptions import ContractLogicError
from web3.types import LogReceipt

from degenbot.aave.deployments import EthereumMainnetAaveV3
from degenbot.aave.events import (
    AaveV3GhoDebtTokenEvent,
    AaveV3PoolConfigEvent,
    AaveV3PoolEvent,
    AaveV3ScaledTokenEvent,
    AaveV3StkAaveEvent,
    ERC20Event,
)
from degenbot.aave.libraries.token_math import TokenMathFactory
from degenbot.aave.libraries.wad_ray_math import wad_mul
from degenbot.aave.processors import (
    CollateralBurnEvent,
    CollateralMintEvent,
    DebtBurnEvent,
    DebtMintEvent,
    TokenProcessorFactory,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.cli import cli
from degenbot.cli.aave_debug_logger import aave_debug_logger
from degenbot.cli.aave_event_matching import EventMatchResult, OperationAwareEventMatcher
from degenbot.cli.aave_transaction_operations import (
    Operation,
    OperationType,
    ScaledTokenEvent,
    TransactionOperationsParser,
    TransactionValidationError,
)
from degenbot.cli.aave_types import TransactionContext
from degenbot.cli.aave_utils import decode_address
from degenbot.cli.utils import get_web3_from_config
from degenbot.config import settings
from degenbot.constants import DEAD_ADDRESS, ERC_1967_IMPLEMENTATION_SLOT, ZERO_ADDRESS
from degenbot.database import db_session
from degenbot.database.models.aave import (
    AaveGhoToken,
    AaveV3Asset,
    AaveV3CollateralPosition,
    AaveV3Contract,
    AaveV3DebtPosition,
    AaveV3Market,
    AaveV3User,
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

if TYPE_CHECKING:
    from eth_typing.evm import BlockParams

    from degenbot.aave.processors.base import BurnResult, MintResult


class TokenType(Enum):
    """Token type for Aave V3 asset lookups."""

    COLLATERAL = "a_token_id"
    DEBT = "v_token_id"


class UserOperation(Enum):
    """User operation types for Aave V3 token events."""

    DEPOSIT = "DEPOSIT"
    WITHDRAW = "WITHDRAW"
    BORROW = "BORROW"
    REPAY = "REPAY"
    GHO_BORROW = "GHO BORROW"
    GHO_REPAY = "GHO REPAY"
    GHO_INTEREST_ACCRUAL = "GHO INTEREST ACCRUAL"
    AAVE_STAKED = "AAVE STAKED"
    AAVE_REDEEM = "AAVE REDEEM"
    STKAAVE_TRANSFER = "stkAAVE TRANSFER"


GHO_VARIABLE_DEBT_TOKEN_ADDRESS = get_checksum_address("0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B")


event_in_process: LogReceipt


class WadRayMathLibrary(Protocol):
    def ray_div(self, a: int, b: int) -> int: ...
    def ray_mul(self, a: int, b: int) -> int: ...


def _extract_user_addresses_from_event(event: LogReceipt) -> set[ChecksumAddress]:
    """
    Extract user addresses from an Aave event.

    Returns a set of all user addresses (senders, recipients, onBehalfOf, etc.)
    that are involved in the event.
    """

    user_addresses: set[ChecksumAddress] = set()
    topic = event["topics"][0]

    if topic == AaveV3ScaledTokenEvent.MINT.value:
        user_addresses.add(decode_address(event["topics"][2]))

    elif topic == AaveV3ScaledTokenEvent.BURN.value:
        user_addresses.add(decode_address(event["topics"][1]))

    elif topic == AaveV3ScaledTokenEvent.BALANCE_TRANSFER.value:
        user_addresses.add(decode_address(event["topics"][1]))
        user_addresses.add(decode_address(event["topics"][2]))

    elif topic == ERC20Event.TRANSFER.value:
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
        user_addresses.add(decode_address(event["topics"][1]))

    elif topic in {
        AaveV3PoolEvent.BORROW.value,
        AaveV3PoolEvent.REPAY.value,
        AaveV3PoolEvent.SUPPLY.value,
        AaveV3PoolEvent.WITHDRAW.value,
    }:
        user_addresses.add(decode_address(event["topics"][2]))

    elif topic == AaveV3PoolEvent.LIQUIDATION_CALL.value:
        user_addresses.add(decode_address(event["topics"][3]))

    elif topic in {AaveV3StkAaveEvent.STAKED.value, AaveV3StkAaveEvent.REDEEM.value}:
        user_addresses.add(decode_address(event["topics"][1]))
        user_addresses.add(decode_address(event["topics"][2]))

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

    pool_address_provider = EthereumMainnetAaveV3.pool_address_provider

    w3 = get_web3_from_config(chain_id=chain_id)

    (market_name,) = raw_call(
        w3=w3,
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

            # Create GHO token entry if it doesn't exist. GHO tokens are chain-unique, so we create
            # a single entry that all markets on this chain will share.
            if (
                gho_token := session.scalar(
                    select(Erc20TokenTable).where(
                        Erc20TokenTable.address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS,
                        Erc20TokenTable.chain == chain_id,
                    )
                )
            ) is None:
                gho_token = Erc20TokenTable(
                    chain=chain_id,
                    address=GHO_VARIABLE_DEBT_TOKEN_ADDRESS,
                )
                session.add(gho_token)
                session.flush()

            if (
                gho_entry := session.scalar(
                    select(AaveGhoToken).where(AaveGhoToken.token_id == gho_token.id)
                )
            ) is None:
                gho_entry = AaveGhoToken(token_id=gho_token.id)
                session.add(gho_entry)

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
    "--verify",
    "verify",
    is_flag=True,
    default=True,
    show_default=True,
    help=(
        "Verify collateral and debt position balances, staked AAVE balances, and GHO discount "
        "amounts at block boundaries."
    ),
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
    "--progress-bar",
    "show_progress",
    is_flag=True,
    default=True,
    show_default=True,
    help="Show progress bars.",
    envvar="DEGENBOT_PROGRESS_BAR",
    show_envvar=True,
)
@click.option(
    "--debug-output",
    "debug_output",
    default=None,
    help="Path to write structured JSON debug output for machine analysis.",
)
def aave_update(
    *,
    chunk_size: int,
    to_block: str,
    verify: bool,
    stop_after_one_chunk: bool,
    show_progress: bool,
    debug_output: str | None,
) -> None:
    """
    Update positions for active Aave markets.

    Processes blockchain events from the last updated block to the specified block,
    updating all user positions, interest rates, and indices in the database.

    Args:
        chunk_size: Maximum number of blocks to process before committing changes.
        to_block: Target block identifier (e.g., 'latest', 'latest:-64', 'finalized:128').
        verify: If True, verify position balances at every block boundary.
        stop_after_one_chunk: If True, stop after processing the first chunk.
        show_progress: Toggle display of progress bars.
        debug_output: Path to write structured JSON debug output.
    """

    if debug_output:
        aave_debug_logger.configure(
            output_path=Path(debug_output),
        )
        logger.info(f"Debug output enabled: {debug_output}")

    with (  # noqa:PLR1702
        db_session() as session,
        logging_redirect_tqdm(
            loggers=[logger],
        ),
    ):
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
            w3 = get_web3_from_config(chain_id=chain_id)

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
                    get_number_for_block_identifier(identifier=block_tag, w3=w3) + block_offset
                )

            current_block_number = get_number_for_block_identifier(identifier="latest", w3=w3)
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

            markets_to_update: set[AaveV3Market] = set()

            while True:
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
                    # Configure debug logger for this market
                    if aave_debug_logger.is_enabled():
                        aave_debug_logger.configure(
                            chain_id=ChainId(chain_id),
                            market_id=market.id,
                        )

                    try:
                        update_aave_market(
                            w3=w3,
                            start_block=working_start_block,
                            end_block=working_end_block,
                            market=market,
                            session=session,
                            verify=verify,
                            show_progress=show_progress,
                        )
                    except Exception as e:  # noqa: BLE001
                        logger.exception("")

                        # Log structured exception data for autonomous analysis
                        if aave_debug_logger.is_enabled():
                            extra_context = {
                                "chain_id": chain_id,
                                "market_id": market.id,
                                "market_name": market.name,
                                "start_block": working_start_block,
                                "end_block": working_end_block,
                            }
                            aave_debug_logger.log_exception(
                                exc=e,
                                extra_context=extra_context,
                            )
                            aave_debug_logger.close()

                        sys.exit(1)

                # At this point, all markets have been updated and the invariant checks have
                # passed, so stamp the update block and commit to the DB
                for market in markets_to_update:
                    market.last_update_block = working_end_block

                # Perform full verification when the chunk spans a verification interval
                full_verification_interval = 250_000
                if (
                    verify
                    and working_end_block // full_verification_interval
                    != working_start_block // full_verification_interval
                ):
                    for market in markets_to_update:
                        _verify_all_positions(
                            w3=w3,
                            market=market,
                            session=session,
                            block_number=working_end_block,
                            show_progress=show_progress,
                        )

                    session.commit()
                    backup_sqlite_database(
                        settings.database.path,
                        suffix=f"{working_end_block}",
                        skip_confirmation=True,
                    )
                    logger.info(f"Created database backup at block {working_end_block:,}")

                _cleanup_zero_balance_positions(session=session, market=market)

                markets_to_update.clear()
                session.commit()

                if working_end_block == last_block or stop_after_one_chunk:
                    break
                working_start_block = working_end_block + 1

                block_pbar.n = working_end_block - initial_start_block

            block_pbar.close()


def _process_asset_initialization_event(
    w3: Web3,
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
    (_, v_token_address, _) = eth_abi.abi.decode(
        types=["address", "address", "address"], data=event["data"]
    )
    v_token_address = get_checksum_address(v_token_address)

    erc20_token_in_db = _get_or_create_erc20_token(
        session=session,
        chain_id=market.chain_id,
        token_address=asset_address,
    )
    a_token = _get_or_create_erc20_token(
        session=session,
        chain_id=market.chain_id,
        token_address=a_token_address,
    )
    v_token = _get_or_create_erc20_token(
        session=session,
        chain_id=market.chain_id,
        token_address=v_token_address,
    )

    # Per EIP-1967, the implementation address is stored at a known storage slot
    (atoken_implementation_address,) = eth_abi.abi.decode(
        types=["address"],
        data=w3.eth.get_storage_at(
            account=get_checksum_address(a_token_address),
            position=ERC_1967_IMPLEMENTATION_SLOT,
            block_identifier=event["blockNumber"],
        ),
    )
    atoken_implementation_address = get_checksum_address(atoken_implementation_address)

    (vtoken_implementation_address,) = eth_abi.abi.decode(
        types=["address"],
        data=w3.eth.get_storage_at(
            account=get_checksum_address(v_token_address),
            position=ERC_1967_IMPLEMENTATION_SLOT,
            block_identifier=event["blockNumber"],
        ),
    )
    vtoken_implementation_address = get_checksum_address(vtoken_implementation_address)

    (atoken_revision,) = raw_call(
        w3=w3,
        address=atoken_implementation_address,
        calldata=encode_function_calldata(
            function_prototype="ATOKEN_REVISION()",
            function_arguments=None,
        ),
        return_types=["uint256"],
    )
    (vtoken_revision,) = raw_call(
        w3=w3,
        address=vtoken_implementation_address,
        calldata=encode_function_calldata(
            function_prototype="DEBT_TOKEN_REVISION()",
            function_arguments=None,
        ),
        return_types=["uint256"],
    )

    session.add(
        AaveV3Asset(
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
    )
    logger.info(f"Added new Aave V3 asset: {asset_address}")


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
    tx_context: TransactionContext,
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

    logger.debug(f"Processing discount token updated event at block {event['blockNumber']}")

    old_discount_token_address = decode_address(event["topics"][1])
    new_discount_token_address = decode_address(event["topics"][2])

    tx_context.gho_asset.v_gho_discount_token = new_discount_token_address

    logger.info(
        f"SET NEW DISCOUNT TOKEN: {old_discount_token_address} -> {new_discount_token_address}"
    )


def _process_discount_rate_strategy_updated_event(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
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

    logger.debug(f"Processing discount rate strategy updated event at block {event['blockNumber']}")

    old_discount_rate_strategy_address = decode_address(event["topics"][1])
    new_discount_rate_strategy_address = decode_address(event["topics"][2])

    tx_context.gho_asset.v_gho_discount_rate_strategy = new_discount_rate_strategy_address

    logger.info(
        f"SET NEW DISCOUNT RATE STRATEGY: {old_discount_rate_strategy_address} -> "
        f"{new_discount_rate_strategy_address}"
    )


def _get_or_init_stk_aave_balance(
    *,
    user: AaveV3User,
    discount_token: ChecksumAddress | None,
    block_number: int,
    w3: Web3,
    tx_context: TransactionContext | None = None,
    log_index: int | None = None,
) -> int:
    """
    Get user's last-known stkAAVE balance.

    If the balance is unknown, perform a contract call at the previous block to ensure
    the balance check is performed before any events in the current block are processed.

    When tx_context and log_index are provided and there are pending stkAAVE transfers
    for this user (transfers with log_index > current log_index), returns the predicted
    balance including the pending delta. This handles the reentrancy case where the GHO
    debt token contract sees the post-transfer balance before the Transfer event is emitted.
    """

    # If discount_token is None (revision 4+), return 0
    if discount_token is None:
        return 0

    if user.stk_aave_balance is None:
        balance: int
        (balance,) = raw_call(
            w3=w3,
            address=discount_token,
            calldata=encode_function_calldata(
                function_prototype="balanceOf(address)",
                function_arguments=[user.address],
            ),
            return_types=["uint256"],
            block_identifier=block_number - 1,
        )
        user.stk_aave_balance = balance

    assert user.stk_aave_balance is not None

    # Check if we need to account for pending transfers due to reentrancy
    # This happens when stkAAVE is transferred during GHO discount updates,
    # and the GHO contract sees the post-transfer balance before the Transfer event
    if (
        tx_context is not None
        and log_index is not None
        and user.address in tx_context.stk_aave_transfer_users
        and discount_token is not None
    ):
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
    assert block_number is not None

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
            discount_token=tx_context.gho_asset.v_gho_discount_token,
            block_number=event["blockNumber"],
            w3=tx_context.w3,
        )
    if to_user is not None and to_user.stk_aave_balance is None:
        _get_or_init_stk_aave_balance(
            user=to_user,
            discount_token=tx_context.gho_asset.v_gho_discount_token,
            block_number=event["blockNumber"],
            w3=tx_context.w3,
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
    if tx_context is not None:
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
            token_address=get_checksum_address(event["address"]),
            token_type=TokenType.COLLATERAL,
        )
    ) is not None:
        (atoken_revision,) = raw_call(
            w3=tx_context.w3,
            address=new_implementation_address,
            calldata=encode_function_calldata(
                function_prototype="ATOKEN_REVISION()",
                function_arguments=None,
            ),
            return_types=["uint256"],
        )
        aave_collateral_asset.a_token_revision = atoken_revision
        logger.info(
            f"Upgraded aToken revision for {aave_collateral_asset.a_token.address} "
            f"to {atoken_revision}"
        )
    elif (
        aave_debt_asset := _get_asset_by_token_type(
            session=tx_context.session,
            market=tx_context.market,
            token_address=get_checksum_address(event["address"]),
            token_type=TokenType.DEBT,
        )
    ) is not None:
        (vtoken_revision,) = raw_call(
            w3=tx_context.w3,
            address=new_implementation_address,
            calldata=encode_function_calldata(
                function_prototype="DEBT_TOKEN_REVISION()",
                function_arguments=None,
            ),
            return_types=["uint256"],
        )
        aave_debt_asset.v_token_revision = vtoken_revision
        logger.info(
            f"Upgraded vToken revision for {aave_debt_asset.v_token.address} to {vtoken_revision}"
        )

        # Handle GHO discount deprecation on upgrade to revision 4+
        if (
            aave_debt_asset.v_token.address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS
            and vtoken_revision >= 4  # noqa: PLR2004
        ):
            gho_asset_db = _get_gho_asset(tx_context.session, tx_context.market)
            gho_asset_db.v_gho_discount_token = None
            gho_asset_db.v_gho_discount_rate_strategy = None

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
            tx_context.session.flush()

    else:
        token_address = get_checksum_address(event["address"])
        msg = f"Unknown token type for address {token_address}. Expected aToken or vToken."
        raise ValueError(msg)


def _get_gho_vtoken_revision(
    session: Session,
    market: AaveV3Market,
) -> int | None:
    """Get the GHO vToken revision from market assets."""
    return session.scalar(
        select(AaveV3Asset.v_token_revision)
        .join(Erc20TokenTable, AaveV3Asset.v_token_id == Erc20TokenTable.id)
        .where(
            AaveV3Asset.market_id == market.id,
            Erc20TokenTable.address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS,
        )
    )


def _is_discount_supported(
    session: Session,
    market: AaveV3Market,
) -> bool:
    """Check if GHO discount mechanism is supported (revision 2 or 3)."""
    revision = _get_gho_vtoken_revision(session, market)
    return revision is not None and revision < 4  # noqa: PLR2004


def _get_or_create_user(
    *,
    tx_context: TransactionContext,
    user_address: ChecksumAddress,
    block_number: int,
) -> AaveV3User:
    """
    Get existing user or create new one with default e_mode.

    When creating a new user, if w3 and block_number are provided and the user
    has an existing GHO debt position, their discount percent will be fetched
    from the contract to properly initialize their gho_discount value.
    """

    user = tx_context.session.scalar(
        select(AaveV3User).where(
            AaveV3User.address == user_address,
            AaveV3User.market_id == tx_context.market.id,
        )
    )

    if user is None:
        # When creating a new user, check if they have a GHO discount on-chain
        # to properly initialize their gho_discount value
        gho_discount = 0

        # Only fetch discount if mechanism is supported (revision 2 or 3)
        if tx_context.gho_asset.v_gho_discount_token is not None and _is_discount_supported(
            session=tx_context.session,
            market=tx_context.market,
        ):
            try:
                (discount_percent,) = raw_call(
                    w3=tx_context.w3,
                    address=GHO_VARIABLE_DEBT_TOKEN_ADDRESS,
                    calldata=encode_function_calldata(
                        function_prototype="getDiscountPercent(address)",
                        function_arguments=[user_address],
                    ),
                    return_types=["uint256"],
                    block_identifier=block_number,
                )
                gho_discount = discount_percent
            except (
                ValueError,
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
        logger.debug(
            f"CREATING USER: {user_address} gho_discount={gho_discount} block={block_number}"
        )

        # Log user creation to structured debug logger
        if aave_debug_logger.is_enabled():
            tx_hash = (
                event_in_process["transactionHash"]
                if "event_in_process" in globals() and event_in_process is not None
                else HexBytes("0x")
            )
            aave_debug_logger.log_user_creation(
                user_address=user_address,
                block_number=block_number,
                tx_hash=tx_hash,
                gho_discount=gho_discount,
                e_mode=0,
            )

        user = AaveV3User(
            market_id=tx_context.market.id,
            address=user_address,
            e_mode=0,
            gho_discount=gho_discount,
        )
        tx_context.market.users.append(user)
        tx_context.session.add(user)
        tx_context.session.flush()

    return user


def _get_or_create_erc20_token(
    session: Session,
    chain_id: int,
    token_address: ChecksumAddress,
) -> Erc20TokenTable:
    """
    Get existing ERC20 token or create new one.
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
        session.add(token)
        session.flush()
    return token


def _get_or_create_position[T: AaveV3CollateralPosition | AaveV3DebtPosition](
    *,
    session: Session,
    user: AaveV3User,
    asset_id: int,
    position_table: type[T],
    tx_context: TransactionContext | None = None,
) -> T:
    """
    Get existing position or create new one with zero balance.
    """

    # Check if we have a tracked position for this user/asset in this transaction
    if tx_context is not None:
        user_address = get_checksum_address(user.address)
        cache_key = (user_address, asset_id)
        if cache_key in tx_context.modified_positions:
            return cast("T", tx_context.modified_positions[cache_key])

    # Query the database directly for the position
    existing_position = session.scalar(
        select(position_table).where(
            position_table.user_id == user.id,
            position_table.asset_id == asset_id,
        )
    )

    if existing_position is not None:
        # Track this position for the rest of the transaction
        if tx_context is not None:
            user_address = get_checksum_address(user.address)
            tx_context.modified_positions[user_address, asset_id] = existing_position
        return existing_position

    # Create new position if none exists
    new_position = cast("T", position_table(user_id=user.id, asset_id=asset_id, balance=0))
    session.add(new_position)
    session.flush()

    # Track the new position
    if tx_context is not None:
        user_address = get_checksum_address(user.address)
        tx_context.modified_positions[user_address, asset_id] = new_position
    return new_position


def _get_or_create_collateral_position(
    session: Session,
    user: AaveV3User,
    asset_id: int,
    tx_context: TransactionContext | None = None,
) -> AaveV3CollateralPosition:
    """
    Get existing collateral position or create new one with zero balance.
    """

    return _get_or_create_position(
        session=session,
        user=user,
        asset_id=asset_id,
        position_table=AaveV3CollateralPosition,
        tx_context=tx_context,
    )


def _get_or_create_debt_position(
    session: Session,
    user: AaveV3User,
    asset_id: int,
    tx_context: TransactionContext | None = None,
) -> AaveV3DebtPosition:
    """
    Get existing debt position or create new one with zero balance.
    """

    return _get_or_create_position(
        session=session,
        user=user,
        asset_id=asset_id,
        position_table=AaveV3DebtPosition,
        tx_context=tx_context,
    )


def _get_gho_asset(
    session: Session,
    market: AaveV3Market,
) -> AaveGhoToken:
    """
    Get GHO token asset for a given market.

    GHO tokens are chain-unique: multiple Aave markets on the same chain share
    a single GHO token. Query by chain_id to retrieve the shared configuration.
    """
    gho_asset = session.scalar(
        select(AaveGhoToken).join(Erc20TokenTable).where(Erc20TokenTable.chain == market.chain_id)
    )
    if gho_asset is None:
        msg = (
            f"GHO token not found for chain {market.chain_id}. "
            "Ensure that market has been activated."
        )
        raise ValueError(msg)
    return gho_asset


def _fetch_discount_token_from_contract(
    w3: Web3,
    gho_asset: AaveGhoToken,
    block_number: int,
) -> ChecksumAddress | None:
    """Fetch the discount token address from the GHO vToken contract.

    This is used to initialize v_gho_discount_token when it's not set in the database
    and no DISCOUNT_TOKEN_UPDATED events exist in the current block range.
    """
    try:
        # GHO vToken has a getDiscountToken() function
        (discount_token,) = raw_call(
            w3=w3,
            address=gho_asset.token.address,
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
) -> AaveV3Contract:
    """
    Get contract by name for a given market.
    """

    contract = session.scalar(
        select(AaveV3Contract).where(
            AaveV3Contract.market_id == market.id,
            AaveV3Contract.name == contract_name,
        )
    )
    if contract is None:
        msg = f"{contract_name} not found for market {market.id}"
        raise ValueError(msg)
    return contract


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
        case TokenType.COLLATERAL:
            return session.scalar(
                select(AaveV3Asset)
                .join(Erc20TokenTable, AaveV3Asset.a_token_id == Erc20TokenTable.id)
                .where(
                    AaveV3Asset.market_id == market.id,
                    Erc20TokenTable.address == token_address,
                )
                .options(joinedload(AaveV3Asset.a_token))
            )
        case TokenType.DEBT:
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


def _get_current_borrow_index_from_pool(
    w3: Web3,
    pool_address: ChecksumAddress,
    underlying_asset_address: ChecksumAddress,
    block_number: int,
) -> int | None:
    """
    Fetch the current borrow index from the Aave Pool contract.

    This is used when the asset's cached borrow_index is 0 (not yet updated
    by a ReserveDataUpdated event) to get the current global index.

    Args:
        w3: Web3 instance
        pool_address: The Aave Pool contract address
        underlying_asset_address: The underlying asset address (e.g., GHO token)
        block_number: The block number to query at

    Returns:
        The current borrow index, or None if the call fails
    """
    try:
        borrow_index: int
        (borrow_index,) = raw_call(
            w3=w3,
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
    w3: Web3,
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
    if revision is None or revision >= 4:  # noqa:PLR2004
        logger.debug(f"Skipping GHO discount verification (revision {revision} < 4 not met)")
        return

    if gho_asset.v_gho_discount_token is None:
        return

    if user_addresses is not None and len(user_addresses) == 0:
        return

    stmt = select(AaveV3User).where(AaveV3User.market_id == market.id)
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
                w3=w3,
                address=GHO_VARIABLE_DEBT_TOKEN_ADDRESS,
                calldata=encode_function_calldata(
                    function_prototype="getDiscountPercent(address)",
                    function_arguments=[user.address],
                ),
                return_types=["uint256"],
                block_identifier=block_number,
            )
        except (ValueError, RuntimeError, eth_abi.exceptions.DecodingError, ContractLogicError):
            # Function may not exist (revision 4+ after upgrade in same block)
            # Verify that our tracked discount is 0 (the new default)
            discount_percent = 0

        assert user.gho_discount == discount_percent, (
            f"User {user.address}: GHO discount {user.gho_discount} "
            f"does not match GHO vDebtToken contract ({discount_percent}) "
            f"@ {GHO_VARIABLE_DEBT_TOKEN_ADDRESS} at block {block_number}"
        )


def _verify_stk_aave_balances(
    *,
    w3: Web3,
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
            w3=w3,
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

    This cleanup runs after chunk verification to remove positions that no longer
    hold any balance, keeping the database lean.
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


def _verify_all_positions(
    *,
    w3: Web3,
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
        w3: Web3 instance for blockchain calls
        market: The Aave V3 market to verify
        session: Database session
        block_number: The block number to verify against
        no_progress: If True, disable progress bars
    """

    logger.info(f"Performing full verification of all positions at block {block_number:,}")

    session.flush()

    # Verify all collateral positions
    _verify_scaled_token_positions(
        w3=w3,
        market=market,
        session=session,
        position_table=AaveV3CollateralPosition,
        block_number=block_number,
        show_progress=show_progress,
        user_addresses=None,
    )

    # Verify all debt positions
    _verify_scaled_token_positions(
        w3=w3,
        market=market,
        session=session,
        position_table=AaveV3DebtPosition,
        block_number=block_number,
        show_progress=show_progress,
        user_addresses=None,
    )

    # Get GHO asset for stkAAVE and discount verification
    gho_asset = _get_gho_asset(session=session, market=market)

    # Verify all stkAAVE balances
    _verify_stk_aave_balances(
        w3=w3,
        session=session,
        market=market,
        gho_asset=gho_asset,
        block_number=block_number,
        show_progress=show_progress,
        user_addresses=None,
    )

    # Verify all GHO discount amounts
    _verify_gho_discount_amounts(
        w3=w3,
        session=session,
        market=market,
        gho_asset=gho_asset,
        block_number=block_number,
        show_progress=show_progress,
        user_addresses=None,
    )


def _verify_scaled_token_positions(
    *,
    w3: Web3,
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

    desc = (
        "Verifying collateral positions"
        if position_table is AaveV3CollateralPosition
        else "Verifying debt positions"
    )

    if user_addresses is not None and len(user_addresses) == 0:
        return

    # Log verification start
    if aave_debug_logger.is_enabled() and user_addresses is not None:
        position_type = "collateral" if position_table is AaveV3CollateralPosition else "debt"
        aave_debug_logger.log_verification_start(
            block_number=block_number,
            user_addresses=[addr.lower() for addr in user_addresses],
            position_type=position_type,
        )

    # Query users for this market
    stmt = select(AaveV3User).where(AaveV3User.market_id == market.id)
    if user_addresses is not None:
        stmt = stmt.where(AaveV3User.address.in_(user_addresses))

    users_to_verify = session.scalars(stmt).all()

    for user in tqdm.tqdm(
        users_to_verify,
        desc=desc,
        leave=False,
        disable=not show_progress,
    ):
        if user.address in {DEAD_ADDRESS, ZERO_ADDRESS}:
            continue

        positions = session.scalars(
            select(position_table)
            .where(position_table.user_id == user.id)
            .options(
                joinedload(position_table.asset).joinedload(AaveV3Asset.a_token),
                joinedload(position_table.asset).joinedload(AaveV3Asset.v_token),
            )
        ).all()

        for position in positions:
            position = cast("AaveV3CollateralPosition | AaveV3DebtPosition", position)

            if position_table is AaveV3CollateralPosition:
                token_address = get_checksum_address(position.asset.a_token.address)
            elif position_table is AaveV3DebtPosition:
                token_address = get_checksum_address(position.asset.v_token.address)
            else:
                msg = f"Unknown position table type: {position_table}"
                raise ValueError(msg)

            (actual_scaled_balance,) = raw_call(
                w3=w3,
                address=token_address,
                calldata=encode_function_calldata(
                    function_prototype="scaledBalanceOf(address)",
                    function_arguments=[user.address],
                ),
                return_types=["uint256"],
                block_identifier=block_number,
            )

            assert actual_scaled_balance == position.balance, (
                f"Balance verification failure for {position.asset}. "
                f"User {position.user} scaled balance ({position.balance}) does not match contract "
                f"balance ({actual_scaled_balance}) at block {block_number}"
            )

            (actual_last_index,) = raw_call(
                w3=w3,
                address=token_address,
                calldata=encode_function_calldata(
                    function_prototype="getPreviousIndex(address)",
                    function_arguments=[user.address],
                ),
                return_types=["uint256"],
                block_identifier=block_number,
            )

            assert actual_last_index == position.last_index, (
                f"Index verification failure for {position.asset}. "
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

    logger.debug(
        f"Processing scaled token operation ({type(event).__name__}) for revision "
        f"{scaled_token_revision}"
    )
    logger.debug(position)

    match event:
        case CollateralMintEvent():
            assert isinstance(position, AaveV3CollateralPosition)
            collateral_processor = TokenProcessorFactory.get_collateral_processor(
                scaled_token_revision
            )
            mint_result: MintResult = collateral_processor.process_mint_event(
                event_data=event,
                previous_balance=position.balance,
                previous_index=position.last_index or 0,
            )
            position.balance += mint_result.balance_delta
            position.last_index = mint_result.new_index
            return UserOperation.WITHDRAW if mint_result.is_repay else UserOperation.DEPOSIT

        case CollateralBurnEvent():
            assert isinstance(position, AaveV3CollateralPosition)
            collateral_processor = TokenProcessorFactory.get_collateral_processor(
                scaled_token_revision
            )
            burn_result: BurnResult = collateral_processor.process_burn_event(
                event_data=event,
                previous_balance=position.balance,
                previous_index=position.last_index or 0,
            )
            logger.debug(
                f"_process_scaled_token_operation burn: delta={burn_result.balance_delta}, "
                f"new_balance={position.balance + burn_result.balance_delta}"
            )
            position.balance += burn_result.balance_delta
            position.last_index = burn_result.new_index
            return UserOperation.WITHDRAW

        case DebtMintEvent():
            assert isinstance(position, AaveV3DebtPosition)
            debt_processor = TokenProcessorFactory.get_debt_processor(scaled_token_revision)
            debt_mint_result: MintResult = debt_processor.process_mint_event(
                event_data=event,
                previous_balance=position.balance,
                previous_index=position.last_index or 0,
                scaled_delta=event.scaled_amount,
            )
            position.balance += debt_mint_result.balance_delta
            position.last_index = debt_mint_result.new_index
            return UserOperation.REPAY if debt_mint_result.is_repay else UserOperation.BORROW

        case DebtBurnEvent():
            assert isinstance(position, AaveV3DebtPosition)
            debt_processor = TokenProcessorFactory.get_debt_processor(scaled_token_revision)
            debt_burn_result: BurnResult = debt_processor.process_burn_event(
                event_data=event,
                previous_balance=position.balance,
                previous_index=position.last_index or 0,
                scaled_delta=event.scaled_amount,
            )
            position.balance += debt_burn_result.balance_delta
            position.last_index = debt_burn_result.new_index
            return UserOperation.REPAY


def calculate_gho_discount_rate(
    debt_balance: int,
    discount_token_balance: int,
) -> int:
    """
    Calculate the GHO discount rate locally.

    Replicates the logic from the GhoDiscountRateStrategy contract at mainnet address
    0x4C38Ec4D1D2068540DfC11DFa4de41F733DDF812.

    Returns the discount rate in basis points (10000 = 100.00%).
    """

    gho_discounted_per_discount_token = 100 * 10**18
    discount_rate_bps = 3000  # 30.00%
    min_discount_token_balance = 10**15
    min_debt_token_balance = 10**18

    if discount_token_balance < min_discount_token_balance or debt_balance < min_debt_token_balance:
        return 0

    discounted_balance = wad_mul(
        a=discount_token_balance,
        b=gho_discounted_per_discount_token,
    )

    if discounted_balance >= debt_balance:
        return discount_rate_bps

    return (discounted_balance * discount_rate_bps) // debt_balance


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


def _process_transaction(tx_context: TransactionContext) -> None:
    """
    Process transaction using operation-based parsing.
    """

    # Log transaction start for debugging
    if aave_debug_logger.is_enabled():
        aave_debug_logger.log_transaction_start(
            tx_hash=tx_context.tx_hash,
            block_number=tx_context.block_number,
            event_count=len(tx_context.events),
            context=tx_context,
        )

    # Capture user discount percents before processing events
    # This ensures calculations use the discount in effect at the start of the transaction

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
        event_address = get_checksum_address(event["address"])

        if (
            topic
            in {
                AaveV3ScaledTokenEvent.MINT.value,
                AaveV3ScaledTokenEvent.BURN.value,
            }
            and event_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS
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

    for event in tx_context.events:
        topic = event["topics"][0]
        event_address = get_checksum_address(event["address"])

        # Capture GHO user discount percents for mint/burn events
        if (
            topic
            in {
                AaveV3ScaledTokenEvent.MINT.value,
                AaveV3ScaledTokenEvent.BURN.value,
            }
            and event_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS
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
                if not _is_discount_supported(
                    session=tx_context.session,
                    market=tx_context.market,
                ):
                    # Discount mechanism deprecated (revision 4+)
                    tx_context.user_discounts[user_address] = 0
                    continue

                try:
                    (discount_percent,) = raw_call(
                        w3=tx_context.w3,
                        address=GHO_VARIABLE_DEBT_TOKEN_ADDRESS,
                        calldata=encode_function_calldata(
                            function_prototype="getDiscountPercent(address)",
                            function_arguments=[user_address],
                        ),
                        return_types=["uint256"],
                        block_identifier=tx_context.block_number,
                    )
                    tx_context.user_discounts[user_address] = discount_percent
                except (
                    ValueError,
                    RuntimeError,
                    eth_abi.exceptions.DecodingError,
                    ContractLogicError,
                ):
                    # Function may not exist (revision 4+), default to 0
                    tx_context.user_discounts[user_address] = 0

    logger.debug(f"Processing _process_transaction for tx at block {tx_context.block_number}")

    # Parse events into operations
    pool_contract = _get_contract(
        session=tx_context.session,
        market=tx_context.market,
        contract_name="POOL",
    )
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

    # Sort operations by the logIndex of their first scaled token event
    # to ensure chronological processing order.
    # This fixes the transfer-to-zero-address bug where burn was processed
    # before transfer created the position.
    sorted_operations = sorted(
        tx_operations.operations,
        key=lambda op: (
            min(ev.event["logIndex"] for ev in op.scaled_token_events)
            if op.scaled_token_events
            else float("inf")
        ),
    )

    logger.debug(f"\n=== OPERATIONS FOR TX {tx_context.tx_hash.to_0x_hex()} ===")
    for op in sorted_operations:
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

    # Process stkAAVE transfers BEFORE operations to ensure stkAAVE balances
    # are up-to-date when GHO debt operations calculate discount rates.
    # This handles cases where stkAAVE transfers (e.g., rewards claims) occur
    # before GHO mint/burn events in the same transaction.
    if tx_context.gho_asset and tx_context.gho_asset.v_gho_discount_token:
        discount_token = tx_context.gho_asset.v_gho_discount_token
        for event in tx_context.events:
            topic = event["topics"][0]
            event_address = get_checksum_address(event["address"])
            if topic == ERC20Event.TRANSFER.value and event_address == discount_token:
                _process_stk_aave_transfer_event(
                    event=event,
                    contract_address=event_address,
                    tx_context=tx_context,
                )

    # Process each operation
    for operation in sorted_operations:
        logger.debug(
            f"Processing operation {operation.operation_id}: {operation.operation_type.name}"
        )
        _process_operation(
            operation=operation,
            tx_context=tx_context,
        )

    # Process non-operation events (e.g., DiscountPercentUpdated, DiscountTokenUpdated)
    # These events are not part of parsed operations but still need to be handled
    assigned_log_indices: set[int] = set()
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

    for event in tx_context.events:
        if event["logIndex"] in assigned_log_indices:
            continue

        topic = event["topics"][0]
        event_address = get_checksum_address(event["address"])

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
                tx_context=tx_context,
            )
        elif topic == AaveV3GhoDebtTokenEvent.DISCOUNT_RATE_STRATEGY_UPDATED.value:
            _process_discount_rate_strategy_updated_event(
                event=event,
                tx_context=tx_context,
            )
        # Note: stkAAVE transfers are processed before operations (see above)
        # to ensure stkAAVE balances are up-to-date when GHO operations calculate
        # discount rates. They should not be processed again here.


def _process_operation(
    *,
    operation: Operation,
    tx_context: TransactionContext,
) -> None:
    """Process a single operation."""
    logger.debug(f"Processing _process_operation for tx at block {tx_context.block_number}")

    # Create matcher for this operation
    matcher = OperationAwareEventMatcher(operation)

    # Log liquidation operations for debugging
    if operation.operation_type in {
        OperationType.LIQUIDATION,
        OperationType.GHO_LIQUIDATION,
        OperationType.SELF_LIQUIDATION,
    }:
        user_address: str = "unknown"
        collateral_asset: str = "unknown"
        debt_asset: str = "unknown"
        debt_to_cover: int = 0
        liquidated_collateral: int = 0
        liquidator: str = "unknown"

        if aave_debug_logger.is_enabled() and operation.pool_event is not None:
            # Extract liquidation data from pool event
            topics = operation.pool_event.get("topics", [])
            if len(topics) >= 4:  # noqa:PLR2004
                collateral_asset = "0x" + topics[1].hex()[-40:]
                debt_asset = "0x" + topics[2].hex()[-40:]
                user_address = "0x" + topics[3].hex()[-40:]

                # Decode liquidation data
                data = operation.pool_event.get("data", "")
                if data:
                    decoded = eth_abi.abi.decode(
                        ["uint256", "uint256", "address", "bool"],
                        data,
                    )
                    debt_to_cover = decoded[0]
                    liquidated_collateral = decoded[1]
                    liquidator = decoded[2]

                    aave_debug_logger.log_liquidation_call(
                        user_address=user_address.lower(),
                        liquidator=liquidator.lower(),
                        collateral_asset=collateral_asset.lower(),
                        debt_asset=debt_asset.lower(),
                        debt_to_cover=debt_to_cover,
                        liquidated_collateral=liquidated_collateral,
                        block_number=tx_context.block_number,
                        tx_hash=tx_context.tx_hash,
                        is_gho=operation.operation_type == OperationType.GHO_LIQUIDATION,
                    )

            # Log liquidation operation start with scaled events info
            scaled_event_types = [ev.event_type for ev in operation.scaled_token_events]

            aave_debug_logger.log_liquidation_operation_start(
                operation_id=operation.operation_id,
                user_address=user_address.lower(),
                operation_type=operation.operation_type.name,
                collateral_asset=collateral_asset.lower(),
                debt_asset=debt_asset.lower(),
                debt_to_cover=debt_to_cover,
                liquidated_collateral=liquidated_collateral,
                scaled_events=scaled_event_types,
                block_number=tx_context.block_number,
                tx_hash=tx_context.tx_hash,
            )

    # Process each scaled token event in the operation
    # Sort by log index to ensure events are processed in chronological order
    sorted_scaled_events = sorted(
        operation.scaled_token_events,
        key=lambda e: e.event["logIndex"],
    )
    for scaled_event in sorted_scaled_events:
        event = scaled_event.event

        # Find match within operation context
        match_result = matcher.find_match()

        # Handle MINT_TO_TREASURY operations specially - they don't have pool events
        # so we process them directly without matching
        if match_result is None and operation.operation_type == OperationType.MINT_TO_TREASURY:
            if scaled_event.event_type == "collateral_mint":
                _process_collateral_mint_with_match(
                    event=event,
                    tx_context=tx_context,
                    operation=operation,
                    scaled_event=scaled_event,
                    match_result={
                        "pool_event": None,
                        "extraction_data": {},
                        "should_consume": False,
                    },
                )
            continue

        # Handle IMPLICIT_BORROW operations - DEBT_MINT without BORROW event
        # These occur in flash loans and other internal Pool operations
        if match_result is None and operation.operation_type == OperationType.IMPLICIT_BORROW:
            if scaled_event.event_type in {"debt_mint", "gho_debt_mint"}:
                _process_debt_mint_with_match(
                    event=event,
                    tx_context=tx_context,
                    operation=operation,
                    scaled_event=scaled_event,
                    match_result={
                        "pool_event": None,
                        "extraction_data": {},
                        "should_consume": False,
                    },
                )
            continue

        # Handle STKAAVE_TRANSFER operations - these are standalone ERC20 transfers
        # that don't require matching with pool events
        if match_result is None and operation.operation_type == OperationType.STKAAVE_TRANSFER:
            # stkAAVE transfers are processed separately to update user balances
            # They don't affect Aave market positions directly
            continue

        if match_result is None:
            msg = f"No match for {scaled_event.event_type} in operation {operation.operation_id}"
            raise ValueError(msg)

        # Route to appropriate handler based on event type
        if scaled_event.event_type == "collateral_mint":
            _process_collateral_mint_with_match(
                event=event,
                tx_context=tx_context,
                operation=operation,
                scaled_event=scaled_event,
                match_result=match_result,
            )
        elif scaled_event.event_type == "collateral_burn":
            _process_collateral_burn_with_match(
                event=event,
                tx_context=tx_context,
                operation=operation,
                scaled_event=scaled_event,
                match_result=match_result,
            )
        elif scaled_event.event_type in {"debt_mint", "gho_debt_mint"}:
            _process_debt_mint_with_match(
                event=event,
                tx_context=tx_context,
                operation=operation,
                scaled_event=scaled_event,
                match_result=match_result,
            )
        elif scaled_event.event_type in {"debt_burn", "gho_debt_burn"}:
            _process_debt_burn_with_match(
                event=event,
                tx_context=tx_context,
                operation=operation,
                scaled_event=scaled_event,
                match_result=match_result,
            )
        elif scaled_event.event_type == "collateral_transfer":
            _process_collateral_transfer(
                event=event,
                tx_context=tx_context,
                operation=operation,
                scaled_event=scaled_event,
            )
        elif scaled_event.event_type in {"debt_transfer", "gho_debt_transfer"}:
            _process_debt_transfer(
                event=event,
                tx_context=tx_context,
                operation=operation,
                scaled_event=scaled_event,
            )
        elif scaled_event.event_type == "discount_transfer":
            # stkAAVE transfers are processed separately to update user balances
            # before GHO debt operations calculate discount rates. They don't
            # affect Aave market positions directly.
            pass
        else:
            msg = f"Unknown event type: {scaled_event.event_type}"
            raise ValueError(msg)


def _process_collateral_mint_with_match(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
    operation: Operation,
    scaled_event: ScaledTokenEvent,
    match_result: EventMatchResult,
) -> None:
    """Process collateral (aToken) mint with operation match."""
    logger.debug(f"Processing _process_collateral_mint_with_match at block {event['blockNumber']}")

    user = _get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    token_address = get_checksum_address(scaled_event.event["address"])
    collateral_asset, _ = _get_scaled_token_asset_by_address(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
    )

    if collateral_asset is None:
        return  # Skip unknown assets

    # Get or create collateral position
    collateral_position = _get_or_create_collateral_position(
        session=tx_context.session, user=user, asset_id=collateral_asset.id, tx_context=tx_context
    )

    # Calculate scaled amount using PoolProcessor for revision 4+
    # The scaled_event.amount is the raw underlying amount
    scaled_amount: int | None = None
    extraction_data = match_result.get("extraction_data", {})
    raw_amount = extraction_data.get("raw_amount")

    # For liquidations, use the liquidated_collateral amount
    if raw_amount is None:
        raw_amount = extraction_data.get("liquidated_collateral")

    if raw_amount is not None and collateral_asset.a_token_revision >= 4:  # noqa:PLR2004
        # Use token revision for math calculations to match contract behavior
        token_math = TokenMathFactory.get_token_math_for_token_revision(
            collateral_asset.a_token_revision
        )
        assert scaled_event.index is not None
        scaled_amount = token_math.get_collateral_mint_scaled_amount(
            amount=raw_amount,
            liquidity_index=scaled_event.index,
        )
    elif operation.operation_type == OperationType.MINT_TO_TREASURY:
        # For MINT_TO_TREASURY, the Mint event amount is the actual minted amount
        # (post-interest), while the MintedToTreasury Pool event shows pre-interest.
        # The Transfer event from address(0) is skipped, so we must calculate
        # the scaled amount from the Mint event data here.
        # Formula: scaled_amount = (value - balance_increase) / index # noqa: ERA001
        # This gives the principal amount converted to scaled balance.
        collateral_processor = TokenProcessorFactory.get_collateral_processor(
            collateral_asset.a_token_revision
        )
        wad_ray_math = collateral_processor.get_math_libraries()["wad_ray"]
        assert scaled_event.balance_increase is not None
        principal_amount = scaled_event.amount - scaled_event.balance_increase
        assert scaled_event.index is not None
        scaled_amount = wad_ray_math.ray_div(principal_amount, scaled_event.index)

    assert scaled_event.balance_increase is not None
    assert scaled_event.index is not None
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
    if scaled_event.index > 0:
        collateral_position.last_index = scaled_event.index


def _process_collateral_burn_with_match(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
    operation: Operation,
    scaled_event: ScaledTokenEvent,
    match_result: EventMatchResult,
) -> None:
    """Process collateral (aToken) burn with operation match."""
    logger.debug(f"Processing _process_collateral_burn_with_match at block {event['blockNumber']}")

    # Skip if user address is missing
    if scaled_event.user_address is None:
        return

    # Get user
    user = _get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get collateral asset
    token_address = get_checksum_address(scaled_event.event["address"])
    collateral_asset, _ = _get_scaled_token_asset_by_address(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
    )

    if collateral_asset is None:
        return  # Skip unknown assets

    # Get collateral position
    collateral_position = _get_or_create_collateral_position(
        session=tx_context.session, user=user, asset_id=collateral_asset.id, tx_context=tx_context
    )

    # Calculate scaled amount using PoolProcessor for revision 4+
    # The scaled_event.amount is the raw underlying amount
    scaled_amount: int | None = None
    extraction_data = match_result.get("extraction_data", {})
    raw_amount = extraction_data.get("raw_amount")

    # For liquidations, use the liquidated_collateral amount
    if raw_amount is None:
        raw_amount = extraction_data.get("liquidated_collateral")

    # Check if this burn follows a BalanceTransfer to the same user in the same transaction
    # If so, use the BalanceTransfer amount to ensure they cancel out exactly
    # ref: Bug #0026 - BalanceTransfer followed by Withdraw must use matching amounts
    if tx_context is not None and scaled_event.user_address is not None:
        # First, check if we have a tracked BalanceTransfer for this user/token
        # This is set when a transfer to this user was skipped (contract receives and burns)
        tracked_key = (token_address, scaled_event.user_address)
        if tracked_key in tx_context.processed_balance_transfers:
            tracked_log_index, tracked_amount = tx_context.processed_balance_transfers[tracked_key]
            # Only use if it happened before this burn
            if tracked_log_index < scaled_event.event["logIndex"]:
                scaled_amount = tracked_amount
                logger.debug(
                    f"Using tracked BalanceTransfer amount {tracked_amount} for burn at "
                    f"log {scaled_event.event['logIndex']} "
                    f"(transfer was at log {tracked_log_index})"
                )

        # If no tracked transfer found and this is a WITHDRAW operation,
        # search through events for a paired BalanceTransfer.
        # Only apply BalanceTransfer matching for WITHDRAW operations where the
        # BalanceTransfer is part of the same atomic operation.
        if (
            scaled_amount is None
            and operation is not None
            and operation.operation_type == OperationType.WITHDRAW
        ):
            """
            Event definition:
                event BalanceTransfer(
                    address indexed from,
                    address indexed to,
                    uint256 value,
                    uint256 index
                );
            """

            # Search all transaction events for a BalanceTransfer to this user
            # that happened before this burn, and find the closest one to the burn
            burn_log_index = scaled_event.event["logIndex"]
            closest_bt_amount = None
            closest_bt_log_index = -1

            for evt in tx_context.events:
                if evt["logIndex"] >= burn_log_index:
                    # Skip events at or after the burn
                    continue
                if evt["topics"][0] != AaveV3ScaledTokenEvent.BALANCE_TRANSFER.value:
                    continue
                if get_checksum_address(evt["address"]) != token_address:
                    continue
                # Check if this BalanceTransfer was TO this user
                to_addr = get_checksum_address("0x" + evt["topics"][2].hex()[-40:])
                # Track the closest BalanceTransfer to the burn
                if to_addr == scaled_event.user_address and evt["logIndex"] > closest_bt_log_index:
                    closest_bt_log_index = evt["logIndex"]
                    (closest_bt_amount, _) = eth_abi.abi.decode(
                        types=["uint256", "uint256"],
                        data=evt["data"],
                    )

            # Use the closest BalanceTransfer if found
            if closest_bt_amount is not None:
                scaled_amount = closest_bt_amount
                logger.debug(
                    f"Using preceding BalanceTransfer amount {closest_bt_amount} for burn at "
                    f"log {burn_log_index} to match transfer at log {closest_bt_log_index}"
                )

    # If this is a WITHDRAW operation and no BalanceTransfer amount was found,
    # use the raw_amount from the Withdraw event to ensure accurate balance updates.
    # The Burn event value may differ by 1 wei due to rounding, but the Withdraw amount
    # is the authoritative value that should be used for balance updates.
    # ref: Bug #0026 and #0027 - Withdraw must use Withdraw event amount only when
    # no BalanceTransfer is present. If a BalanceTransfer was found, use its amount
    # to ensure exact cancellation.
    if (
        operation is not None
        and operation.operation_type == OperationType.WITHDRAW
        and scaled_amount is None
        and raw_amount is not None
    ):
        # Calculate scaled amount from Withdraw event's raw_amount
        if collateral_asset.a_token_revision >= 4:  # noqa:PLR2004
            # Use token revision for math calculations to match contract behavior
            token_math = TokenMathFactory.get_token_math_for_token_revision(
                collateral_asset.a_token_revision
            )
            assert scaled_event.index is not None
            scaled_amount = token_math.get_collateral_burn_scaled_amount(
                amount=raw_amount,
                liquidity_index=scaled_event.index,
            )
        else:
            # For revisions 1-3, use standard ray_div
            scaled_amount = raw_amount * 10**27 // (scaled_event.index or 1)
        logger.debug(
            f"Using Withdraw raw_amount {raw_amount} to calculate scaled_amount {scaled_amount}"
        )

    if scaled_amount is None and raw_amount is not None and collateral_asset.a_token_revision >= 4:  # noqa:PLR2004
        # Use token revision for math calculations to match contract behavior
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

    # Update last_index
    collateral_position.last_index = scaled_event.index

    # Log liquidation match for debugging
    if (
        aave_debug_logger.is_enabled()
        and operation is not None
        and operation.operation_type
        in {
            OperationType.LIQUIDATION,
            OperationType.GHO_LIQUIDATION,
            OperationType.SELF_LIQUIDATION,
        }
    ):
        extraction_data = match_result.get("extraction_data", {})
        aave_debug_logger.log_liquidation_match(
            operation_id=operation.operation_id,
            user_address=scaled_event.user_address or "unknown",
            scaled_event_type=scaled_event.event_type,
            token_address=token_address,
            matched_amount=extraction_data.get("liquidated_collateral", 0),
            extraction_data=extraction_data,
            block_number=scaled_event.event["blockNumber"],
            tx_hash=tx_context.tx_hash,
        )


def _process_debt_mint_with_match(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
    operation: Operation,
    scaled_event: ScaledTokenEvent,
    match_result: EventMatchResult,
) -> None:
    """Process debt (vToken) mint with operation match."""
    logger.debug(f"Processing _process_debt_mint_with_match at block {event['blockNumber']}")

    user = _get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    token_address = get_checksum_address(scaled_event.event["address"])
    _, debt_asset = _get_scaled_token_asset_by_address(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
    )

    if debt_asset is None:
        return  # Skip unknown assets

    # Get or create debt position
    debt_position = _get_or_create_debt_position(
        session=tx_context.session,
        user=user,
        asset_id=debt_asset.id,
    )

    # Calculate scaled amount using PoolProcessor for revision 4+
    # The scaled_event.amount is the raw underlying amount
    scaled_amount: int | None = None
    extraction_data = match_result.get("extraction_data", {})
    raw_amount = extraction_data.get("raw_amount")

    # For liquidations, use the debt_to_cover amount ONLY for pre-v4 tokens
    # For v4+, let the processor calculate from the actual Mint event values
    # to avoid discrepancies between debt_to_cover and actual minted amount
    is_liquidation = operation is not None and operation.operation_type in {
        OperationType.LIQUIDATION,
        OperationType.GHO_LIQUIDATION,
        OperationType.SELF_LIQUIDATION,
    }

    if raw_amount is None and not is_liquidation:
        raw_amount = extraction_data.get("debt_to_cover")

    if raw_amount is not None:
        # Use token revision for math calculations to match contract behavior
        token_math = TokenMathFactory.get_token_math_for_token_revision(debt_asset.v_token_revision)
        assert scaled_event.index is not None
        scaled_amount = token_math.get_debt_mint_scaled_amount(
            amount=raw_amount,
            borrow_index=scaled_event.index,
        )

    # Check if this is a GHO token and use GHO-specific processing
    if token_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS:
        # Use the effective discount from transaction context or user record
        effective_discount = (
            tx_context.user_discounts.get(user.address, user.gho_discount)
            if tx_context is not None
            else user.gho_discount
        )

        # Process using GHO-specific processor
        gho_processor = TokenProcessorFactory.get_gho_debt_processor(debt_asset.v_token_revision)
        assert scaled_event.balance_increase is not None
        assert scaled_event.index is not None
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
        )

        # Apply the calculated balance delta
        debt_position.balance += gho_result.balance_delta
        # Always fetch the current global index from the contract.
        # The asset's cached borrow_index may be stale (from a previous block).
        # The event's index is the user's cached lastIndex, not the current global index.
        pool_contract = _get_contract(
            session=tx_context.session,
            market=tx_context.market,
            contract_name="POOL",
        )
        fetched_index = _get_current_borrow_index_from_pool(
            w3=tx_context.w3,
            pool_address=get_checksum_address(pool_contract.address),
            underlying_asset_address=get_checksum_address(debt_asset.underlying_token.address),
            block_number=scaled_event.event["blockNumber"],
        )
        # Use fetched index if available, otherwise fall back to event index
        current_index = fetched_index if fetched_index is not None else scaled_event.index
        debt_position.last_index = current_index

        # Refresh discount if needed
        if (
            gho_result.should_refresh_discount
            and tx_context.gho_asset.v_gho_discount_token is not None
        ):
            discount_token_balance = _get_or_init_stk_aave_balance(
                user=user,
                discount_token=tx_context.gho_asset.v_gho_discount_token,
                block_number=scaled_event.event["blockNumber"],
                w3=tx_context.w3,
                tx_context=tx_context,
                log_index=scaled_event.event["logIndex"],
            )
            assert current_index is not None
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

        # Always fetch the current global index from the contract.
        # The asset's cached borrow_index may be stale (from a previous block).
        # The event's index is the user's cached lastIndex, not the current global index.
        pool_contract = _get_contract(
            session=tx_context.session,
            market=tx_context.market,
            contract_name="POOL",
        )
        fetched_index = _get_current_borrow_index_from_pool(
            w3=tx_context.w3,
            pool_address=get_checksum_address(pool_contract.address),
            underlying_asset_address=get_checksum_address(debt_asset.underlying_token.address),
            block_number=scaled_event.event["blockNumber"],
        )
        # Use fetched index if available, otherwise fall back to event index
        current_index = fetched_index if fetched_index is not None else scaled_event.index
        debt_position.last_index = current_index


def _process_debt_burn_with_match(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
    operation: Operation,
    scaled_event: ScaledTokenEvent,
    match_result: EventMatchResult,
) -> None:
    """Process debt (vToken) burn with operation match."""
    logger.debug(f"Processing _process_debt_burn_with_match at block {event['blockNumber']}")

    user = _get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.user_address,
        block_number=scaled_event.event["blockNumber"],
    )

    token_address = get_checksum_address(scaled_event.event["address"])
    _, debt_asset = _get_scaled_token_asset_by_address(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
    )

    assert debt_asset is not None

    # Get debt position
    debt_position = _get_or_create_debt_position(
        session=tx_context.session,
        user=user,
        asset_id=debt_asset.id,
    )

    # Calculate scaled amount using PoolProcessor for revision 4+
    # The scaled_event.amount is the raw underlying amount
    scaled_amount: int | None = None
    extraction_data = match_result.get("extraction_data", {})
    raw_amount = extraction_data.get("raw_amount")

    # For liquidations, use the debt_to_cover amount ONLY for pre-v4 tokens
    # For v4+, let the processor calculate from the actual Burn event values
    # to avoid discrepancies between debt_to_cover and actual burned amount
    is_liquidation = operation is not None and operation.operation_type in {
        OperationType.LIQUIDATION,
        OperationType.GHO_LIQUIDATION,
        OperationType.SELF_LIQUIDATION,
    }

    if raw_amount is None and not is_liquidation:
        raw_amount = extraction_data.get("debt_to_cover")

    logger.debug(f"_process_debt_burn_with_match: vToken revision = {debt_asset.v_token_revision}")
    logger.debug(f"_process_debt_burn_with_match: raw_amount = {raw_amount}")

    if raw_amount is not None and debt_asset.v_token_revision >= 4:  # noqa:PLR2004
        assert scaled_event.index is not None
        # Use token revision for math calculations to match contract behavior
        token_math = TokenMathFactory.get_token_math_for_token_revision(debt_asset.v_token_revision)
        scaled_amount = token_math.get_debt_burn_scaled_amount(
            amount=raw_amount,
            borrow_index=scaled_event.index,
        )

    # Check if this is a GHO token and use GHO-specific processing
    if token_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS:
        # Use the effective discount from transaction context or user record
        effective_discount = (
            tx_context.user_discounts.get(user.address, user.gho_discount)
            if tx_context is not None
            else user.gho_discount
        )

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
        # Always fetch the current global index from the contract.
        # The asset's cached borrow_index may be stale (from a previous block).
        # The event's index is the user's cached lastIndex, not the current global index.
        pool_contract = _get_contract(
            session=tx_context.session,
            market=tx_context.market,
            contract_name="POOL",
        )
        fetched_index = _get_current_borrow_index_from_pool(
            w3=tx_context.w3,
            pool_address=get_checksum_address(pool_contract.address),
            underlying_asset_address=get_checksum_address(debt_asset.underlying_token.address),
            block_number=scaled_event.event["blockNumber"],
        )
        # Use fetched index if available, otherwise fall back to event index
        current_index = fetched_index if fetched_index is not None else scaled_event.index
        debt_position.last_index = current_index

        # Refresh discount if needed
        if (
            gho_result.should_refresh_discount
            and tx_context.gho_asset.v_gho_discount_token is not None
        ):
            discount_token_balance = _get_or_init_stk_aave_balance(
                user=user,
                discount_token=tx_context.gho_asset.v_gho_discount_token,
                block_number=scaled_event.event["blockNumber"],
                w3=tx_context.w3,
                tx_context=tx_context,
                log_index=scaled_event.event["logIndex"],
            )
            assert current_index is not None
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
        _process_scaled_token_operation(
            event=DebtBurnEvent(
                from_=scaled_event.from_address or scaled_event.user_address,
                target=scaled_event.target_address or scaled_event.user_address,
                value=scaled_event.amount,
                balance_increase=scaled_event.balance_increase,
                index=scaled_event.index,
                scaled_amount=scaled_amount,
            ),
            scaled_token_revision=debt_asset.v_token_revision,
            position=debt_position,
        )

        # Always fetch the current global index from the contract.
        # The asset's cached borrow_index may be stale (from a previous block).
        # The event's index is the user's cached lastIndex, not the current global index.
        pool_contract = _get_contract(
            session=tx_context.session,
            market=tx_context.market,
            contract_name="POOL",
        )
        fetched_index = _get_current_borrow_index_from_pool(
            w3=tx_context.w3,
            pool_address=get_checksum_address(pool_contract.address),
            underlying_asset_address=get_checksum_address(debt_asset.underlying_token.address),
            block_number=scaled_event.event["blockNumber"],
        )
        # Use fetched index if available, otherwise fall back to event index
        current_index = fetched_index if fetched_index is not None else scaled_event.index
        debt_position.last_index = current_index

    # Log liquidation match for debugging
    if (
        aave_debug_logger.is_enabled()
        and operation is not None
        and operation.operation_type
        in {
            OperationType.LIQUIDATION,
            OperationType.GHO_LIQUIDATION,
            OperationType.SELF_LIQUIDATION,
        }
    ):
        extraction_data = match_result.get("extraction_data", {})
        aave_debug_logger.log_liquidation_match(
            operation_id=operation.operation_id,
            user_address=scaled_event.user_address or "unknown",
            scaled_event_type=scaled_event.event_type,
            token_address=token_address,
            matched_amount=extraction_data.get("debt_to_cover", 0),
            extraction_data=extraction_data,
            block_number=scaled_event.event["blockNumber"],
            tx_hash=tx_context.tx_hash,
        )


def _process_collateral_transfer(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
    operation: Operation,
    scaled_event: ScaledTokenEvent,
) -> None:
    """Process collateral (aToken) transfer between users."""
    logger.debug(
        f"Processing _process_collateral_transfer_with_match at block {event['blockNumber']}"
    )
    logger.debug(
        f"Processing _process_collateral_transfer_with_match at block {event['blockNumber']}"
    )
    logger.debug(
        f"  Event: logIndex={scaled_event.event['logIndex']}, type={scaled_event.event_type}, "
        f"index={scaled_event.index}, from={scaled_event.from_address}, "
        f"to={scaled_event.target_address}"
    )

    # Skip if addresses are missing
    if scaled_event.from_address is None or scaled_event.target_address is None:
        return

    # Skip BalanceTransfer events that are tracked in balance_transfer_events
    # These will be handled by their paired ERC20 Transfer events
    # ref: Issue #0030 - Standalone BalanceTransfer events should NOT be skipped
    if (
        scaled_event.index is not None
        and scaled_event.index > 0
        and operation
        and operation.balance_transfer_events
    ):
        for bt_event in operation.balance_transfer_events:
            if bt_event["logIndex"] == scaled_event.event["logIndex"]:
                # This BalanceTransfer is paired with an ERC20 Transfer
                # Skip it - the ERC20 Transfer will handle the balance change
                logger.debug(
                    f"Skipping paired BalanceTransfer at log {scaled_event.event['logIndex']}"
                )
                return

    # Log standalone BalanceTransfer processing
    if scaled_event.index is not None and scaled_event.index > 0:
        logger.debug(
            f"Processing standalone BalanceTransfer at log {scaled_event.event['logIndex']} "
            f"from {scaled_event.from_address} to {scaled_event.target_address} "
            f"amount {scaled_event.amount}"
        )

    # Skip transfers that are part of REPAY_WITH_ATOKENS operations
    # These transfers represent the internal movement of aTokens before burning,
    # and the collateral burn event will handle the actual balance reduction
    if operation and operation.operation_type == OperationType.REPAY_WITH_ATOKENS:
        return

    # Skip transfers from zero address (mint events)
    # These are protocol reserve mints via mintToTreasury()
    if scaled_event.from_address == ZERO_ADDRESS:
        return

    # Skip ERC20 transfers that correspond to direct collateral burns.
    # Only skip when the transfer target is the zero address (direct burn).
    # Transfers to adapters, pools, or other intermediate contracts should be
    # processed normally as they represent actual balance movements.
    if scaled_event.index is None and scaled_event.target_address == ZERO_ADDRESS and tx_context:
        # Check if there's a corresponding SCALED_TOKEN_BURN for this direct burn
        for evt in tx_context.events:
            if evt["topics"][0] != AaveV3ScaledTokenEvent.BURN.value:
                continue
            # Skip GHO debt burns (collateral burns are all other burns)
            if get_checksum_address(evt["address"]) == GHO_VARIABLE_DEBT_TOKEN_ADDRESS:
                continue
            if get_checksum_address(evt["address"]) == get_checksum_address(
                scaled_event.event["address"]
            ):
                # The burn user is in topics[1] of the SCALED_TOKEN_BURN event
                burn_user = get_checksum_address("0x" + evt["topics"][1].hex()[-40:])
                if burn_user == scaled_event.from_address:
                    # Check if the amounts match
                    burn_amount = int.from_bytes(evt["data"][:32], "big")
                    if burn_amount == scaled_event.amount:
                        # Skip this direct burn as the SCALED_TOKEN_BURN will handle it
                        return

    sender = _get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.from_address,
        block_number=scaled_event.event["blockNumber"],
    )

    token_address = get_checksum_address(scaled_event.event["address"])
    collateral_asset, _ = _get_scaled_token_asset_by_address(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
    )

    assert collateral_asset is not None

    sender_position = _get_or_create_collateral_position(
        session=tx_context.session,
        user=sender,
        asset_id=collateral_asset.id,
        tx_context=tx_context,
    )

    # Determine transfer amount
    transfer_amount = scaled_event.amount
    transfer_index = scaled_event.index

    # Check if we have a paired BalanceTransfer event
    # For liquidation transfers, the BalanceTransfer amount is the actual scaled balance
    # while the ERC20 Transfer amount includes accrued interest
    matched_balance_transfer = False
    if operation and operation.balance_transfer_events:
        for bt_event in operation.balance_transfer_events:
            # Check if this BalanceTransfer is for the same token and from the same user
            # and is close to the ERC20 Transfer event in log index (within 3 logs)
            bt_from = get_checksum_address("0x" + bt_event["topics"][1].hex()[-40:])
            bt_to = get_checksum_address("0x" + bt_event["topics"][2].hex()[-40:])
            bt_token = get_checksum_address(bt_event["address"])
            bt_log_index = bt_event["logIndex"]
            transfer_log_index = scaled_event.event["logIndex"]

            # Match by token, from address, and log index proximity
            if (
                bt_token == token_address
                and bt_from == scaled_event.from_address
                and bt_to == scaled_event.target_address
                and abs(bt_log_index - transfer_log_index) <= 3  # noqa:PLR2004
            ):
                # Found matching BalanceTransfer - use its amount (scaled balance without interest)
                bt_amount, bt_index = eth_abi.abi.decode(
                    types=["uint256", "uint256"],
                    data=bt_event["data"],
                )
                transfer_amount = bt_amount
                transfer_index = bt_index
                matched_balance_transfer = True
                logger.debug(
                    f"Using BalanceTransfer amount {bt_amount} for transfer from {bt_from} "
                    f"at log {bt_log_index}"
                )
                break

    if not matched_balance_transfer:
        logger.debug(
            f"No BalanceTransfer match for transfer from {scaled_event.from_address} "
            f"amount {scaled_event.amount} at log {scaled_event.event['logIndex']}"
        )
        # Only use scaled_event values if no BalanceTransfer match found
        if scaled_event.index is not None and scaled_event.index > 0:
            # Standalone BalanceTransfer - amount is already the scaled balance
            transfer_amount = scaled_event.amount
            transfer_index = scaled_event.index
        else:
            # Standalone ERC20 Transfer - amount is the aToken amount (includes interest)
            # For transfers to treasury, this over-counts by the accrued interest
            transfer_amount = scaled_event.amount
            transfer_index = collateral_asset.liquidity_index

    # Update sender's balance
    sender_position.balance -= transfer_amount

    assert transfer_amount is not None
    assert transfer_index is not None

    # Only update last_index if the new index is greater than the current one
    # This prevents a transfer with a paired BalanceTransfer (which has the index
    # from when the transfer occurred) from overwriting a higher index set by
    # a subsequent operation (like a burn) that occurred later in the transaction
    current_sender_index = sender_position.last_index or 0
    if transfer_index > 0 and transfer_index > current_sender_index:
        sender_position.last_index = transfer_index

    # Handle recipient
    if scaled_event.target_address != ZERO_ADDRESS:  # noqa:PLR1702
        # Check if the recipient immediately burns the tokens (without a WITHDRAW operation)
        # Only apply this skip logic to ERC20 Transfers (index is None or 0), NOT to
        # BalanceTransfer events (index > 0). BalanceTransfer events represent the actual
        # movement of scaled balances and must always be processed.
        # Also, don't skip if this ERC20 Transfer has a paired BalanceTransfer event,
        # as the BalanceTransfer represents the actual balance movement.
        # ref: Issue #0026 - Don't skip if the burn is part of a WITHDRAW operation
        # ref: Issue #0030 - BalanceTransfer events must always update recipient balance
        skip_recipient_update = False
        has_paired_balance_transfer = (
            operation is not None
            and operation.balance_transfer_events
            and any(
                bt_event["logIndex"] > scaled_event.event["logIndex"]
                for bt_event in operation.balance_transfer_events
            )
        )
        if (
            tx_context is not None
            and (scaled_event.index is None or scaled_event.index == 0)
            and not has_paired_balance_transfer
        ):
            transfer_log_index = scaled_event.event["logIndex"]
            # Check if there's a WITHDRAW pool event anywhere in the transaction
            # If so, this is part of a withdrawal and we should NOT skip the recipient update
            has_withdraw_in_tx = any(
                evt["topics"][0] == AaveV3PoolEvent.WITHDRAW.value for evt in tx_context.events
            )
            if not has_withdraw_in_tx:
                # No WITHDRAW in transaction - check if recipient burns immediately
                for evt in tx_context.events:
                    # Only check events after the transfer
                    if evt["logIndex"] <= transfer_log_index:
                        continue
                    # Check for SCALED_TOKEN_BURN from the same recipient
                    if evt["topics"][0] == AaveV3ScaledTokenEvent.BURN.value:
                        burn_user = get_checksum_address("0x" + evt["topics"][1].hex()[-40:])
                        if burn_user == scaled_event.target_address:
                            skip_recipient_update = True
                            logger.debug(
                                f"Skipping recipient balance update for "
                                f"{scaled_event.target_address} "
                                f"because they immediately burn the received tokens"
                            )
                            break

        if not skip_recipient_update:
            recipient = _get_or_create_user(
                tx_context=tx_context,
                user_address=scaled_event.target_address,
                block_number=scaled_event.event["blockNumber"],
            )

            recipient_position = _get_or_create_collateral_position(
                session=tx_context.session,
                user=recipient,
                asset_id=collateral_asset.id,
                tx_context=tx_context,
            )
            recipient_position.balance += transfer_amount

            if transfer_index > 0:
                recipient_position.last_index = transfer_index

        # Track this BalanceTransfer for potential matching with subsequent burns
        # This allows exact cancellation when the recipient burns the transferred tokens
        # ref: Bug #0026
        # Only track if we have an actual BalanceTransfer (matched_balance_transfer=True)
        # or if this is a standalone BalanceTransfer event (index > 0)
        # IMPORTANT: Only track if we actually updated the recipient's balance.
        # If the recipient update was skipped (e.g., because they immediately burn),
        # tracking the BalanceTransfer would cause the burn to use the wrong amount.
        # ref: Bug #0029
        if tx_context is not None and not skip_recipient_update:
            should_track = False
            track_log_index = None
            track_amount = None

            if operation and operation.balance_transfer_events and matched_balance_transfer:
                # Use the BalanceTransfer event's data for tracking
                bt_event = operation.balance_transfer_events[0]
                track_amount, _ = eth_abi.abi.decode(
                    types=["uint256", "uint256"],
                    data=bt_event["data"],
                )
                track_log_index = bt_event["logIndex"]
                should_track = True
                logger.debug(
                    f"Tracking BalanceTransfer via operation: token={token_address}, "
                    f"recipient={scaled_event.target_address}, amount={track_amount}, "
                    f"log={track_log_index}"
                )
            elif scaled_event.index is not None and scaled_event.index > 0:
                # Standalone BalanceTransfer (no paired ERC20 Transfer)
                track_amount = transfer_amount
                track_log_index = scaled_event.event["logIndex"]
                should_track = True
                logger.debug(
                    f"Tracking standalone BalanceTransfer: token={token_address}, "
                    f"recipient={scaled_event.target_address}, amount={track_amount}, "
                    f"log={track_log_index}"
                )

            if should_track and track_log_index is not None and track_amount is not None:
                tx_context.processed_balance_transfers[
                    token_address,
                    scaled_event.target_address,
                ] = (
                    track_log_index,
                    track_amount,
                )
                # Standalone BalanceTransfer (no paired ERC20 Transfer)
                tx_context.processed_balance_transfers[
                    token_address,
                    scaled_event.target_address,
                ] = (
                    scaled_event.event["logIndex"],
                    transfer_amount,
                )


def _process_debt_transfer(
    *,
    event: LogReceipt,
    tx_context: TransactionContext,
    operation: Operation,
    scaled_event: ScaledTokenEvent,
) -> None:
    """Process debt (vToken) transfer between users."""
    logger.debug(f"Processing _process_debt_transfer_with_match at block {event['blockNumber']}")

    # Skip if addresses are missing
    if scaled_event.from_address is None or scaled_event.target_address is None:
        return

    # Skip transfers to zero address (burns) - these are handled by Burn events
    # Processing both Transfer(to=0) and Burn would result in double-counting
    if scaled_event.target_address == ZERO_ADDRESS:
        return

    # Skip transfers from zero address (mints) - these are handled by Mint events
    # Processing both Transfer(from=0) and Mint would result in double-counting
    # This occurs during _burnScaled when interest > repayment amount
    if scaled_event.from_address == ZERO_ADDRESS:
        return

    # Skip if addresses are missing
    if scaled_event.from_address is None or scaled_event.target_address is None:
        return

    # Get sender
    sender = _get_or_create_user(
        tx_context=tx_context,
        user_address=scaled_event.from_address,
        block_number=scaled_event.event["blockNumber"],
    )

    # Get debt asset
    token_address = get_checksum_address(scaled_event.event["address"])
    _, debt_asset = _get_scaled_token_asset_by_address(
        session=tx_context.session,
        market=tx_context.market,
        token_address=token_address,
    )

    if debt_asset is None:
        return

    # Get sender's position
    sender_position = _get_or_create_debt_position(
        session=tx_context.session,
        user=sender,
        asset_id=debt_asset.id,
    )

    # Determine transfer amount
    transfer_amount = scaled_event.amount
    transfer_index = scaled_event.index

    assert transfer_index is not None

    # Check if we have a paired BalanceTransfer event
    if operation and operation.balance_transfer_events:
        # Decode the BalanceTransfer event to get scaled amount and index
        bt_event = operation.balance_transfer_events[0]
        transfer_amount, transfer_index = eth_abi.abi.decode(
            types=["uint256", "uint256"],
            data=bt_event["data"],
        )
    else:
        # Standalone BalanceTransfer - scale the amount
        transfer_amount = transfer_amount * transfer_index // 10**27

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
            session=tx_context.session,
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
        token_type=TokenType.COLLATERAL,
    )

    debt_asset = _get_asset_by_token_type(
        session=session,
        market=market,
        token_address=token_address,
        token_type=TokenType.DEBT,
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


def _get_debt_token_addresses(
    session: Session,
    chain_id: int,
) -> list[ChecksumAddress]:
    """
    Get all vToken (debt token) addresses for a given chain.
    """
    return list(
        session.scalars(
            select(Erc20TokenTable.address)
            .join(
                AaveV3Asset,
                AaveV3Asset.v_token_id == Erc20TokenTable.id,
            )
            .where(Erc20TokenTable.chain == chain_id)
        ).all()
    )


def _update_contract_revision(
    *,
    session: Session,
    w3: Web3,
    market: AaveV3Market,
    contract_name: str,
    new_address: ChecksumAddress,
    revision_function_prototype: str,
) -> None:
    """
    Update contract revision in database.
    """
    (revision,) = raw_call(
        w3=w3,
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
    contract.revision = revision

    logger.info(f"Upgraded revision for {contract.name} to {revision}")


def _process_proxy_creation_event(
    *,
    w3: Web3,
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
        w3=w3,
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


def _process_umbrella_creation_event(
    *,
    session: Session,
    market: AaveV3Market,
    event: LogReceipt,
    proxy_name: str,
    proxy_id: bytes,
) -> None:
    """
    Process an AddressSet event for UMBRELLA contract creation.

    The AddressSet event structure:
    - topics[1]: id (bytes32) - e.g., "UMBRELLA"
    - topics[2]: oldAddress (address) - typically 0x0 for new addresses
    - topics[3]: newAddress (address) - the actual contract address
    """
    logger.debug(f"Processing _process_umbrella_creation_event at block {event['blockNumber']}")

    (decoded_proxy_id,) = eth_abi.abi.decode(types=["bytes32"], data=event["topics"][1])

    if decoded_proxy_id != proxy_id:
        return

    new_address = decode_address(event["topics"][3])

    if (
        session.scalar(select(AaveV3Contract).where(AaveV3Contract.address == new_address))
        is not None
    ):
        return

    market.contracts.append(
        AaveV3Contract(
            market_id=market.id,
            name=proxy_name,
            address=new_address,
            revision=None,
        )
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
    tx_context.session.flush()  # Ensure the discount update is visible to subsequent queries


def _event_sort_key(event: LogReceipt) -> tuple[int, int]:
    """Sort key for chronological ordering by (blockNumber, logIndex)."""
    return (event["blockNumber"], event["logIndex"])


def _fetch_pool_events(
    w3: Web3,
    pool_address: ChecksumAddress,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """Fetch Pool contract events for assertions and config updates."""
    return fetch_logs_retrying(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=[pool_address],
        topic_signature=[
            [
                AaveV3PoolEvent.SUPPLY.value,
                AaveV3PoolEvent.WITHDRAW.value,
                AaveV3PoolEvent.BORROW.value,
                AaveV3PoolEvent.REPAY.value,
                AaveV3PoolEvent.LIQUIDATION_CALL.value,
                AaveV3PoolEvent.DEFICIT_CREATED.value,
                AaveV3PoolEvent.RESERVE_DATA_UPDATED.value,
                AaveV3PoolEvent.USER_E_MODE_SET.value,
            ]
        ],
    )


def _fetch_reserve_initialization_events(
    w3: Web3,
    configurator_address: ChecksumAddress,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """Fetch Pool Configurator events for reserve initialization."""

    return fetch_logs_retrying(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=[configurator_address],
        topic_signature=[[AaveV3PoolEvent.RESERVE_INITIALIZED.value]],
    )


def _fetch_scaled_token_events(
    w3: Web3,
    token_addresses: list[ChecksumAddress],
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """Fetch events from all scaled tokens (aTokens, vTokens)."""
    if not token_addresses:
        return []

    return fetch_logs_retrying(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=token_addresses,
        topic_signature=[
            [
                AaveV3ScaledTokenEvent.MINT.value,
                AaveV3ScaledTokenEvent.BURN.value,
                AaveV3ScaledTokenEvent.BALANCE_TRANSFER.value,
                AaveV3PoolConfigEvent.UPGRADED.value,
                AaveV3GhoDebtTokenEvent.DISCOUNT_PERCENT_UPDATED.value,
                # Include ERC20 Transfer events for proper paired transfer matching
                ERC20Event.TRANSFER.value,
            ]
        ],
    )


def _fetch_stk_aave_events(
    w3: Web3,
    discount_token: ChecksumAddress | None,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """Fetch stkAAVE events including STAKED and REDEEM for classification."""
    if not discount_token:
        return []
    return fetch_logs_retrying(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=[discount_token],
        topic_signature=[
            [
                AaveV3StkAaveEvent.STAKED.value,
                AaveV3StkAaveEvent.REDEEM.value,
                ERC20Event.TRANSFER.value,
            ]
        ],
    )


def _fetch_address_provider_events(
    w3: Web3,
    provider_address: ChecksumAddress,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch Pool Address Provider events for contract updates.
    """

    return fetch_logs_retrying(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=[provider_address],
        topic_signature=[
            [
                AaveV3PoolConfigEvent.PROXY_CREATED.value,
                AaveV3PoolConfigEvent.POOL_CONFIGURATOR_UPDATED.value,
                AaveV3PoolConfigEvent.POOL_DATA_PROVIDER_UPDATED.value,
                AaveV3PoolConfigEvent.POOL_UPDATED.value,
                AaveV3PoolConfigEvent.ADDRESS_SET.value,
            ]
        ],
    )


def _fetch_discount_config_events(
    w3: Web3,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """Fetch discount-related events from any contract (not address-specific)."""
    return fetch_logs_retrying(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        topic_signature=[
            [
                AaveV3GhoDebtTokenEvent.DISCOUNT_RATE_STRATEGY_UPDATED.value,
                AaveV3GhoDebtTokenEvent.DISCOUNT_TOKEN_UPDATED.value,
            ]
        ],
    )


def _build_transaction_contexts(
    *,
    events: list[LogReceipt],
    market: AaveV3Market,
    session: Session,
    w3: Web3,
    gho_asset: AaveGhoToken,
    known_scaled_token_addresses: set[ChecksumAddress],
    known_debt_token_addresses: set[ChecksumAddress],
    pool_address: ChecksumAddress,
) -> dict[HexBytes, TransactionContext]:
    """Group events by transaction with full categorization."""

    logger.debug(f"_build_transaction_contexts: starting categorization for {len(events)} events")
    logger.debug(
        f"_build_transaction_contexts: known_scaled_tokens={len(known_scaled_token_addresses)} "
        f"known_debt_tokens={len(known_debt_token_addresses)}"
    )

    contexts: dict[HexBytes, TransactionContext] = {}

    for event in sorted(events, key=_event_sort_key):
        tx_hash = event["transactionHash"]
        block_num = event["blockNumber"]
        topic = event["topics"][0]
        event_address = get_checksum_address(event["address"])

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
                w3=w3,
                tx_hash=tx_hash,
                block_number=block_num,
                events=[],
                market=market,
                session=session,
                gho_asset=gho_asset,
            )

        ctx = contexts[tx_hash]
        ctx.events.append(event)

        # Skip scaled token events from the Pool contract specifically
        # The Pool contract may emit Mint/Burn/Transfer-like events that have the
        # same topic signature as aToken/vToken events but are not scaled token events
        # All other addresses should be processed, even if not in known_scaled_token_addresses,
        # as they may be new tokens not yet in the database
        if (
            topic
            in {
                AaveV3ScaledTokenEvent.MINT.value,
                AaveV3ScaledTokenEvent.BURN.value,
                AaveV3ScaledTokenEvent.BALANCE_TRANSFER.value,
            }
            and event_address == pool_address
        ):
            logger.debug(
                f"_build_transaction_contexts: SKIPPING scaled token "
                f"event from Pool addr={event_address}"
            )
            continue

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

        # Log event categorization for debugging (lists removed in Phase 6)
        if topic in {
            AaveV3PoolEvent.SUPPLY.value,
            AaveV3PoolEvent.WITHDRAW.value,
            AaveV3PoolEvent.BORROW.value,
            AaveV3PoolEvent.REPAY.value,
            AaveV3PoolEvent.LIQUIDATION_CALL.value,
            AaveV3PoolEvent.DEFICIT_CREATED.value,
        }:
            logger.debug(
                f"_build_transaction_contexts: categorized as POOL_EVENT topic={topic.to_0x_hex()}"
            )
        elif topic == AaveV3StkAaveEvent.STAKED.value:
            logger.debug("_build_transaction_contexts: categorized as STAKED event")
        elif topic == AaveV3StkAaveEvent.REDEEM.value:
            logger.debug("_build_transaction_contexts: categorized as REDEEM event")
        elif topic == ERC20Event.TRANSFER.value and event_address == (
            gho_asset.v_gho_discount_token if gho_asset else None
        ):
            logger.debug("_build_transaction_contexts: categorized as stkAAVE TRANSFER event")
        elif topic == AaveV3ScaledTokenEvent.MINT.value:
            if event_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS:
                logger.debug("_build_transaction_contexts: categorized as GHO_MINT event")
            elif event_address in known_debt_token_addresses:
                logger.debug(
                    f"_build_transaction_contexts: categorized as "
                    f"DEBT_MINT event addr={event_address}"
                )
            else:
                logger.debug(
                    f"_build_transaction_contexts: categorized as "
                    f"COLLATERAL_MINT event addr={event_address}"
                )
        elif topic == AaveV3ScaledTokenEvent.BURN.value:
            if event_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS:
                logger.debug("_build_transaction_contexts: categorized as GHO_BURN event")
            elif event_address in known_debt_token_addresses:
                logger.debug(
                    f"_build_transaction_contexts: categorized as "
                    f"DEBT_BURN event addr={event_address}"
                )
            else:
                logger.debug(
                    f"_build_transaction_contexts: categorized as "
                    f"COLLATERAL_BURN event addr={event_address}"
                )
        elif topic == AaveV3ScaledTokenEvent.BALANCE_TRANSFER.value:
            logger.debug("_build_transaction_contexts: categorized as BALANCE_TRANSFER event")
        elif topic in {
            AaveV3GhoDebtTokenEvent.DISCOUNT_PERCENT_UPDATED.value,
            AaveV3GhoDebtTokenEvent.DISCOUNT_RATE_STRATEGY_UPDATED.value,
            AaveV3GhoDebtTokenEvent.DISCOUNT_TOKEN_UPDATED.value,
        }:
            logger.debug("_build_transaction_contexts: categorized as DISCOUNT_UPDATE event")
        elif topic == AaveV3PoolEvent.RESERVE_DATA_UPDATED.value:
            logger.debug("_build_transaction_contexts: categorized as RESERVE_DATA_UPDATE event")
        elif topic == AaveV3PoolEvent.USER_E_MODE_SET.value:
            logger.debug("_build_transaction_contexts: categorized as USER_E_MODE_SET event")
        elif topic == AaveV3PoolConfigEvent.UPGRADED.value:
            logger.debug("_build_transaction_contexts: categorized as UPGRADED event")
        elif topic == ERC20Event.TRANSFER.value:
            logger.debug("_build_transaction_contexts: categorized as ERC20_TRANSFER (ignored)")
        else:
            msg = f"Could not identify topic: {topic.to_0x_hex()}"
            logger.error(f"_build_transaction_contexts: {msg}")
            raise ValueError(msg)

    logger.debug(
        f"_build_transaction_contexts: completed with {len(contexts)} transaction contexts"
    )

    return contexts


def update_aave_market(
    *,
    w3: Web3,
    start_block: int,
    end_block: int,
    market: AaveV3Market,
    session: Session,
    verify: bool,
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

    # Phase 1
    for event in _fetch_address_provider_events(
        w3=w3,
        provider_address=_get_contract(
            session=session,
            market=market,
            contract_name="POOL_ADDRESS_PROVIDER",
        ).address,
        start_block=start_block,
        end_block=end_block,
    ):
        topic = event["topics"][0]

        if topic == AaveV3PoolConfigEvent.PROXY_CREATED.value:
            _process_proxy_creation_event(
                w3=w3,
                session=session,
                market=market,
                event=event,
                proxy_name="POOL",
                proxy_id=eth_abi.abi.encode(["bytes32"], [b"POOL"]),
                revision_function_prototype="POOL_REVISION",
            )
            _process_proxy_creation_event(
                w3=w3,
                session=session,
                market=market,
                event=event,
                proxy_name="POOL_CONFIGURATOR",
                proxy_id=eth_abi.abi.encode(["bytes32"], [b"POOL_CONFIGURATOR"]),
                revision_function_prototype="CONFIGURATOR_REVISION",
            )
        elif topic == AaveV3PoolConfigEvent.POOL_UPDATED.value:
            _update_contract_revision(
                session=session,
                w3=w3,
                market=market,
                contract_name="POOL",
                new_address=decode_address(event["topics"][2]),
                revision_function_prototype="POOL_REVISION",
            )
        elif topic == AaveV3PoolConfigEvent.POOL_CONFIGURATOR_UPDATED.value:
            _update_contract_revision(
                session=session,
                w3=w3,
                market=market,
                contract_name="POOL_CONFIGURATOR",
                new_address=decode_address(event["topics"][2]),
                revision_function_prototype="CONFIGURATOR_REVISION",
            )
        elif topic == AaveV3PoolConfigEvent.POOL_DATA_PROVIDER_UPDATED.value:
            (old_pool_data_provider_address,) = eth_abi.abi.decode(
                types=["address"], data=event["topics"][1]
            )
            old_pool_data_provider_address = get_checksum_address(old_pool_data_provider_address)

            (new_pool_data_provider_address,) = eth_abi.abi.decode(
                types=["address"], data=event["topics"][2]
            )
            new_pool_data_provider_address = get_checksum_address(new_pool_data_provider_address)

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
                    select(AaveV3Contract).where(
                        AaveV3Contract.address == old_pool_data_provider_address
                    )
                )
                assert pool_data_provider is not None
                pool_data_provider.address = new_pool_data_provider_address
        elif topic == AaveV3PoolConfigEvent.ADDRESS_SET.value:
            _process_umbrella_creation_event(
                session=session,
                market=market,
                event=event,
                proxy_name="UMBRELLA",
                proxy_id=eth_abi.abi.encode(["bytes32"], [b"UMBRELLA"]),
            )

    # Phase 2
    try:
        pool_configurator = _get_contract(
            session=session,
            market=market,
            contract_name="POOL_CONFIGURATOR",
        )
    except ValueError:
        # Configurator not initialized yet, skip reserve initialization
        pool_configurator = None
    if pool_configurator is not None:
        for event in _fetch_reserve_initialization_events(
            w3=w3,
            configurator_address=pool_configurator.address,
            start_block=start_block,
            end_block=end_block,
        ):
            _process_asset_initialization_event(
                w3=w3,
                event=event,
                market=market,
                session=session,
            )

    # Phase 3
    all_events: list[LogReceipt] = []

    try:
        pool = _get_contract(
            session=session,
            market=market,
            contract_name="POOL",
        )
    except ValueError:
        # Pool not initialized yet, skip to next chunk
        logger.warning(f"Pool not initialized for market {market.id}, skipping")
        return

    pool_events = _fetch_pool_events(
        w3=w3,
        pool_address=pool.address,
        start_block=start_block,
        end_block=end_block,
    )
    all_events.extend(pool_events)

    known_scaled_token_addresses = set(
        _get_all_scaled_token_addresses(
            session=session,
            chain_id=w3.eth.chain_id,
        )
    )
    known_debt_token_addresses = set(
        _get_debt_token_addresses(
            session=session,
            chain_id=w3.eth.chain_id,
        )
    )

    scaled_token_events = _fetch_scaled_token_events(
        w3=w3,
        token_addresses=list(known_scaled_token_addresses),
        start_block=start_block,
        end_block=end_block,
    )
    all_events.extend(scaled_token_events)

    discount_config_events = _fetch_discount_config_events(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
    )
    all_events.extend(discount_config_events)

    gho_asset = _get_gho_asset(session=session, market=market)

    # Process discount config events BEFORE fetching stkAAVE events
    # This ensures gho_asset.v_gho_discount_token is set correctly
    if gho_asset is not None:
        for event in discount_config_events:
            topic = event["topics"][0]
            if topic == AaveV3GhoDebtTokenEvent.DISCOUNT_TOKEN_UPDATED.value:
                # Update the discount token directly
                new_discount_token_address = decode_address(event["topics"][2])
                gho_asset.v_gho_discount_token = new_discount_token_address
                logger.info(
                    f"SET NEW DISCOUNT TOKEN: {decode_address(event['topics'][1])} -> "
                    f"{new_discount_token_address}"
                )

        # If v_gho_discount_token is still None, try to fetch it from the contract
        # This handles the case where we're processing blocks before any
        # DISCOUNT_TOKEN_UPDATED event or when the database hasn't been initialized
        if gho_asset.v_gho_discount_token is None:
            try:
                discount_token_from_contract = _fetch_discount_token_from_contract(
                    w3=w3,
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
                w3=w3,
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
        w3=w3,
        gho_asset=gho_asset,
        known_scaled_token_addresses=known_scaled_token_addresses,
        known_debt_token_addresses=known_debt_token_addresses,
        pool_address=get_checksum_address(pool.address),
    )

    # Build users by block for verification at block boundaries
    users_by_block: dict[int, set[ChecksumAddress]] = {}
    for event in all_events:
        block_number = event["blockNumber"]
        if block_number not in users_by_block:
            users_by_block[block_number] = set()
        users_by_block[block_number].update(_extract_user_addresses_from_event(event))

    # Sort transaction contexts chronologically by (block_number, first_event_log_index)
    sorted_tx_contexts = sorted(
        tx_contexts.values(),
        key=lambda ctx: (
            (ctx.block_number, ctx.events[0]["logIndex"]) if ctx.events else (ctx.block_number, 0)
        ),
    )

    last_verified_block: int | None = None

    # Process transactions chronologically, verifying at block boundaries
    for tx_context in tqdm.tqdm(
        sorted_tx_contexts,
        desc="Processing transactions",
        leave=False,
        disable=not show_progress,
    ):
        current_block = tx_context.block_number

        # Log block boundary for debugging when entering a new block
        if aave_debug_logger.is_enabled():
            block_users = users_by_block.get(current_block, set())
            aave_debug_logger.log_block_boundary(
                block_number=current_block,
                event_count=len([e for e in all_events if e["blockNumber"] == current_block]),
                user_count=len(block_users),
                user_addresses=[addr.lower() for addr in block_users],
            )

        # Verify users from the previous block before processing first transaction of new block
        if verify and last_verified_block is not None and current_block != last_verified_block:
            users_to_verify = users_by_block.get(last_verified_block, set())
            if users_to_verify:
                session.flush()
                _verify_scaled_token_positions(
                    w3=w3,
                    market=market,
                    session=session,
                    position_table=AaveV3CollateralPosition,
                    block_number=last_verified_block,
                    show_progress=show_progress,
                    user_addresses=users_to_verify,
                )
                _verify_scaled_token_positions(
                    w3=w3,
                    market=market,
                    session=session,
                    position_table=AaveV3DebtPosition,
                    block_number=last_verified_block,
                    show_progress=show_progress,
                    user_addresses=users_to_verify,
                )
                _verify_stk_aave_balances(
                    w3=w3,
                    session=session,
                    market=market,
                    gho_asset=gho_asset,
                    block_number=last_verified_block,
                    show_progress=show_progress,
                    user_addresses=users_to_verify,
                )
                _verify_gho_discount_amounts(
                    w3=w3,
                    session=session,
                    market=market,
                    gho_asset=gho_asset,
                    block_number=last_verified_block,
                    show_progress=show_progress,
                    user_addresses=users_to_verify,
                )

        # Set global event reference for debugging
        if tx_context.events:
            global event_in_process  # noqa: PLW0603
            event_in_process = tx_context.events[0]

        # Process entire transaction atomically with full context
        _process_transaction(tx_context=tx_context)

        last_verified_block = current_block

    # Perform final verification at chunk boundary for the last block
    if verify and last_verified_block is not None:
        users_to_verify = users_by_block.get(last_verified_block, set())
        if users_to_verify:
            session.flush()
            _verify_scaled_token_positions(
                w3=w3,
                market=market,
                session=session,
                position_table=AaveV3CollateralPosition,
                block_number=last_verified_block,
                show_progress=show_progress,
                user_addresses=users_to_verify,
            )
            _verify_scaled_token_positions(
                w3=w3,
                market=market,
                session=session,
                position_table=AaveV3DebtPosition,
                block_number=last_verified_block,
                show_progress=show_progress,
                user_addresses=users_to_verify,
            )
            _verify_stk_aave_balances(
                w3=w3,
                session=session,
                market=market,
                gho_asset=gho_asset,
                block_number=last_verified_block,
                show_progress=show_progress,
                user_addresses=users_to_verify,
            )
            _verify_gho_discount_amounts(
                w3=w3,
                session=session,
                market=market,
                gho_asset=gho_asset,
                block_number=last_verified_block,
                show_progress=show_progress,
                user_addresses=users_to_verify,
            )

    logger.info(f"{market} successfully updated to block {end_block:,}")
