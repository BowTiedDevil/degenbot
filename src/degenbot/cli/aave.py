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
from web3 import Web3
from web3.exceptions import ContractLogicError
from web3.types import LogReceipt, TxParams

from degenbot.aave.deployments import EthereumMainnetAaveV3
from degenbot.aave.enrichment import ScaledEventEnricher
from degenbot.aave.events import (
    AaveV3GhoDebtTokenEvent,
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
from degenbot.aave.models import EnrichedScaledTokenEvent
from degenbot.aave.processors import (
    CollateralBurnEvent,
    CollateralMintEvent,
    DebtBurnEvent,
    DebtMintEvent,
    TokenProcessorFactory,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.cli import cli
from degenbot.cli.aave_event_matching import OperationAwareEventMatcher
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
from degenbot.constants import DEAD_ADDRESS, ERC_1967_IMPLEMENTATION_SLOT, MAX_UINT256, ZERO_ADDRESS
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
}


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


FULL_VERIFICATION_INTERVAL = 250_000
SCALED_AMOUNT_POOL_REVISION = 9


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
        AaveV3PoolEvent.RESERVE_DATA_UPDATED.value,
        AaveV3PoolEvent.MINTED_TO_TREASURY.value,
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

            # GHO tokens are chain-unique, so create a single entry that all markets on this chain
            # will share.
            gho_asset_token = _get_or_create_erc20_token(
                w3=w3,
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
def aave_update(
    *,
    chunk_size: int,
    to_block: str,
    verify: bool,
    stop_after_one_chunk: bool,
    show_progress: bool,
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
            w3 = get_web3_from_config(chain_id=chain_id)

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
                                w3=w3,
                                start_block=working_start_block,
                                end_block=working_end_block,
                                market=market,
                                session=session,
                                verify=verify,
                                show_progress=show_progress,
                            )
                        except Exception:  # noqa: BLE001
                            logger.exception("")
                            sys.exit(1)

                        market.last_update_block = working_end_block

                        # Perform full verification when the chunk spans a verification interval
                        if (
                            verify
                            and working_end_block // FULL_VERIFICATION_INTERVAL
                            != working_start_block // FULL_VERIFICATION_INTERVAL
                        ):
                            _verify_all_positions(
                                w3=w3,
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
                            backup_sqlite_database(
                                session=session,
                                suffix=f"{working_end_block}",
                                skip_confirmation=True,
                            )
                            logger.info(f"Created database backup at block {working_end_block:,}")
                            db_session.remove()
                        else:
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
        w3=w3,
        session=session,
        chain_id=market.chain_id,
        token_address=asset_address,
    )
    a_token = _get_or_create_erc20_token(
        w3=w3,
        session=session,
        chain_id=market.chain_id,
        token_address=a_token_address,
    )
    v_token = _get_or_create_erc20_token(
        w3=w3,
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
            w3=tx_context.w3,
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
        gho_asset = _get_gho_asset(tx_context.session, tx_context.market)
        if (
            gho_asset.v_token is not None
            and aave_debt_asset.v_token.address == gho_asset.v_token.address
            and vtoken_revision >= 4  # noqa: PLR2004
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
        token_address = get_checksum_address(event["address"])
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
    return revision is not None and revision < 4  # noqa: PLR2004


def _prefetch_users_for_transaction(
    tx_context: TransactionContext,
    user_addresses: set[ChecksumAddress],
) -> dict[ChecksumAddress, AaveV3User]:
    """
    Batch prefetch all existing users for a transaction.

    This reduces N+1 queries by loading all users in a single query upfront.
    Returns a dictionary mapping user addresses to their AaveV3User objects.

    Args:
        tx_context: The transaction context containing session and market
        user_addresses: Set of user addresses to prefetch

    Returns:
        Dictionary mapping address -> AaveV3User for existing users
    """

    if not user_addresses:
        return {}

    users = tx_context.session.scalars(
        select(AaveV3User).where(
            AaveV3User.address.in_(list(user_addresses)),
            AaveV3User.market_id == tx_context.market.id,
        )
    ).all()

    return {user.address: user for user in users}


def _prefetch_positions_for_transaction(
    tx_context: TransactionContext,
    user_addresses: set[ChecksumAddress],
) -> None:
    """
    Batch prefetch all positions for users in a transaction.

    Uses existing user_cache to avoid N+1 queries. Positions are cached with
    table class as discriminator to distinguish collateral vs debt positions
    for the same user and asset.
    """

    # Build reverse lookup: user_id -> user_address
    user_id_to_address = {user.id: addr for addr, user in tx_context.user_cache.items()}

    if not user_id_to_address:
        return

    user_ids = list(user_id_to_address.keys())

    # Query collateral positions (no JOIN needed - we have user data in cache)
    collateral_positions = tx_context.session.scalars(
        select(AaveV3CollateralPosition).where(AaveV3CollateralPosition.user_id.in_(user_ids))
    ).all()

    for collateral_pos in collateral_positions:
        # INVARIANT: Every position must belong to a user in our cache
        assert collateral_pos.user_id in user_id_to_address, (
            f"Collateral position {collateral_pos.id} (user_id={collateral_pos.user_id}) "
            f"found but user not in transaction user_cache. "
            f"This indicates a logic error in user prefetching."
        )

        user_address = user_id_to_address[collateral_pos.user_id]
        tx_context.modified_positions[
            user_address,
            collateral_pos.asset_id,
            AaveV3CollateralPosition,
        ] = collateral_pos

    # Query debt positions
    debt_positions = tx_context.session.scalars(
        select(AaveV3DebtPosition).where(AaveV3DebtPosition.user_id.in_(user_ids))
    ).all()

    for debt_pos in debt_positions:
        # INVARIANT: Every position must belong to a user in our cache
        assert debt_pos.user_id in user_id_to_address, (
            f"Debt position {debt_pos.id} (user_id={debt_pos.user_id}) "
            f"found but user not in transaction user_cache. "
            f"This indicates a logic error in user prefetching."
        )

        user_address = user_id_to_address[debt_pos.user_id]
        tx_context.modified_positions[
            user_address,
            debt_pos.asset_id,
            AaveV3DebtPosition,
        ] = debt_pos

    logger.debug(
        f"Prefetched {len(collateral_positions)} collateral + {len(debt_positions)} "
        f"debt positions for {len(user_addresses)} users"
    )


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

    # Check cache first to avoid database query
    if user_address in tx_context.user_cache:
        return tx_context.user_cache[user_address]

    # User not in cache - query database (this handles the edge case where
    # a user was added by a concurrent transaction or cache wasn't pre-filled)
    user = tx_context.session.scalar(
        select(AaveV3User).where(
            AaveV3User.address == user_address,
            AaveV3User.market_id == tx_context.market.id,
        )
    )

    if user is not None:
        # Add to cache for future lookups
        tx_context.user_cache[user_address] = user
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
                w3=tx_context.w3,
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

    # Add new user to cache
    tx_context.user_cache[user_address] = user

    return user


def _fetch_erc20_token_metadata(
    w3: Web3,
    token_address: ChecksumAddress,
) -> tuple[str | None, str | None, int | None]:
    """
    Fetch ERC20 token metadata (name, symbol, decimals) from the blockchain.

    Attempts to fetch using standard ERC20 function signatures, falling back
    to uppercase versions and bytes32 decoding as needed.

    Args:
        chain_id: The chain ID where the token exists
        token_address: The token contract address

    Returns:
        Tuple of (name, symbol, decimals) or (None, None, None) if all fetch attempts fail
    """

    name = _try_fetch_token_string(
        w3=w3,
        token_address=token_address,
        lower_func="name()",
        upper_func="NAME()",
    )
    symbol = _try_fetch_token_string(
        w3=w3,
        token_address=token_address,
        lower_func="symbol()",
        upper_func="SYMBOL()",
    )
    decimals = _try_fetch_token_uint256(
        w3=w3,
        token_address=token_address,
        lower_func="decimals()",
        upper_func="DECIMALS()",
    )

    return name, symbol, decimals


def _try_fetch_token_string(
    w3: Web3,
    token_address: ChecksumAddress,
    lower_func: str,
    upper_func: str,
) -> str | None:
    """
    Try to fetch a string value from an ERC20 token, with fallback to bytes32.
    """

    for func_prototype in (lower_func, upper_func):
        try:
            result = w3.eth.call(
                TxParams(
                    to=token_address,
                    data=encode_function_calldata(
                        function_prototype=func_prototype,
                        function_arguments=None,
                    ),
                )
            )

            try:
                (value,) = eth_abi.abi.decode(types=["string"], data=result)
                return str(value)
            except eth_abi.exceptions.DecodingError:
                # Fallback for older tokens that return bytes32
                (value,) = eth_abi.abi.decode(types=["bytes32"], data=result)
                return (
                    value.decode("utf-8", errors="ignore").strip("\x00")
                    if isinstance(value, (bytes, HexBytes))
                    else str(value)
                )
        except Exception:
            continue

    return None


def _try_fetch_token_uint256(
    w3: Web3,
    token_address: ChecksumAddress,
    lower_func: str,
    upper_func: str,
) -> int | None:
    """Try to fetch a uint256 value from an ERC20 token."""
    for func_prototype in (lower_func, upper_func):
        try:
            (result,) = raw_call(
                w3=w3,
                address=token_address,
                calldata=encode_function_calldata(
                    function_prototype=func_prototype,
                    function_arguments=None,
                ),
                return_types=["uint256"],
            )
            return int(result)
        except Exception:
            continue

    return None


def _get_or_create_erc20_token(
    w3: Web3,
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
            w3=w3,
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

    Uses tx_context.modified_positions cache to avoid repeated database queries.
    New positions are created on-demand and added to the cache.
    """

    # INVARIANT: User must be in the transaction's user cache
    assert user.address in tx_context.user_cache, (
        f"User {user.address} not found in transaction user_cache. "
        f"All users should be prefetched before position access. "
        f"This indicates _get_or_create_user was not used or user_cache is corrupted."
    )

    # Check cache using table class as discriminator
    cache_key = (user.address, asset_id, position_table)
    if cache_key in tx_context.modified_positions:
        cached_position = tx_context.modified_positions[cache_key]

        # INVARIANT: Cached position must be of expected type
        assert isinstance(cached_position, position_table), (
            f"Cache type mismatch: expected {position_table.__name__}, "
            f"got {type(cached_position).__name__} for key {cache_key}. "
            f"This indicates cache key collision or corruption."
        )

        return cached_position

    # Cache miss - query database
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

        tx_context.modified_positions[cache_key] = existing_position
        return existing_position

    # Create new position
    new_position = position_table(user_id=user.id, asset_id=asset_id, balance=0)
    tx_context.session.add(new_position)
    tx_context.session.flush()

    # Add new position to cache
    tx_context.modified_positions[cache_key] = new_position

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
    w3: Web3,
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
        (discount_token,) = raw_call(
            w3=w3,
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
    fetched_index = _get_current_borrow_index_from_pool(
        w3=tx_context.w3,
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
                w3=w3,
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

    # Verify all collateral positions
    _verify_scaled_token_positions(
        w3=w3,
        market=market,
        session=session,
        position_table=AaveV3CollateralPosition,
        block_number=block_number,
        show_progress=show_progress,
    )

    # Verify all debt positions
    _verify_scaled_token_positions(
        w3=w3,
        market=market,
        session=session,
        position_table=AaveV3DebtPosition,
        block_number=block_number,
        show_progress=show_progress,
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
    )

    # Verify all GHO discount amounts
    _verify_gho_discount_amounts(
        w3=w3,
        session=session,
        market=market,
        gho_asset=gho_asset,
        block_number=block_number,
        show_progress=show_progress,
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
            w3=w3,
            address=token_address,
            calldata=encode_function_calldata(
                function_prototype="scaledBalanceOf(address)",
                function_arguments=[position.user.address],
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
                function_arguments=[position.user.address],
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


def _process_transaction(tx_context: TransactionContext) -> None:
    """
    Process transaction using operation-based parsing.
    """

    # Capture user discount percents before processing events
    # This ensures calculations use the discount in effect at the start of the transaction

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
        event_address = get_checksum_address(event["address"])

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
                        w3=tx_context.w3,
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
                tx_context.last_withdraw_token_address = get_checksum_address(
                    first_event.event["address"]
                )
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

    # Create enricher and matcher for this operation
    enricher = ScaledEventEnricher(
        pool_revision=tx_context.pool_revision,
        token_revisions={},
        session=tx_context.session,
    )
    matcher = OperationAwareEventMatcher(operation, enricher)

    # Log liquidation operations for debugging
    # Process each scaled token event in the operation
    # Sort by log index to ensure events are processed in chronological order
    sorted_scaled_events = sorted(
        operation.scaled_token_events,
        key=lambda e: e.event["logIndex"],
    )
    for scaled_event in sorted_scaled_events:
        event = scaled_event.event

        # Find match within operation context
        match_result = matcher.find_match(scaled_event)

        # Route to appropriate handler based on event type
        enriched_event = match_result.enriched_event
        if scaled_event.event_type == ScaledTokenEventType.COLLATERAL_MINT:
            # Special case: When interest exceeds withdrawal amount, the aToken contract
            # emits a Mint event instead of a Burn event (AToken rev_4.sol:2836-2839).
            # This happens when nextBalance > previousBalance after burning.
            # Detection: amount < balance_increase indicates the withdrawal was less than interest.
            # In this case, we should treat it as a burn (subtract from balance), not a mint.
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
        tx_context: Transaction context with Web3 and market info
        operation: The operation containing minted_to_treasury_amount

    Returns:
        The calculated scaled amount to add to the treasury position
    """

    # No BalanceTransfer handling here - liquidation fees are processed by liquidation operations
    # This function only handles actual mints to treasury (protocol fee accrual)

    # No BalanceTransfer - use MintedToTreasury amount
    assert scaled_event.balance_increase is not None
    assert scaled_event.index is not None

    # Special case: when amount == balanceIncrease, the treasury's existing
    # aTokens accrued interest equal to the accruedToTreasury amount.
    # No new scaled tokens are minted - the existing balance simply appreciated.
    if scaled_event.amount == scaled_event.balance_increase:
        logger.debug("MINT_TO_TREASURY: amount == balanceIncrease, setting scaled_amount = 0")
        return 0

    # Get the MintedToTreasury amount from the operation
    # This is populated during operation creation for ALL revisions
    if operation.minted_to_treasury_amount is None:
        msg = "MINT_TO_TREASURY operation missing minted_to_treasury_amount"
        raise ValueError(msg)

    minted_amount = operation.minted_to_treasury_amount

    # Use PoolMath for revision-aware calculation
    # This delegates to the appropriate rounding library based on Pool revision
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
    token_address = get_checksum_address(scaled_event.event["address"])
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

    token_address = get_checksum_address(scaled_event.event["address"])
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
    token_address = get_checksum_address(scaled_event.event["address"])
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
    token_address = get_checksum_address(scaled_event.event["address"])
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
        if operation.operation_type == OperationType.GHO_REPAY:
            # Use enriched scaled_amount (calculated from Repay event in enrichment layer)
            # instead of calling processor which derives amount from Mint event fields.
            # This avoids 1 wei rounding errors from integer truncation in interest calculations.
            # See debug/aave/0037 - GHO REPAY Uses Mint Event Instead of Repay Event Amount.md
            assert enriched_event.scaled_amount is not None
            debt_position.balance -= enriched_event.scaled_amount
            _update_debt_position_index(
                tx_context=tx_context,
                debt_asset=debt_asset,
                debt_position=debt_position,
                event_index=scaled_event.index,
                event_block_number=scaled_event.event["blockNumber"],
            )
        else:
            # Use the effective discount from transaction context
            effective_discount = tx_context.user_discounts.get(user.address, user.gho_discount)

            # Process using GHO-specific processor
            gho_processor = TokenProcessorFactory.get_gho_debt_processor(
                debt_asset.v_token_revision
            )
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

        # Check if this Mint event is part of a REPAY operation
        # In REPAY, Mint is emitted when interest > repayment, but the net effect
        # is still a burn of scaled tokens
        if operation.operation_type in {OperationType.REPAY, OperationType.GHO_REPAY}:
            # Treat as burn: calculate actual scaled burn amount from Pool event
            # Use TokenMath to match on-chain calculation
            assert operation.pool_event is not None
            repay_amount, _ = eth_abi.abi.decode(
                types=["uint256", "bool"],
                data=operation.pool_event["data"],
            )
            # Use token revision (not pool revision) to get correct TokenMath
            token_math = TokenMathFactory.get_token_math_for_token_revision(
                debt_asset.v_token_revision
            )
            actual_scaled_burn = token_math.get_debt_burn_scaled_amount(
                repay_amount, scaled_event.index
            )
            logger.debug(
                f"REPAY with Mint event: treating as burn, "
                f"repay_amount={repay_amount}, scaled_burn={actual_scaled_burn}"
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
    token_address = get_checksum_address(scaled_event.event["address"])
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
            current_index = debt_position.last_index
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

        # For liquidation operations, determine if this is a bad debt (deficit) liquidation
        # Bad debt liquidations have a DEFICIT_CREATED event and burn the FULL debt balance
        # Normal liquidations burn only the debtToCover amount
        is_bad_debt_liquidation = False
        if operation and operation.operation_type in {
            OperationType.LIQUIDATION,
            OperationType.GHO_LIQUIDATION,
        }:
            # Check if there's a DEFICIT_CREATED event for the same user in this transaction
            for evt in tx_context.events:
                if evt["topics"][0] == AaveV3PoolEvent.DEFICIT_CREATED.value:
                    # DEFICIT_CREATED event has user as topic[1]
                    deficit_user = get_checksum_address("0x" + evt["topics"][1].hex()[-40:])
                    if deficit_user == user.address:
                        is_bad_debt_liquidation = True
                        logger.debug(
                            f"_process_debt_burn_with_match: Bad debt liquidation detected "
                            f"for user {user.address}"
                        )
                        break

        if is_bad_debt_liquidation:
            # Bad debt liquidation: The contract burns the ENTIRE debt balance (borrowerReserveDebt)
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
            current_index = debt_position.last_index or 0
            if scaled_event.index > current_index:
                debt_position.last_index = scaled_event.index
            return
        if operation and operation.operation_type in {
            OperationType.LIQUIDATION,
            OperationType.GHO_LIQUIDATION,
        }:
            # Normal liquidation: use debtToCover from pool event
            burn_value = enriched_event.raw_amount
            logger.debug(
                f"_process_debt_burn_with_match: NORMAL LIQUIDATION - using "
                f"debtToCover={burn_value}"
            )
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
    """
    # Skip paired BalanceTransfer events - handled by their paired ERC20 Transfer
    if (
        scaled_event.index is not None
        and scaled_event.index > 0
        and operation
        and operation.balance_transfer_events
    ):
        for bt_event in operation.balance_transfer_events:
            if bt_event["logIndex"] == scaled_event.event["logIndex"]:
                return True

    # Skip REPAY_WITH_ATOKENS transfers (handled by burn event)
    if operation and operation.operation_type == OperationType.REPAY_WITH_ATOKENS:
        return True

    # Skip protocol mints (from zero address)
    if scaled_event.from_address == ZERO_ADDRESS:
        return True

    # Skip ERC20 transfers corresponding to direct burns (handled by Burn event)
    if scaled_event.index is None and scaled_event.target_address == ZERO_ADDRESS:
        gho_vtoken_address = tx_context.gho_vtoken_address
        for evt in tx_context.events:
            if evt["topics"][0] != AaveV3ScaledTokenEvent.BURN.value:
                continue
            # Skip GHO debt burns - collateral burns are all other burns
            if (
                gho_vtoken_address is not None
                and get_checksum_address(evt["address"]) == gho_vtoken_address
            ):
                continue
            if get_checksum_address(evt["address"]) == get_checksum_address(
                scaled_event.event["address"]
            ):
                burn_user = get_checksum_address("0x" + evt["topics"][1].hex()[-40:])
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

    for bt_event in operation.balance_transfer_events:
        bt_from = get_checksum_address("0x" + bt_event["topics"][1].hex()[-40:])
        bt_to = get_checksum_address("0x" + bt_event["topics"][2].hex()[-40:])
        bt_token = get_checksum_address(bt_event["address"])

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

    token_address = get_checksum_address(scaled_event.event["address"])
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
            # Standalone ERC20 Transfer - use current liquidity index
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

    logger.debug(f"Processing _process_debt_transfer_with_match at block {event['blockNumber']}")

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
    token_address = get_checksum_address(scaled_event.event["address"])
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


def _fetch_pool_events(
    w3: Web3,
    pool_address: ChecksumAddress,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch Pool contract events for assertions and config updates.
    """

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
                AaveV3PoolEvent.MINTED_TO_TREASURY.value,
            ]
        ],
    )


def _fetch_reserve_initialization_events(
    w3: Web3,
    configurator_address: ChecksumAddress,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch Pool Configurator events for reserve initialization.
    """

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
    """
    Fetch events from all scaled tokens (aTokens, vTokens).
    """

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
    """
    Fetch stkAAVE events including STAKED and REDEEM for classification.
    """

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
    """
    Fetch discount-related events from any contract (not address-specific).
    """

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


def _log_event_categorization(
    *,
    topic: HexBytes,
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
    w3: Web3,
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

    logger.debug(
        f"_build_transaction_contexts: completed with {len(contexts)} transaction contexts"
    )

    # Prefetch all users for each transaction to avoid N+1 queries
    for tx_hash, ctx in contexts.items():
        user_addresses = _extract_user_addresses_from_transaction(ctx.events)
        if user_addresses:
            user_cache = _prefetch_users_for_transaction(ctx, user_addresses)
            ctx.user_cache = user_cache

            # INVARIANT: Prefetch should find or create all users
            assert len(user_cache) <= len(user_addresses), (
                f"User cache size exceeds expected: got {len(user_cache)}, "
                f"expected at most {len(user_addresses)}. This indicates duplicate users."
            )

            # Prefetch positions (uses user cache)
            _prefetch_positions_for_transaction(ctx, user_addresses)

            logger.debug(
                f"Prefetched {len(user_cache)} users and {len(ctx.modified_positions)} "
                f"positions for transaction {tx_hash.to_0x_hex()}"
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
        pool_contract=pool,
    )

    # Sort transaction contexts chronologically by (block_number, first_event_log_index)
    sorted_tx_contexts = sorted(
        tx_contexts.values(),
        key=lambda ctx: (
            (ctx.block_number, ctx.events[0]["logIndex"]) if ctx.events else (ctx.block_number, 0)
        ),
    )

    # Collect all users modified during this chunk for verification
    users_modified: set[ChecksumAddress] = set()

    # Process transactions chronologically
    for tx_context in tqdm.tqdm(
        sorted_tx_contexts,
        desc="Processing transactions",
        leave=False,
        disable=not show_progress,
    ):
        # Process entire transaction atomically with full context
        _process_transaction(tx_context=tx_context)

        # Track users modified in this transaction
        users_modified.update(tx_context.user_cache.keys())

    # Perform verification at chunk boundary for all modified users
    if verify and users_modified:
        _verify_scaled_token_positions(
            w3=w3,
            market=market,
            session=session,
            position_table=AaveV3CollateralPosition,
            block_number=end_block,
            show_progress=show_progress,
            user_addresses=users_modified,
        )
        _verify_scaled_token_positions(
            w3=w3,
            market=market,
            session=session,
            position_table=AaveV3DebtPosition,
            block_number=end_block,
            show_progress=show_progress,
            user_addresses=users_modified,
        )
        _verify_stk_aave_balances(
            w3=w3,
            session=session,
            market=market,
            gho_asset=gho_asset,
            block_number=end_block,
            show_progress=show_progress,
            user_addresses=users_modified,
        )
        _verify_gho_discount_amounts(
            w3=w3,
            session=session,
            market=market,
            gho_asset=gho_asset,
            block_number=end_block,
            show_progress=show_progress,
            user_addresses=users_modified,
        )

    logger.info(f"{market} successfully updated to block {end_block:,}")
