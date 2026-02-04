"""
Aave V3 CLI commands for market synchronization and position tracking.

Provides commands to activate and update Aave markets by fetching and processing related blockchain
events. The goal is to maintain a synchronized view of all user positions in the database.
Collateral (aTokens) and debt (vTokens) positions are tracked separately.

CLI Commands:
    aave activate [market name] - Enable updates for this market
    aave deactivate ethereum_aave_v3 - Disable updates for this market; existing data for the
        market will be preserved, but the market will be excluded from subsequent updates
    aave update - Synchronize positions for all active markets to the given block

Event Processing:
    Processes blockchain events chronologically by (blockNumber, logIndex):
    - ReserveDataUpdated: Updates liquidity/borrow rates and indices
    - Mint: Collateral deposits, debt borrows, or interest accrual
    - Burn: Collateral withdrawals or debt repayments
    - BalanceTransfer: Collateral transfers between users
    - UserEModeSet: E-mode category changes
    - Upgraded: Token contract upgrades (revision changes)
    - DiscountTokenUpdated / DiscountRateStrategyUpdated: GHO config changes

Scaled Balance Tracking:
    Database stores scaled balances: actual_balance = scaled * index
    - Mint: amountScaled = ray_div(user_amount, index)
    - Burn: amountScaled = ray_div(user_amount + interest, index)
    - Interest accrues automatically via index updates

GHO Discount Mechanism:
    GHO borrowers receive interest discounts based on stkAAVE holdings.
    Discount rate is recalculated on each balance-changing action.
    Version-specific math libraries handle different GHO contract revisions.

stkAAVE Balance Tracking (Event-Based):
    Instead of making expensive RPC calls to fetch stkAAVE balances, we track them via events.
    This eliminates 9 RPC calls per block and scales better with user count.

    Rationale:
        - RPC calls to `balanceOf` are expensive and don't scale
        - stkAAVE balances only change via Staked, Redeem, Transfer, and Slashed events
        - Event-based tracking is deterministic and auditable
        - Previous block lookups ensure correct state when processing intra-block events

    Design Assumptions:
        - stkAAVE events (Staked, Redeem, Transfer, Slashed) are emitted before GHO debt events
          in the same transaction. This is guaranteed by the Aave protocol's implementation.
        - Balances are lazily loaded from RPC when first needed (nullable column)
        - Lazy loading fetches from block-1 to ensure pre-event state
        - Once initialized, balances are updated immediately via events

    Event Processing Order:
        - stkAAVE events update database balances immediately as processed
        - GHO debt events read from database (which has current state)
        - No caching needed since DB is source of truth

    Invariants:
        - user.stk_aave_balance is None until first needed (lazy loading)
        - After lazy load, balance reflects state at block-1 (before any events in current block)
        - stkAAVE events in current block update the balance to reflect current state
        - GHO events always see correct balance for discount calculations
        - Transfer events route to correct handler based on contract address

Transaction-Level Discount Tracking:
    When DiscountPercentUpdated and Mint/Burn events occur in the same transaction, Mint/Burn
    operations must use the OLD discount rate (pre-update), not the new rate. This mirrors the
    contract's _accrueDebtOnAction behavior which uses the stored discount rate before any updates
    in the current transaction. The tx_discount_overrides dictionary tracks these per-transaction,
    per-user overrides and is cleared at transaction boundaries.

Token Revisions:
    Aave protocol upgrades produce multiple token versions (v3.1-v3.4). Each revision uses specific
    math libraries and control flow, which can change. Upgrades are tracked via EIP-1967 proxy
    events and contract calls to identify the current revision for a particular token. Functions
    involving scaled tokens (aToken, vToken) contain version checks to take the correct actions.

Debug Controls (Environment Variables):
    DEGENBOT_VERBOSE_ALL=1 - Enable all verbose logging
    DEGENBOT_VERBOSE_USERS=0x123...,0x456... - Trace specific addresses
    DEGENBOT_VERBOSE_TX=0xabc...,0xdef... - Trace specific transactions

See src/degenbot/aave/AGENTS.md for architecture details.
See docs/cli/aave.md for command reference.
"""

import operator
import os
from collections.abc import Callable
from dataclasses import dataclass, field
from enum import Enum
from typing import TYPE_CHECKING, Any, ClassVar, Protocol, TypedDict, cast

import click
import eth_abi.abi
import tqdm
from eth_typing import ChainId, ChecksumAddress
from hexbytes import HexBytes
from sqlalchemy import delete, select
from sqlalchemy.orm import Session
from web3 import Web3
from web3.types import LogReceipt

import degenbot.aave.libraries.v3_1 as aave_library_v3_1
import degenbot.aave.libraries.v3_2 as aave_library_v3_2
import degenbot.aave.libraries.v3_3 as aave_library_v3_3
import degenbot.aave.libraries.v3_4 as aave_library_v3_4
from degenbot.aave.deployments import EthereumMainnetAaveV3
from degenbot.checksum_cache import get_checksum_address
from degenbot.cli import cli
from degenbot.cli.utils import get_web3_from_config
from degenbot.constants import ERC_1967_IMPLEMENTATION_SLOT, ZERO_ADDRESS
from degenbot.database import db_session
from degenbot.database.models.aave import (
    AaveGhoTokenTable,
    AaveV3AssetsTable,
    AaveV3CollateralPositionsTable,
    AaveV3ContractsTable,
    AaveV3DebtPositionsTable,
    AaveV3MarketTable,
    AaveV3UsersTable,
)
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.functions import (
    encode_function_calldata,
    fetch_logs_retrying,
    get_number_for_block_identifier,
    raw_call,
)
from degenbot.logging import logger

if TYPE_CHECKING:
    from eth_typing.evm import BlockParams


class TokenType(Enum):
    """Token type for Aave V3 asset lookups."""

    COLLATERAL = "a_token_id"
    DEBT = "v_token_id"


# TODO: implement collateral enabled scraper


type TokenRevision = int


class UserOperation(Enum):
    """User operation types for Aave V3 token events."""

    DEPOSIT = "DEPOSIT"
    WITHDRAW = "WITHDRAW"
    BORROW = "BORROW"
    REPAY = "REPAY"
    GHO_BORROW = "GHO BORROW"
    GHO_REPAY = "GHO REPAY"
    AAVE_STAKED = "AAVE STAKED"
    AAVE_REDEEM = "AAVE REDEEM"
    STKAAVE_TRANSFER = "stkAAVE TRANSFER"


@dataclass
class BlockStateCache:
    """Cache for blockchain state within a single block and its predecessor."""

    w3: Web3
    block_number: int
    _cache: dict[tuple[str, ...], Any] = field(default_factory=dict)
    _prev_block_cache: dict[tuple[str, ...], Any] = field(default_factory=dict)
    _processed_transfer_log_indices: set[int] = field(default_factory=set)

    def mark_transfer_processed(self, log_index: int) -> None:
        """Mark a transfer event as processed to avoid reprocessing."""
        self._processed_transfer_log_indices.add(log_index)

    def is_transfer_processed(self, log_index: int) -> bool:
        """Check if a transfer event has already been processed."""
        return log_index in self._processed_transfer_log_indices


# GhoVariableDebtToken
# Rev 1: 0x3FEaB6F8510C73E05b8C0Fdf96Df012E3A144319
# Rev 2: 0x7aa606b1B341fFEeAfAdbbE4A2992EFB35972775
#        0x4da27a545c0c5B758a6BA100e3a049001de870f5 (discount token stkAAVE)

# GhoDiscountRateStrategy
# 0x4C38Ec4D1D2068540DfC11DFa4de41F733DDF812

GHO_VARIABLE_DEBT_TOKEN_ADDRESS = get_checksum_address("0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B")

# TODO: debug variable, remove this after testing is complete
event_in_process: LogReceipt


@dataclass
class EventHandlerContext:
    """Context object passed to event handlers containing all necessary state."""

    w3: Web3
    event: LogReceipt
    market: AaveV3MarketTable
    session: Session
    users_to_check: dict[ChecksumAddress, int]
    gho_users_to_check: dict[ChecksumAddress, int]
    cache: BlockStateCache
    tx_discount_overrides: dict[tuple[HexBytes, ChecksumAddress], int]
    tx_discount_updated_users: set[ChecksumAddress]
    contract_address: ChecksumAddress


class VerboseConfig:
    """Runtime configurable verbose logging settings for Aave event processing."""

    all_enabled: ClassVar[bool] = False
    users: ClassVar[set[ChecksumAddress]] = set()
    transactions: ClassVar[set[HexBytes]] = set()

    @classmethod
    def toggle_all(cls, *, enabled: bool | None = None) -> bool:
        """Toggle or set VERBOSE_ALL. Returns the new state."""
        if enabled is None:
            cls.all_enabled = not cls.all_enabled
        else:
            cls.all_enabled = enabled
        return cls.all_enabled

    @classmethod
    def add_user(cls, user_address: ChecksumAddress) -> None:
        """Add a user address to VERBOSE_USERS."""
        cls.users.add(user_address)

    @classmethod
    def remove_user(cls, user_address: ChecksumAddress) -> None:
        """Remove a user address from VERBOSE_USERS."""
        cls.users.discard(user_address)

    @classmethod
    def clear_users(cls) -> None:
        """Clear all users from VERBOSE_USERS."""
        cls.users.clear()

    @classmethod
    def add_transaction(cls, tx_hash: HexBytes | str) -> None:
        """Add a transaction hash to VERBOSE_TX."""
        if isinstance(tx_hash, str):
            tx_hash = HexBytes(tx_hash)
        cls.transactions.add(tx_hash)

    @classmethod
    def remove_transaction(cls, tx_hash: HexBytes | str) -> None:
        """Remove a transaction hash from VERBOSE_TX."""
        if isinstance(tx_hash, str):
            tx_hash = HexBytes(tx_hash)
        cls.transactions.discard(tx_hash)

    @classmethod
    def clear_transactions(cls) -> None:
        """Clear all transactions from VERBOSE_TX."""
        cls.transactions.clear()

    @classmethod
    def is_verbose(
        cls,
        user_address: ChecksumAddress | None = None,
        tx_hash: HexBytes | None = None,
    ) -> bool:
        """Check if verbose logging should be enabled for the given context."""
        return (
            cls.all_enabled
            or (user_address is not None and user_address in cls.users)
            or (tx_hash is not None and tx_hash in cls.transactions)
        )


def _init_verbose_config_from_env() -> None:
    """Initialize VerboseConfig from environment variables."""
    # DEGENBOT_VERBOSE_ALL: Set to "1", "true", or "yes" to enable
    verbose_all = os.environ.get("DEGENBOT_VERBOSE_ALL", "").lower()
    if verbose_all in {"1", "true", "yes"}:
        VerboseConfig.toggle_all(enabled=True)

    # DEGENBOT_VERBOSE_USERS: Comma-separated list of addresses
    verbose_users = os.environ.get("DEGENBOT_VERBOSE_USERS", "")
    if verbose_users:
        for addr in verbose_users.split(","):
            addr_ = addr.strip()
            if addr_:
                VerboseConfig.add_user(get_checksum_address(addr_))

    # DEGENBOT_VERBOSE_TX: Comma-separated list of transaction hashes
    verbose_tx = os.environ.get("DEGENBOT_VERBOSE_TX", "")
    if verbose_tx:
        for tx_hash in verbose_tx.split(","):
            tx_hash_ = tx_hash.strip()
            if tx_hash_:
                VerboseConfig.add_transaction(HexBytes(tx_hash_))


# Initialize from environment on module load
_init_verbose_config_from_env()


class AaveV3Event(Enum):
    SCALED_TOKEN_MINT = HexBytes(
        "0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196"
    )
    SCALED_TOKEN_BURN = HexBytes(
        "0x4cf25bc1d991c17529c25213d3cc0cda295eeaad5f13f361969b12ea48015f90"
    )
    SCALED_TOKEN_BALANCE_TRANSFER = HexBytes(
        "0x4beccb90f994c31aced7a23b5611020728a23d8ec5cddd1a3e9d97b96fda8666"
    )
    RESERVE_DATA_UPDATED = HexBytes(
        "0x804c9b842b2748a22bb64b345453a3de7ca54a6ca45ce00d415894979e22897a"
    )
    RESERVE_INITIALIZED = HexBytes(
        "0x3a0ca721fc364424566385a1aa271ed508cc2c0949c2272575fb3013a163a45f"
    )
    USER_E_MODE_SET = HexBytes("0xd728da875fc88944cbf17638bcbe4af0eedaef63becd1d1c57cc097eb4608d84")
    POOL_CONFIGURATOR_UPDATED = HexBytes(
        "0x8932892569eba59c8382a089d9b732d1f49272878775235761a2a6b0309cd465"
    )
    POOL_DATA_PROVIDER_UPDATED = HexBytes(
        "0xc853974cfbf81487a14a23565917bee63f527853bcb5fa54f2ae1cdf8a38356d"
    )
    POOL_UPDATED = HexBytes("0x90affc163f1a2dfedcd36aa02ed992eeeba8100a4014f0b4cdc20ea265a66627")
    UPGRADED = HexBytes("0xbc7cd75a20ee27fd9adebab32041f755214dbc6bffa90cc0225b39da2e5c2d3b")
    DISCOUNT_RATE_STRATEGY_UPDATED = HexBytes(
        "0x194bd59f47b230edccccc2be58b92dde3a5dadd835751a621af59006928bccef"
    )
    DISCOUNT_TOKEN_UPDATED = HexBytes(
        "0x6b489e1dbfbe36f55c511c098bcc9d92fec7f04f74ceb75018697ab68f7d3529"
    )
    DISCOUNT_PERCENT_UPDATED = HexBytes(
        "0x74ab9665e7c36c29ddb78ef88a3e2eac73d35b8b16de7bc573e313e320104956"
    )
    STAKED = HexBytes("0x6c86f3fd5118b3aa8bb4f389a617046de0a3d3d477de1a1673d227f802f616dc")
    REDEEM = HexBytes("0x3f693fff038bb8a046aa76d9516190ac7444f7d69cf952c4cbdc086fdef2d6fc")
    TRANSFER = HexBytes("0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef")
    SLASHED = HexBytes("0x4ed05e9673c26d2ed44f7ef6a7f2942df0ee3b5e1e17db4b99f9dcd261a339cd")
    PROXY_CREATED = HexBytes("0x4a465a9bd819d9662563c1e11ae958f8109e437e7f4bf1c6ef0b9a7b3f35d478")


class PercentageMathLibrary(Protocol):
    def percent_div(self, value: int, percentage: int) -> int: ...
    def percent_mul(self, value: int, percentage: int) -> int: ...


class WadRayMathLibrary(Protocol):
    def ray_div(self, a: int, b: int) -> int: ...
    def ray_mul(self, a: int, b: int) -> int: ...


class MathLibraries(TypedDict):
    wad_ray: WadRayMathLibrary
    percentage: PercentageMathLibrary


SCALED_TOKEN_REVISION_LIBRARIES: dict[TokenRevision, MathLibraries] = {
    1: MathLibraries(
        wad_ray=aave_library_v3_1.wad_ray_math,
        percentage=aave_library_v3_1.percentage_math,
    ),
    2: MathLibraries(
        wad_ray=aave_library_v3_2.wad_ray_math,
        percentage=aave_library_v3_2.percentage_math,
    ),
    3: MathLibraries(
        wad_ray=aave_library_v3_3.wad_ray_math,
        percentage=aave_library_v3_3.percentage_math,
    ),
    4: MathLibraries(
        wad_ray=aave_library_v3_4.wad_ray_math,
        percentage=aave_library_v3_4.percentage_math,
    ),
}


def _decode_address(input_: bytes) -> ChecksumAddress:
    """
    Get the checksummed address from the given byte stream.
    """

    (address,) = eth_abi.abi.decode(types=["address"], data=input_)
    return get_checksum_address(address)


def _decode_uint_values(
    event: LogReceipt,
    num_values: int | None = None,
) -> tuple[int, ...]:
    """
    Decode uint256 values from event data.
    """

    if num_values is None:
        num_values = len(event["data"]) // 32
    types = ["uint256"] * num_values
    return eth_abi.abi.decode(types=types, data=event["data"])


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

    w3 = get_web3_from_config(chain_id)

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
            select(AaveV3MarketTable).where(
                AaveV3MarketTable.chain_id == chain_id,
                AaveV3MarketTable.name == market_name,
            )
        )

        if market is not None:
            market.active = True
        else:
            market = AaveV3MarketTable(
                chain_id=chain_id,
                name=market_name,
                active=True,
                # The pool address provider was deployed on block 16,291,071 by TX
                # 0x75fb6e6be55226712f896ae81bbfc86005b2521adb7555d28ce6fe8ab495ef73
                last_update_block=16_291_070,
            )
            session.add(market)

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
                    select(AaveGhoTokenTable).where(AaveGhoTokenTable.token_id == gho_token.id)
                )
            ) is None:
                gho_entry = AaveGhoTokenTable(token_id=gho_token.id)
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
            select(AaveV3MarketTable).where(
                AaveV3MarketTable.chain_id == chain_id,
                AaveV3MarketTable.name == market_name,
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
    "--verify-strict",
    "verify_strict",
    is_flag=True,
    default=False,
    show_default=True,
    help="Verify position and discount amounts on every block boundary.",
)
@click.option(
    "--verify-chunk",
    "verify_chunk",
    is_flag=True,
    default=False,
    show_default=True,
    help="Verify position and discount amounts only at the end of each chunk.",
)
@click.option(
    "--stop-after-one-chunk",
    "stop_after_one_chunk",
    is_flag=True,
    default=False,
    show_default=True,
    help="Stop processing after the first chunk.",
)
@click.option(
    "--no-progress",
    "no_progress",
    is_flag=True,
    default=False,
    show_default=True,
    help="Disable progress bars.",
)
def aave_update(
    *,
    chunk_size: int,
    to_block: str,
    verify_strict: bool,
    verify_chunk: bool,
    stop_after_one_chunk: bool,
    no_progress: bool,
) -> None:
    """
    Update positions for active Aave markets.

    Processes blockchain events from the last updated block to the specified block,
    updating all user positions, interest rates, and indices in the database.

    Args:
        chunk_size: Maximum number of blocks to process before committing changes.
        to_block: Target block identifier (e.g., 'latest', 'latest:-64', 'finalized:128').
        verify_strict: If True, verify position balances at every block boundary.
        verify_chunk: If True, verify position balances only at chunk boundaries.
        stop_after_one_chunk: If True, stop after processing the first chunk.
        no_progress: If True, disable progress bars.
    """

    with db_session() as session:
        active_chains = set(
            session.scalars(
                select(AaveV3MarketTable.chain_id).where(
                    AaveV3MarketTable.active,
                    AaveV3MarketTable.name.contains("aave"),
                )
            ).all()
        )

        for chain_id in active_chains:
            w3 = get_web3_from_config(chain_id)

            active_markets = session.scalars(
                select(AaveV3MarketTable).where(
                    AaveV3MarketTable.active,
                    AaveV3MarketTable.chain_id == chain_id,
                    AaveV3MarketTable.name.contains("aave"),
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

            if initial_start_block >= last_block:
                click.echo(f"Chain {chain_id} has not advanced since the last update.")
                continue

            block_pbar = tqdm.tqdm(
                total=last_block - initial_start_block + 1,
                bar_format="{desc} {percentage:3.1f}% |{bar}|",
                leave=False,
                disable=no_progress,
            )

            block_pbar.n = working_start_block - initial_start_block

            markets_to_update: set[AaveV3MarketTable] = set()

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
                    try:
                        update_aave_market(
                            w3=w3,
                            start_block=working_start_block,
                            end_block=working_end_block,
                            market=market,
                            session=session,
                            verify_strict=verify_strict,
                            verify_chunk=verify_chunk,
                            no_progress=no_progress,
                        )
                    except Exception as e:  # noqa: BLE001
                        logger.info(f"Processing failed on event: {event_in_process}")
                        logger.info("")
                        logger.info("")
                        logger.info("")
                        logger.exception(e)
                        logger.info("")
                        logger.info("")
                        logger.info("")
                        return

                # At this point, all markets have been updated and the invariant checks have
                # passed, so stamp the update block and commit to the DB
                for market in markets_to_update:
                    market.last_update_block = working_end_block
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
    market: AaveV3MarketTable,
    session: Session,
) -> None:
    """
    Process a ReserveInitialized event to add a new Aave asset to the database.
    """

    # EVENT DEFINITION
    # event ReserveInitialized(
    #     address indexed asset,
    #     address indexed aToken,
    #     address stableDebtToken,
    #     address variableDebtToken,
    #     address interestRateStrategyAddress
    # );

    asset_address = _decode_address(event["topics"][1])
    a_token_address = _decode_address(event["topics"][2])

    # Note: stableDebtToken is deprecated in Aave V3, so is ignored
    (_, v_token_address, _) = eth_abi.abi.decode(
        types=["address", "address", "address"], data=event["data"]
    )
    v_token_address = get_checksum_address(v_token_address)

    erc20_token_in_db = _get_or_create_erc20_token(
        session=session,
        chain_id=market.chain_id,
        token_address=asset_address,
    )

    if (
        a_token := session.scalar(
            select(Erc20TokenTable).where(
                Erc20TokenTable.chain == market.chain_id,
                Erc20TokenTable.address == a_token_address,
            )
        )
    ) is None:
        a_token = Erc20TokenTable(
            chain=market.chain_id,
            address=a_token_address,
        )
        session.add(a_token)
        session.flush()

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
        AaveV3AssetsTable(
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


def _get_contract_update_events(
    w3: Web3,
    start_block: int,
    end_block: int,
    address: ChecksumAddress,
) -> list[LogReceipt]:
    """
    Retrieve all `PoolConfiguratorUpdated`, `PoolUpdated`, and `PoolDataProviderUpdated` events for
    the given range
    """

    return fetch_logs_retrying(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=[address],
        topic_signature=[
            [
                AaveV3Event.POOL_CONFIGURATOR_UPDATED.value,
                AaveV3Event.POOL_DATA_PROVIDER_UPDATED.value,
                AaveV3Event.POOL_UPDATED.value,
            ],
        ],
    )


def _get_reserve_initialized_events(
    w3: Web3,
    start_block: int,
    end_block: int,
    address: ChecksumAddress,
) -> list[LogReceipt]:
    """
    Retrieve all `ReserveInitialized` events for the given range.
    """

    return fetch_logs_retrying(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=[address],
        topic_signature=[
            # matches topic0 on `ReserveInitialized`
            [AaveV3Event.RESERVE_INITIALIZED.value],
        ],
    )


def _process_user_e_mode_set_event(
    context: EventHandlerContext,
) -> None:
    """
    Process a UserEModeSet event to update a user's E-Mode category.
    """

    # EVENT DEFINITION
    # event UserEModeSet(
    #     address indexed user,
    #     uint8 categoryId
    # );

    user_address = _decode_address(context.event["topics"][1])

    (e_mode,) = eth_abi.abi.decode(types=["uint8"], data=context.event["data"])

    user = _get_or_create_user(
        session=context.session,
        market=context.market,
        user_address=user_address,
    )
    user.e_mode = e_mode


def _process_discount_token_updated_event(
    context: EventHandlerContext,
) -> None:
    """
    Process a DiscountTokenUpdated event to set the GHO vToken discount token
    """

    # EVENT DEFINITION
    # event DiscountTokenUpdated(
    #     address indexed oldDiscountToken,
    #     address indexed newDiscountToken
    # );

    old_discount_token_address = _decode_address(context.event["topics"][1])
    new_discount_token_address = _decode_address(context.event["topics"][2])

    gho_asset = _get_gho_asset(session=context.session, market=context.market)
    gho_asset.v_gho_discount_token = new_discount_token_address

    logger.info(
        f"SET NEW DISCOUNT TOKEN: {old_discount_token_address} -> {new_discount_token_address}"
    )


def _process_discount_rate_strategy_updated_event(
    context: EventHandlerContext,
) -> None:
    """
    Process a DiscountRateStrategyUpdated event to set the GHO vToken attribute
    """

    # EVENT DEFINITION
    # event DiscountRateStrategyUpdated(
    #     address indexed oldDiscountRateStrategy,
    #     address indexed newDiscountRateStrategy
    # );
    old_discount_rate_strategy_address = _decode_address(context.event["topics"][1])
    new_discount_rate_strategy_address = _decode_address(context.event["topics"][2])

    gho_asset = _get_gho_asset(session=context.session, market=context.market)
    gho_asset.v_gho_discount_rate_strategy = new_discount_rate_strategy_address

    logger.info(
        f"SET NEW DISCOUNT RATE STRATEGY: {old_discount_rate_strategy_address} -> "
        f"{new_discount_rate_strategy_address}"
    )


def _get_or_init_stk_aave_balance(
    user: AaveV3UsersTable,
    discount_token: ChecksumAddress,
    block_number: int,
    w3: Web3,
) -> int:
    """
    Get user's last-known stkAAVE balance.

    If the balance is unknown, perform a contract call at the previous block to ensure
    the balance check is done before any events in the current block are processed.
    """

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

    return user.stk_aave_balance


def _process_stk_aave_staked_event(context: EventHandlerContext) -> None:
    """
    Process a Staked event on the stkAAVE token.

    EVENT DEFINITION
    # event Staked(
    #     address indexed from,
    #     address indexed to,
    #     uint256 assets,
    #     uint256 shares
    # );
    """
    to_address = _decode_address(context.event["topics"][2])
    assets, shares = _decode_uint_values(event=context.event, num_values=2)

    to_user = _get_or_create_user(
        session=context.session, market=context.market, user_address=to_address
    )

    if to_user.stk_aave_balance is None:
        # Balance will be lazy-loaded when needed by GHO processors
        to_user.stk_aave_balance = shares
    else:
        to_user.stk_aave_balance += shares

    if VerboseConfig.is_verbose(
        user_address=to_address, tx_hash=context.event.get("transactionHash")
    ):
        logger.info(f"stkAAVE Staked: {to_address}")
        logger.info(f"  assets: {assets}")
        logger.info(f"  shares: {shares}")
        logger.info(f"  new balance: {to_user.stk_aave_balance}")


def _process_stk_aave_redeem_event(context: EventHandlerContext) -> None:
    """
    Process a Redeem event on the stkAAVE token.

    EVENT DEFINITION
    # event Redeem(
    #     address indexed from,
    #     address indexed to,
    #     uint256 assets,
    #     uint256 shares
    # );
    """

    from_address = _decode_address(context.event["topics"][1])
    assets, shares = _decode_uint_values(event=context.event, num_values=2)

    from_user = context.session.scalar(
        select(AaveV3UsersTable).where(
            AaveV3UsersTable.address == from_address,
            AaveV3UsersTable.market_id == context.market.id,
        )
    )

    if from_user is not None and from_user.stk_aave_balance is not None:
        from_user.stk_aave_balance -= shares
        assert from_user.stk_aave_balance >= 0

        if VerboseConfig.is_verbose(
            user_address=from_address, tx_hash=context.event.get("transactionHash")
        ):
            logger.info(f"stkAAVE Redeem: {from_address}")
            logger.info(f"  assets: {assets}")
            logger.info(f"  shares: {shares}")
            logger.info(f"  new balance: {from_user.stk_aave_balance}")


def _process_stk_aave_transfer_event(context: EventHandlerContext) -> None:
    """
    Process a Transfer event on the stkAAVE token.

    EVENT DEFINITION
    # event Transfer(
    #     address indexed from,
    #     address indexed to,
    #     uint256 value
    # );
    """
    from_address = _decode_address(context.event["topics"][1])
    to_address = _decode_address(context.event["topics"][2])
    (value,) = _decode_uint_values(event=context.event, num_values=1)

    # Update sender balance if user exists and has a balance
    from_user = context.session.scalar(
        select(AaveV3UsersTable).where(
            AaveV3UsersTable.address == from_address,
            AaveV3UsersTable.market_id == context.market.id,
        )
    )
    if from_user is not None and from_user.stk_aave_balance is not None:
        from_user.stk_aave_balance -= value
        assert from_user.stk_aave_balance >= 0

    # Update recipient balance
    to_user = _get_or_create_user(
        session=context.session, market=context.market, user_address=to_address
    )
    if to_user.stk_aave_balance is None:
        to_user.stk_aave_balance = value
    else:
        to_user.stk_aave_balance += value

    if VerboseConfig.is_verbose(
        user_address=from_address, tx_hash=context.event.get("transactionHash")
    ) or VerboseConfig.is_verbose(
        user_address=to_address, tx_hash=context.event.get("transactionHash")
    ):
        logger.info(f"stkAAVE Transfer: {from_address} -> {to_address}")
        logger.info(f"  value: {value}")
        if from_user is not None:
            logger.info(f"  sender new balance: {from_user.stk_aave_balance}")
        logger.info(f"  recipient new balance: {to_user.stk_aave_balance}")


def _process_stk_aave_slashed_event(context: EventHandlerContext) -> None:
    """
    Process a Slashed event on the stkAAVE token.

    EVENT DEFINITION
    # event Slashed(
    #     address indexed destination,
    #     uint256 amount
    # );
    """
    # Topics[1] is the destination (user being slashed)
    destination_address = _decode_address(context.event["topics"][1])
    (amount,) = _decode_uint_values(event=context.event, num_values=1)

    destination_user = context.session.scalar(
        select(AaveV3UsersTable).where(
            AaveV3UsersTable.address == destination_address,
            AaveV3UsersTable.market_id == context.market.id,
        )
    )

    if destination_user is not None and destination_user.stk_aave_balance is not None:
        destination_user.stk_aave_balance -= amount
        assert destination_user.stk_aave_balance >= 0

        if VerboseConfig.is_verbose(
            user_address=destination_address, tx_hash=context.event.get("transactionHash")
        ):
            logger.info(f"stkAAVE Slashed: {destination_address}")
            logger.info(f"  amount: {amount}")
            logger.info(f"  new balance: {destination_user.stk_aave_balance}")


def _process_reserve_data_update_event(
    context: EventHandlerContext,
) -> None:
    """
    Process a ReserveDataUpdated event to update asset rates and indices.
    """

    # EVENT DEFINITION
    # event ReserveDataUpdated(
    #     address indexed reserve,
    #     uint256 liquidityRate,
    #     uint256 stableBorrowRate,
    #     uint256 variableBorrowRate,
    #     uint256 liquidityIndex,
    #     uint256 variableBorrowIndex
    # );
    reserve_asset_address = _decode_address(context.event["topics"][1])

    asset_in_db = None
    for asset in context.market.assets:
        if asset.underlying_token.address == reserve_asset_address:
            asset_in_db = asset
            break
    assert asset_in_db is not None

    if asset_in_db.last_update_block is not None:
        assert asset_in_db.last_update_block <= context.event["blockNumber"]

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
        data=context.event["data"],
    )

    asset_in_db.liquidity_rate = liquidity_rate
    asset_in_db.borrow_rate = variable_borrow_rate
    asset_in_db.liquidity_index = liquidity_index
    asset_in_db.borrow_index = variable_borrow_index
    asset_in_db.last_update_block = context.event["blockNumber"]


def _process_scaled_token_upgrade_event(
    context: EventHandlerContext,
) -> None:
    """
    Process an Upgraded event to update the aToken or vToken revision.
    """

    # EVENT DEFINITION
    # event Upgraded(
    #     address indexed implementation
    # );

    new_implementation_address = _decode_address(context.event["topics"][1])

    if (
        aave_collateral_asset := _get_asset_by_token_type(
            market=context.market,
            token_address=get_checksum_address(context.event["address"]),
            token_type=TokenType.COLLATERAL,
        )
    ) is not None:
        (atoken_revision,) = raw_call(
            w3=context.w3,
            address=new_implementation_address,
            calldata=encode_function_calldata(
                function_prototype="ATOKEN_REVISION()",
                function_arguments=None,
            ),
            return_types=["uint256"],
        )
        aave_collateral_asset.a_token_revision = atoken_revision
        logger.info(f"Upgraded aToken revision to {atoken_revision}")
    elif (
        aave_debt_asset := _get_asset_by_token_type(
            market=context.market,
            token_address=get_checksum_address(context.event["address"]),
            token_type=TokenType.DEBT,
        )
    ) is not None:
        (vtoken_revision,) = raw_call(
            w3=context.w3,
            address=new_implementation_address,
            calldata=encode_function_calldata(
                function_prototype="DEBT_TOKEN_REVISION()",
                function_arguments=None,
            ),
            return_types=["uint256"],
        )
        aave_debt_asset.v_token_revision = vtoken_revision
        logger.info(f"Upgraded vToken revision to {vtoken_revision}")
    else:
        token_address = get_checksum_address(context.event["address"])
        msg = f"Unknown token type for address {token_address}. Expected aToken or vToken."
        raise ValueError(msg)


def _get_math_libraries(
    token_revision: int,
) -> tuple[WadRayMathLibrary, PercentageMathLibrary]:
    """
    Get both WadRayMath and PercentageMath libraries for the token revision.
    """
    try:
        libs = SCALED_TOKEN_REVISION_LIBRARIES[token_revision]
        return libs["wad_ray"], libs["percentage"]
    except KeyError:
        msg = f"Unsupported revision: {token_revision}"
        raise ValueError(msg) from None


def _get_or_create_user(
    session: Session,
    market: AaveV3MarketTable,
    user_address: ChecksumAddress,
) -> AaveV3UsersTable:
    """
    Get existing user or create new one with default e_mode.
    """

    if (
        user := session.scalar(
            select(AaveV3UsersTable).where(
                AaveV3UsersTable.address == user_address,
                AaveV3UsersTable.market_id == market.id,
            )
        )
    ) is None:
        user = AaveV3UsersTable(
            market_id=market.id,
            address=user_address,
            e_mode=0,
            gho_discount=0,
        )
        session.add(user)
        session.flush()
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


def _get_or_create_collateral_position(
    session: Session,
    user: AaveV3UsersTable,
    asset_id: int,
) -> AaveV3CollateralPositionsTable:
    """
    Get existing collateral position or create new one with zero balance.
    """

    position = None
    for p in user.collateral_positions:
        if p.asset_id == asset_id:
            position = p
            break

    if position is None:
        position = AaveV3CollateralPositionsTable(user_id=user.id, asset_id=asset_id, balance=0)
        session.add(position)
        user.collateral_positions.append(position)
    return position


def _get_or_create_debt_position(
    session: Session,
    user: AaveV3UsersTable,
    asset_id: int,
) -> AaveV3DebtPositionsTable:
    """
    Get existing debt position or create new one with zero balance.
    """

    position = None
    for p in user.debt_positions:
        if p.asset_id == asset_id:
            position = p
            break

    if position is None:
        position = AaveV3DebtPositionsTable(user_id=user.id, asset_id=asset_id, balance=0)
        session.add(position)
        user.debt_positions.append(position)
    return position


def _get_gho_asset(
    session: Session,
    market: AaveV3MarketTable,
) -> AaveGhoTokenTable:
    """
    Get GHO token asset for a given market.

    GHO tokens are chain-unique: multiple Aave markets on the same chain share
    a single GHO token. Query by chain_id to retrieve the shared configuration.
    """
    gho_asset = session.scalar(
        select(AaveGhoTokenTable)
        .join(Erc20TokenTable)
        .where(Erc20TokenTable.chain == market.chain_id)
    )
    if gho_asset is None:
        msg = (
            f"GHO token not found for chain {market.chain_id}. "
            "Ensure that market has been activated."
        )
        raise ValueError(msg)
    return gho_asset


def _get_contract(
    market: AaveV3MarketTable,
    contract_name: str,
) -> AaveV3ContractsTable:
    """
    Get contract by name for a given market.
    """
    for contract in market.contracts:
        if contract.name == contract_name:
            return contract
    msg = f"{contract_name} not found for market {market.id}"
    raise ValueError(msg)


def _get_asset_by_token_type(
    market: AaveV3MarketTable,
    token_address: ChecksumAddress,
    token_type: TokenType,
) -> AaveV3AssetsTable | None:
    """
    Get AaveV3 asset by aToken (collateral) or vToken (debt) address.
    """
    for asset in market.assets:
        if token_type == TokenType.COLLATERAL:
            if asset.a_token.address == token_address:
                return asset
        elif token_type == TokenType.DEBT and asset.v_token.address == token_address:
            return asset
    return None


def _verify_gho_discount_amounts(
    *,
    w3: Web3,
    session: Session,
    market: AaveV3MarketTable,
    users_to_check: dict[ChecksumAddress, int],
    no_progress: bool,
) -> None:
    """
    Verify that the GHO discount values match the contract.
    """

    for user_address, last_update_block in tqdm.tqdm(
        users_to_check.items(),
        desc="Verifying GHO discount amounts",
        leave=False,
        disable=no_progress,
    ):
        user = session.scalar(
            select(AaveV3UsersTable).where(
                AaveV3UsersTable.address == user_address,
                AaveV3UsersTable.market_id == market.id,
            )
        )
        assert user is not None

        (discount_percent,) = raw_call(
            w3=w3,
            address=GHO_VARIABLE_DEBT_TOKEN_ADDRESS,
            calldata=encode_function_calldata(
                function_prototype="getDiscountPercent(address)",
                function_arguments=[user_address],
            ),
            return_types=["uint256"],
            block_identifier=last_update_block,
        )

        assert user.gho_discount == discount_percent, (
            f"User {user_address}: GHO discount {user.gho_discount} "
            f"does not match GHO vDebtToken token contract ({discount_percent}) "
            f"@ {GHO_VARIABLE_DEBT_TOKEN_ADDRESS} at block {last_update_block}"
        )


def _verify_stk_aave_balances(
    *,
    w3: Web3,
    session: Session,
    market: AaveV3MarketTable,
    gho_users_to_check: dict[ChecksumAddress, int],
    no_progress: bool,
) -> None:
    """
    Verify that the tracked stkAAVE balances match the contract.
    """

    gho_asset = _get_gho_asset(session=session, market=market)
    if gho_asset.v_gho_discount_token is None:
        return

    discount_token = gho_asset.v_gho_discount_token

    for user_address, last_update_block in tqdm.tqdm(
        gho_users_to_check.items(),
        desc="Verifying stkAAVE balances",
        leave=False,
        disable=no_progress,
    ):
        user = session.scalar(
            select(AaveV3UsersTable).where(
                AaveV3UsersTable.address == user_address,
                AaveV3UsersTable.market_id == market.id,
            )
        )
        assert user is not None

        # Skip if balance hasn't been initialized yet (lazy loading)
        if user.stk_aave_balance is None:
            continue

        (actual_balance,) = raw_call(
            w3=w3,
            address=discount_token,
            calldata=encode_function_calldata(
                function_prototype="balanceOf(address)",
                function_arguments=[user_address],
            ),
            return_types=["uint256"],
            block_identifier=last_update_block,
        )

        assert user.stk_aave_balance == actual_balance, (
            f"User {user_address}: stkAAVE balance {user.stk_aave_balance} "
            f"does not match contract ({actual_balance}) "
            f"@ {discount_token} at block {last_update_block}"
        )


def _verify_scaled_token_positions(
    *,
    w3: Web3,
    market: AaveV3MarketTable,
    session: Session,
    users_to_check: dict[ChecksumAddress, int],
    position_table: type[AaveV3CollateralPositionsTable | AaveV3DebtPositionsTable],
    no_progress: bool,
) -> None:
    """
    Verify that the database position balances match the contract.
    """

    desc = (
        "Verifying collateral positions"
        if position_table is AaveV3CollateralPositionsTable
        else "Verifying debt positions"
    )
    for user_address, last_update_block in tqdm.tqdm(
        users_to_check.items(),
        desc=desc,
        leave=False,
        disable=no_progress,
    ):
        if user_address == ZERO_ADDRESS:
            logger.error("SKIPPED ZERO ADDRESS!")
            continue

        user = session.scalar(
            select(AaveV3UsersTable).where(
                AaveV3UsersTable.address == user_address,
                AaveV3UsersTable.market_id == market.id,
            )
        )
        assert user is not None, f"Could not identify user {user_address}"

        for position in session.scalars(
            select(position_table).where(position_table.user_id == user.id)
        ):
            position = cast("AaveV3CollateralPositionsTable | AaveV3DebtPositionsTable", position)

            if position_table is AaveV3CollateralPositionsTable:
                token_address = get_checksum_address(position.asset.a_token.address)
            elif position_table is AaveV3DebtPositionsTable:
                token_address = get_checksum_address(position.asset.v_token.address)
            else:
                msg = f"Unknown position table type: {position_table}"
                raise ValueError(msg)

            (actual_scaled_balance,) = raw_call(
                w3=w3,
                address=token_address,
                calldata=encode_function_calldata(
                    function_prototype="scaledBalanceOf(address)",
                    function_arguments=[user_address],
                ),
                return_types=["uint256"],
                block_identifier=last_update_block,
            )

            assert actual_scaled_balance == position.balance, (
                f"User {user_address}: "
                f"{'collateral' if position_table is AaveV3CollateralPositionsTable else 'debt'} "
                f"balance ({position.balance}) does not match scaled token contract "
                f"({actual_scaled_balance}) @ {token_address} at block {last_update_block}"
            )


@dataclass(frozen=True, slots=True)
class CollateralMintEvent:
    value: int
    balance_increase: int
    index: int


@dataclass(frozen=True, slots=True)
class CollateralBurnEvent:
    value: int
    balance_increase: int
    index: int


@dataclass(frozen=True, slots=True)
class DebtMintEvent:
    caller: ChecksumAddress
    on_behalf_of: ChecksumAddress
    value: int
    balance_increase: int
    index: int


@dataclass(frozen=True, slots=True)
class DebtBurnEvent:
    from_: ChecksumAddress
    target: ChecksumAddress
    value: int
    balance_increase: int
    index: int


def _log_token_operation(
    *,
    user_operation: UserOperation,
    user_address: ChecksumAddress,
    token_type: str,
    token_address: ChecksumAddress,
    index: int,
    balance_info: str,
    tx_hash: HexBytes,
    block_info: str,
    balance_delta: int | None = None,
) -> None:
    """
    Log token operation details for verbose output.
    """

    logger.info(user_operation.value)
    logger.info(f"{token_type}  : {token_address}")
    logger.info(f"User    : {user_address}")
    logger.info(f"Index   : {index}")
    logger.info(f"Balance : {balance_info}")
    if balance_delta is not None:
        logger.info(f"Delta   : {balance_delta}")
    logger.info(f"TX      : {tx_hash.to_0x_hex()}")
    logger.info(f"Block   : {block_info}")
    logger.info("")


def _log_balance_transfer(
    *,
    token_address: ChecksumAddress,
    from_address: ChecksumAddress,
    from_balance_info: str,
    to_address: ChecksumAddress,
    to_balance_info: str,
    tx_hash: HexBytes,
    block_info: str,
) -> None:
    """
    Log balance transfer details for verbose output.
    """
    logger.info("BALANCE TRANSFER")
    logger.info(f"aToken  : {token_address}")
    logger.info(f"User    : {from_address}")
    logger.info(f"Balance : {from_balance_info}")
    logger.info(f"User    : {to_address}")
    logger.info(f"Balance : {to_balance_info}")
    logger.info(f"TX      : {tx_hash.to_0x_hex()}")
    logger.info(f"Block   : {block_info}")
    logger.info("")


def _process_scaled_token_operation(
    event: CollateralMintEvent | CollateralBurnEvent | DebtMintEvent | DebtBurnEvent,
    scaled_token_revision: int,
    position: AaveV3CollateralPositionsTable | AaveV3DebtPositionsTable,
) -> UserOperation:
    """
    Determine the user operation for scaled token events and apply balance delta to position.
    """

    ray_math, _ = _get_math_libraries(scaled_token_revision)
    operation: UserOperation

    match event:
        case CollateralMintEvent():
            if event.balance_increase > event.value:
                requested_amount = event.balance_increase - event.value
                balance_delta = -ray_math.ray_div(a=requested_amount, b=event.index)
                operation = UserOperation.WITHDRAW
            else:
                requested_amount = event.value - event.balance_increase
                balance_delta = ray_math.ray_div(a=requested_amount, b=event.index)
                operation = UserOperation.DEPOSIT

        case CollateralBurnEvent():
            requested_amount = event.value + event.balance_increase
            balance_delta = -ray_math.ray_div(a=requested_amount, b=event.index)
            operation = UserOperation.WITHDRAW

        case DebtMintEvent():
            if event.balance_increase > event.value:
                requested_amount = event.balance_increase - event.value
                balance_delta = -ray_math.ray_div(a=requested_amount, b=event.index)
                operation = UserOperation.REPAY
            else:
                requested_amount = event.value - event.balance_increase
                balance_delta = ray_math.ray_div(a=requested_amount, b=event.index)
                operation = UserOperation.BORROW

        case DebtBurnEvent():
            requested_amount = event.value + event.balance_increase
            balance_delta = -ray_math.ray_div(a=requested_amount, b=event.index)
            operation = UserOperation.REPAY

    assert requested_amount >= 0

    position.balance += balance_delta
    position.last_index = event.index

    return operation


def _accrue_debt_on_action(
    *,
    debt_position: AaveV3DebtPositionsTable,
    percentage_math: PercentageMathLibrary,
    wad_ray_math: WadRayMathLibrary,
    previous_scaled_balance: int,
    discount_percent: int,
    index: int,
    token_revision: int,
) -> int:
    """
    Simulate the GhoVariableDebtToken _accrueDebtOnAction function.

    REFERENCE:
    ```
    /**
    * @dev Accumulates debt of the user since last action.
    * @dev It skips applying discount in case there is no balance increase or discount percent is zero.
    * @param user The address of the user
    * @param previousScaledBalance The previous scaled balance of the user
    * @param discountPercent The discount percent
    * @param index The variable debt index of the reserve
    * @return The increase in scaled balance since the last action of `user`
    * @return The discounted amount in scaled balance off the balance increase
    */
    function _accrueDebtOnAction(
        address user,
        uint256 previousScaledBalance,
        uint256 discountPercent,
        uint256 index
    ) internal returns (uint256, uint256) {
        uint256 balanceIncrease = previousScaledBalance.rayMul(index) -
            previousScaledBalance.rayMul(_userState[user].additionalData);

        uint256 discountScaled = 0;
        if (balanceIncrease != 0 && discountPercent != 0) {
            uint256 discount = balanceIncrease.percentMul(discountPercent);
            discountScaled = discount.rayDiv(index);
            balanceIncrease = balanceIncrease - discount;
        }

        _userState[user].additionalData = index.toUint128();

        _ghoUserState[user].accumulatedDebtInterest = (balanceIncrease +
            _ghoUserState[user].accumulatedDebtInterest).toUint128();

        return (balanceIncrease, discountScaled);
    }
    ```
    """

    if token_revision in {1, 2, 3}:
        balance_increase = wad_ray_math.ray_mul(
            a=previous_scaled_balance,
            b=index,
        ) - wad_ray_math.ray_mul(
            a=previous_scaled_balance,
            b=debt_position.last_index or 0,
        )

        discount = 0
        discount_scaled = 0
        if balance_increase != 0 and discount_percent != 0:
            discount = percentage_math.percent_mul(
                value=balance_increase,
                percentage=discount_percent,
            )
            discount_scaled = wad_ray_math.ray_div(a=discount, b=index)
            balance_increase -= discount

            if VerboseConfig.is_verbose(
                tx_hash=event_in_process.get("transactionHash") if event_in_process else None,
            ):
                logger.info("_accrue_debt_on_action:")
                logger.info(f"  previous_scaled_balance={previous_scaled_balance}")
                logger.info(f"  last_index={debt_position.last_index}")
                logger.info(f"  current_index={index}")
                logger.info(f"  balance_increase={balance_increase + discount}")
                logger.info(f"  discount_percent={discount_percent}")
                logger.info(f"  discount={discount}")
                logger.info(f"  discount_scaled={discount_scaled}")

    else:
        msg = f"Unsupported token revision {token_revision}"
        raise ValueError(msg)

    return discount_scaled


def _get_discount_rate(
    w3: Web3,
    discount_rate_strategy: ChecksumAddress,
    debt_token_balance: int,
    discount_token_balance: int,
) -> int:
    """
    Get the discount percentage from the discount rate strategy contract.

    Calls calculateDiscountRate on the strategy contract with debt and discount token balances
    to determine the user's interest discount percentage.
    """
    new_discount_percentage: int
    (new_discount_percentage,) = raw_call(
        w3=w3,
        address=discount_rate_strategy,
        calldata=encode_function_calldata(
            function_prototype="calculateDiscountRate(uint256,uint256)",
            function_arguments=[debt_token_balance, discount_token_balance],
        ),
        return_types=["uint256"],
    )

    return new_discount_percentage


def _refresh_discount_rate(
    w3: Web3,
    user: AaveV3UsersTable,
    discount_rate_strategy: ChecksumAddress,
    discount_token_balance: int,
    scaled_debt_balance: int,
    debt_index: int,
    wad_ray_math: WadRayMathLibrary,
) -> None:
    """
    Calculate and update the user's GHO discount rate.

    Calculates the debt token balance from scaled balance and index, then
    fetches and applies the new discount rate from the strategy contract.
    """

    debt_token_balance = wad_ray_math.ray_mul(
        a=scaled_debt_balance,
        b=debt_index,
    )
    user.gho_discount = _get_discount_rate(
        w3=w3,
        discount_rate_strategy=discount_rate_strategy,
        debt_token_balance=debt_token_balance,
        discount_token_balance=discount_token_balance,
    )


def _get_discounted_balance(
    scaled_balance: int,
    previous_index: int,
    current_index: int,
    user: AaveV3UsersTable,
    ray_math_module: WadRayMathLibrary,
    percentage_math: PercentageMathLibrary,
    discount_percent: int | None = None,
) -> int:
    """
    Get the discounted balance for the user.

    This effectively replicates the `super.BalanceOf(user)` call used in the `_burnScaled`
    and `_mintScaled` function calls in GhoVariableDebtToken.sol (version 2).

    Ref: 0x7aa606b1B341fFEeAfAdbbE4A2992EFB35972775 (mainnet)
    """

    if scaled_balance == 0:
        return 0

    # index = POOL.getReserveNormalizedVariableDebt(_underlyingAsset); #noqa:ERA001
    # replaced by `current_index` argument

    # previousIndex = _userState[user].additionalData; #noqa:ERA001
    # replaced by `previous_index` argument

    # uint256 balance = scaledBalance.rayMul(index);
    balance = ray_math_module.ray_mul(
        a=scaled_balance,
        b=current_index,
    )

    if current_index == previous_index:
        return balance

    discount_percentage = discount_percent if discount_percent is not None else user.gho_discount

    if discount_percentage != 0:
        # uint256 balanceIncrease = balance - scaledBalance.rayMul(previousIndex);
        balance_increase = balance - ray_math_module.ray_mul(
            a=scaled_balance,
            b=previous_index,
        )

        balance -= percentage_math.percent_mul(
            value=balance_increase,
            percentage=discount_percentage,
        )

    return balance


def _process_gho_debt_burn(
    *,
    w3: Web3,
    discount_token: ChecksumAddress,
    discount_rate_strategy: ChecksumAddress,
    event_data: DebtBurnEvent,
    user: AaveV3UsersTable,
    scaled_token_revision: int,
    debt_position: AaveV3DebtPositionsTable,
    state_block: int,
    event: LogReceipt,
    tx_discount_overrides: dict[tuple[HexBytes, ChecksumAddress], int],
    tx_discount_updated_users: set[ChecksumAddress],
) -> UserOperation:
    """
    Determine the user operation that triggered a GHO vToken Burn event and apply balance delta.
    """

    wad_ray_math_library, percentage_math_library = _get_math_libraries(scaled_token_revision)
    # Get the effective discount percent for this transaction
    # Use the override if available (set by DiscountPercentUpdated event in same tx),
    # otherwise use the user's current discount
    tx_hash = event.get("transactionHash") if event else None
    effective_discount = (
        tx_discount_overrides.get((tx_hash, user.address), user.gho_discount)
        if tx_hash
        else user.gho_discount
    )

    if scaled_token_revision == 1:
        # uint256 amountToBurn = amount - balanceIncrease;
        requested_amount = event_data.value + event_data.balance_increase

        # uint256 amountScaled = amount.rayDiv(index);
        amount_scaled = wad_ray_math_library.ray_div(
            a=requested_amount,
            b=event_data.index,
        )

        # uint256 previousScaledBalance = super.balanceOf(user);
        previous_scaled_balance = debt_position.balance

        # uint256 discountPercent = _ghoUserState[user].discountPercent;
        # (available from `user`)

        # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(...)
        discount_scaled = _accrue_debt_on_action(
            debt_position=debt_position,
            percentage_math=percentage_math_library,
            wad_ray_math=wad_ray_math_library,
            previous_scaled_balance=previous_scaled_balance,
            discount_percent=effective_discount,
            index=event_data.index,
            token_revision=scaled_token_revision,
        )

        # _burn(user, (amountScaled + discountScaled).toUint128()); #noqa:ERA001
        balance_delta = -(amount_scaled + discount_scaled)

        # Update the discount percentage for the new balance
        # Skip if discount was already updated via DiscountPercentUpdated event in this tx
        if user.address not in tx_discount_updated_users:
            discount_token_balance = _get_or_init_stk_aave_balance(
                user=user,
                discount_token=discount_token,
                block_number=state_block,
                w3=w3,
            )
            _refresh_discount_rate(
                w3=w3,
                user=user,
                discount_rate_strategy=discount_rate_strategy,
                discount_token_balance=discount_token_balance,
                scaled_debt_balance=debt_position.balance + balance_delta,
                debt_index=event_data.index,
                wad_ray_math=wad_ray_math_library,
            )

    elif scaled_token_revision in {2, 3}:
        # uint256 amountToBurn = amount - balanceIncrease;
        requested_amount = event_data.value + event_data.balance_increase

        # uint256 amountScaled = amount.rayDiv(index);
        amount_scaled = wad_ray_math_library.ray_div(
            a=requested_amount,
            b=event_data.index,
        )

        # uint256 previousScaledBalance = super.balanceOf(user);
        previous_scaled_balance = debt_position.balance
        previous_index = debt_position.last_index or 0

        # uint256 balanceBeforeBurn = balanceOf(user);
        balance_before_burn = _get_discounted_balance(
            scaled_balance=previous_scaled_balance,
            previous_index=previous_index,
            current_index=event_data.index,
            user=user,
            ray_math_module=wad_ray_math_library,
            percentage_math=percentage_math_library,
            discount_percent=effective_discount,
        )

        # uint256 discountPercent = _ghoUserState[user].discountPercent;
        # (available from `user`)

        # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(...)
        discount_scaled = _accrue_debt_on_action(
            debt_position=debt_position,
            percentage_math=percentage_math_library,
            wad_ray_math=wad_ray_math_library,
            previous_scaled_balance=previous_scaled_balance,
            discount_percent=effective_discount,
            index=event_data.index,
            token_revision=scaled_token_revision,
        )

        if requested_amount == balance_before_burn:
            # _burn(user, previousScaledBalance.toUint128()); # noqa:ERA001
            balance_delta = -previous_scaled_balance
        else:
            # _burn(user, (amountScaled + discountScaled).toUint128()); # noqa:ERA001
            balance_delta = -(amount_scaled + discount_scaled)

        # Update the discount percentage for the new balance
        # Skip if discount was already updated via DiscountPercentUpdated event in this tx
        if user.address not in tx_discount_updated_users:
            discount_token_balance = _get_or_init_stk_aave_balance(
                user=user,
                discount_token=discount_token,
                block_number=state_block,
                w3=w3,
            )
            _refresh_discount_rate(
                w3=w3,
                user=user,
                discount_rate_strategy=discount_rate_strategy,
                discount_token_balance=discount_token_balance,
                scaled_debt_balance=debt_position.balance + balance_delta,
                debt_index=event_data.index,
                wad_ray_math=wad_ray_math_library,
            )

        if VerboseConfig.is_verbose(
            user_address=user.address, tx_hash=event_in_process["transactionHash"]
        ):
            logger.info("_burnScaled (vGHO version 2)")
            logger.info(f"{previous_scaled_balance=}")
            logger.info(f"{amount_scaled=}")
            logger.info(f"{requested_amount=}")
            logger.info(f"{balance_before_burn=}")
            logger.info(f"{discount_scaled=}")
            logger.info(f"{user.gho_discount=}")

    else:
        msg = f"Unknown token revision: {scaled_token_revision}"
        raise ValueError(msg)

    if VerboseConfig.is_verbose(
        user_address=user.address, tx_hash=event_in_process["transactionHash"]
    ):
        logger.info(f"{debt_position.balance=}")
        logger.info(f"{debt_position.balance + balance_delta=}")
        logger.info(f"{event_in_process=}")
        logger.info(f"{user.address=}")
        logger.info(f"{user.gho_discount=}")
        logger.info(f"{discount_scaled=}")
        logger.info(f"{balance_delta=}")
        logger.info(f"{discount_token=}")
        logger.info(f"{discount_rate_strategy=}")
        logger.info(f"{state_block=}")

    assert requested_amount >= 0
    assert debt_position.balance + balance_delta >= 0, (
        f"{debt_position.balance} + {balance_delta} < 0!"
    )

    # Update the debt position
    debt_position.balance += balance_delta
    debt_position.last_index = event_data.index

    return UserOperation.GHO_REPAY


def _process_staked_aave_event(
    *,
    w3: Web3,
    market: AaveV3MarketTable,
    session: Session,
    discount_token: ChecksumAddress,
    discount_rate_strategy: ChecksumAddress,
    event_data: DebtMintEvent,
    user: AaveV3UsersTable,
    scaled_token_revision: int,
    debt_position: AaveV3DebtPositionsTable,
    state_block: int,
    event: LogReceipt,
    cache: BlockStateCache,
    tx_discount_overrides: dict[tuple[HexBytes, ChecksumAddress], int],
    tx_discount_updated_users: set[ChecksumAddress],
) -> UserOperation | None:
    """
    Process a GHO vToken Mint event triggered by an AAVE staking event or stkAAVE transfer.

    This occurs when updateDiscountDistribution is triggered externally, resulting in a Mint event
    where value equals balanceIncrease.
    """

    if VerboseConfig.is_verbose(
        user_address=user.address, tx_hash=event_in_process["transactionHash"]
    ):
        logger.info("_process_staked_aave_event")
        # logger.info(f"{event=}")

    # This condition occurs when updateDiscountDistribution is triggered by an AAVE staking
    # event (Staked/Redeem), or when a stkAAVE token balance is transferred. The amount given to
    # updateDiscountDistribution cannot be reversed from the GHO VariableDebtToken Mint
    # event, so get it from the relevant accessory event.
    accessory_events = [
        e
        for e in fetch_logs_retrying(
            w3=w3,
            start_block=state_block,
            end_block=state_block,
            address=[discount_token],
            topic_signature=[
                [
                    AaveV3Event.STAKED.value,
                    AaveV3Event.REDEEM.value,
                    AaveV3Event.TRANSFER.value,
                ],
            ],
        )
        if e["transactionHash"] == event["transactionHash"]
        if e["topics"][0] in {AaveV3Event.STAKED.value, AaveV3Event.REDEEM.value}
        or (
            not cache.is_transfer_processed(e["logIndex"])
            and (
                # For transfers, only include if it involves the Mint event user
                _decode_address(e["topics"][1]) == user.address  # from
                or _decode_address(e["topics"][2]) == user.address  # to
            )
        )
    ]
    if not accessory_events:
        # All accessory events were filtered out (e.g., transfers already processed)
        # Fall back to standard mint processing
        return None

    if len(accessory_events) > 1:
        # Some transactions emit multiple trigger events, so place the useful ones first
        # Ref: TX 0x818bc84e89fea83f4d53a8dda5c5b84691a6557d47153320021e0d0f9539de9a
        event_priority = {
            AaveV3Event.STAKED.value: 0,
            AaveV3Event.REDEEM.value: 0,
            AaveV3Event.TRANSFER.value: 1,
        }
        accessory_events.sort(key=lambda event: event_priority[event["topics"][0]])

    discount_token_info_event, *_ = accessory_events

    match discount_token_info_event["topics"][0]:
        case AaveV3Event.STAKED.value:
            return _process_aave_stake(
                w3=w3,
                discount_token=discount_token,
                discount_rate_strategy=discount_rate_strategy,
                event_data=event_data,
                recipient=user,
                scaled_token_revision=scaled_token_revision,
                debt_position=debt_position,
                triggering_event=discount_token_info_event,
                tx_discount_overrides=tx_discount_overrides,
                tx_discount_updated_users=tx_discount_updated_users,
            )
        case AaveV3Event.REDEEM.value:
            return _process_aave_redeem(
                w3=w3,
                discount_token=discount_token,
                discount_rate_strategy=discount_rate_strategy,
                event_data=event_data,
                sender=user,
                scaled_token_revision=scaled_token_revision,
                debt_position=debt_position,
                triggering_event=discount_token_info_event,
                tx_discount_overrides=tx_discount_overrides,
                tx_discount_updated_users=tx_discount_updated_users,
            )
        case AaveV3Event.TRANSFER.value:
            # Mark this transfer as processed so subsequent Mint events in the same
            # transaction use the next unprocessed transfer
            cache.mark_transfer_processed(discount_token_info_event["logIndex"])
            return _process_staked_aave_transfer(
                w3=w3,
                market=market,
                session=session,
                discount_token=discount_token,
                discount_rate_strategy=discount_rate_strategy,
                event_data=event_data,
                scaled_token_revision=scaled_token_revision,
                debt_position=debt_position,
                triggering_event=discount_token_info_event,
                cache=cache,
                tx_discount_overrides=tx_discount_overrides,
                tx_discount_updated_users=tx_discount_updated_users,
            )
        case _:
            msg = "Should be unreachable"
            raise ValueError(msg)


def _process_aave_stake(
    *,
    w3: Web3,
    discount_token: ChecksumAddress,
    discount_rate_strategy: ChecksumAddress,
    event_data: DebtMintEvent,
    recipient: AaveV3UsersTable,
    scaled_token_revision: int,
    debt_position: AaveV3DebtPositionsTable,
    triggering_event: LogReceipt,
    tx_discount_overrides: dict[tuple[HexBytes, ChecksumAddress], int],
    tx_discount_updated_users: set[ChecksumAddress],
) -> UserOperation:
    """
    Process a GHO vToken Mint event triggered by an AAVE staking event.

    This handles the discount distribution update when a user stakes AAVE tokens.
    """

    operation: UserOperation = UserOperation.AAVE_STAKED

    wad_ray_math_library, percentage_math_library = _get_math_libraries(scaled_token_revision)

    # EVENT DEFINITION
    # event Staked(address indexed from, address indexed to, uint256 assets, uint256 shares)
    # event Redeem(address indexed from, address indexed to, uint256 assets, uint256 shares)
    assets, shares = _decode_uint_values(
        event=triggering_event,
        num_values=2,
    )
    assert assets == shares
    requested_amount = assets

    from_address = event_data.caller
    assert from_address == ZERO_ADDRESS, f"{event_data=}"
    assert recipient.address == event_data.on_behalf_of

    # For staking/redemption, the recipient is the user staking/redeeming
    recipient_debt_position = debt_position

    if VerboseConfig.is_verbose(
        user_address=recipient.address, tx_hash=event_in_process["transactionHash"]
    ):
        logger.info(f"{event_data.caller=}")
        logger.info(f"{event_data.on_behalf_of=}")

    # uint256 recipientPreviousScaledBalance = super.balanceOf(recipient)
    recipient_previous_scaled_balance = recipient_debt_position.balance

    if recipient_previous_scaled_balance > 0:
        if VerboseConfig.is_verbose(
            user_address=recipient.address, tx_hash=event_in_process["transactionHash"]
        ):
            logger.info(f"{recipient_previous_scaled_balance=}")
            logger.info("Processing case: recipientPreviousScaledBalance > 0")

        # Get the effective discount percent for this transaction
        # Use the override if available (set by DiscountPercentUpdated event in same tx),
        # otherwise use the user's current discount
        tx_hash = triggering_event.get("transactionHash")
        effective_discount = (
            tx_discount_overrides.get((tx_hash, recipient.address), recipient.gho_discount)
            if tx_hash
            else recipient.gho_discount
        )

        # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(...)
        recipient_discount_scaled = _accrue_debt_on_action(
            debt_position=recipient_debt_position,
            percentage_math=percentage_math_library,
            wad_ray_math=wad_ray_math_library,
            previous_scaled_balance=recipient_previous_scaled_balance,
            discount_percent=effective_discount,
            index=event_data.index,
            token_revision=scaled_token_revision,
        )

        # _burn(recipient, discountScaled.toUint128())
        recipient_debt_position.balance -= recipient_discount_scaled
        recipient_new_scaled_balance = recipient_debt_position.balance
        recipient_debt_position.last_index = event_data.index

        # Update the discount percentage for the new balance
        recipient_previous_discount_percent = recipient.gho_discount
        # Skip if discount was already updated via DiscountPercentUpdated event in this tx
        if recipient.address not in tx_discount_updated_users:
            recipient_new_discount_token_balance = _get_or_init_stk_aave_balance(
                user=recipient,
                discount_token=discount_token,
                block_number=triggering_event["blockNumber"],
                w3=w3,
            )
            _refresh_discount_rate(
                w3=w3,
                user=recipient,
                discount_rate_strategy=discount_rate_strategy,
                discount_token_balance=recipient_new_discount_token_balance,
                scaled_debt_balance=recipient_debt_position.balance,
                debt_index=event_data.index,
                wad_ray_math=wad_ray_math_library,
            )
        recipient_new_discount_percent = recipient.gho_discount

        if VerboseConfig.is_verbose(
            user_address=recipient.address, tx_hash=event_in_process["transactionHash"]
        ):
            logger.info(f"{recipient.address=}")
            logger.info(f"{requested_amount=}")
            logger.info(f"{recipient_previous_scaled_balance=}")
            logger.info(f"{recipient_new_scaled_balance=}")
            logger.info(
                f"Discount Percent: {recipient_previous_discount_percent} -> {recipient_new_discount_percent}"
            )

    return operation


def _process_aave_redeem(
    *,
    w3: Web3,
    discount_token: ChecksumAddress,
    discount_rate_strategy: ChecksumAddress,
    event_data: DebtMintEvent,
    sender: AaveV3UsersTable,
    scaled_token_revision: int,
    debt_position: AaveV3DebtPositionsTable,
    triggering_event: LogReceipt,
    tx_discount_overrides: dict[tuple[HexBytes, ChecksumAddress], int],
    tx_discount_updated_users: set[ChecksumAddress],
) -> UserOperation:
    """
    Process a GHO vToken Mint event triggered by an AAVE redemption event.

    This handles the discount distribution update when a user redeems stkAAVE tokens.
    """

    operation: UserOperation = UserOperation.AAVE_REDEEM

    wad_ray_math_library, percentage_math_library = _get_math_libraries(scaled_token_revision)

    # EVENT DEFINITION
    # event Staked(address indexed from, address indexed to, uint256 assets, uint256 shares)
    # event Redeem(address indexed from, address indexed to, uint256 assets, uint256 shares)
    assets, shares = _decode_uint_values(
        event=triggering_event,
        num_values=2,
    )
    assert assets == shares
    requested_amount = assets

    from_address = event_data.caller
    assert from_address == ZERO_ADDRESS
    assert sender.address == event_data.on_behalf_of

    # For staking/redemption, the recipient is the user staking/redeeming
    sender_debt_position = debt_position

    # Get the discount token balance (will be post-redeem since stkAAVE events processed first)
    sender_discount_token_balance = _get_or_init_stk_aave_balance(
        user=sender,
        discount_token=discount_token,
        block_number=triggering_event["blockNumber"],
        w3=w3,
    )

    if VerboseConfig.is_verbose(
        user_address=sender.address, tx_hash=event_in_process["transactionHash"]
    ):
        logger.info(f"{event_data.caller=}")
        logger.info(f"{event_data.on_behalf_of=}")

    # uint256 recipientPreviousScaledBalance = super.balanceOf(recipient)
    sender_previous_scaled_balance = sender_debt_position.balance
    # from tenderly: 131879097492186474365915
    # from degenbot: 131879097492186474365915 OK!

    if sender_previous_scaled_balance > 0:
        if VerboseConfig.is_verbose(
            user_address=sender.address, tx_hash=event_in_process["transactionHash"]
        ):
            logger.info("Processing case: senderPreviousScaledBalance > 0")

        # Get the effective discount percent for this transaction
        # Use the override if available (set by DiscountPercentUpdated event in same tx),
        # otherwise use the user's current discount
        tx_hash = triggering_event.get("transactionHash")
        effective_discount = (
            tx_discount_overrides.get((tx_hash, sender.address), sender.gho_discount)
            if tx_hash
            else sender.gho_discount
        )

        # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(...)
        sender_discount_scaled = _accrue_debt_on_action(
            debt_position=sender_debt_position,
            percentage_math=percentage_math_library,
            wad_ray_math=wad_ray_math_library,
            previous_scaled_balance=sender_previous_scaled_balance,
            discount_percent=effective_discount,
            index=event_data.index,
            token_revision=scaled_token_revision,
        )

        # _burn(recipient, discountScaled.toUint128())
        sender_debt_position.balance -= sender_discount_scaled
        sender_debt_position.last_index = event_data.index

        sender_previous_discount_percent = effective_discount
        # Skip if discount was already updated via DiscountPercentUpdated event in this tx
        if sender.address not in tx_discount_updated_users:
            _refresh_discount_rate(
                w3=w3,
                user=sender,
                discount_rate_strategy=discount_rate_strategy,
                discount_token_balance=sender_discount_token_balance - requested_amount,
                scaled_debt_balance=sender_debt_position.balance,
                debt_index=event_data.index,
                wad_ray_math=wad_ray_math_library,
            )
        sender_new_discount_percent = sender.gho_discount

        if VerboseConfig.is_verbose(
            user_address=sender.address, tx_hash=event_in_process["transactionHash"]
        ):
            logger.info(f"{sender.address=}")
            logger.info(f"{sender_discount_token_balance=}")
            logger.info(f"{requested_amount=}")
            logger.info(f"{sender_previous_scaled_balance=}")
            logger.info(
                f"Discount Percent: {sender_previous_discount_percent} -> {sender_new_discount_percent}"
            )

    return operation


def _process_staked_aave_transfer(
    *,
    w3: Web3,
    market: AaveV3MarketTable,
    session: Session,
    discount_token: ChecksumAddress,
    discount_rate_strategy: ChecksumAddress,
    event_data: DebtMintEvent,
    scaled_token_revision: int,
    debt_position: AaveV3DebtPositionsTable,
    triggering_event: LogReceipt,
    cache: BlockStateCache,
    tx_discount_overrides: dict[tuple[HexBytes, ChecksumAddress], int],
    tx_discount_updated_users: set[ChecksumAddress],
) -> UserOperation:
    """
    Process a GHO vToken Mint event triggered by an stkAAVE transfer.

    This handles the discount distribution update when stkAAVE is transferred between users.
    Both sender and recipient's discount rates are updated.
    """

    wad_ray_math_library, percentage_math_library = _get_math_libraries(scaled_token_revision)

    # EVENT DEFINITION
    # event Transfer(address indexed from, address indexed to, uint256 value)
    (amount_transferred,) = _decode_uint_values(
        event=triggering_event,
        num_values=1,
    )
    requested_amount = amount_transferred

    from_address = _decode_address(triggering_event["topics"][1])
    to_address = _decode_address(triggering_event["topics"][2])

    # When a user sends or receives stkAAVE, their discount is updated.
    # A sender reduces their stkAAVE balance, so their GHO vToken debt is increased
    # A receiver increases their stkAAVE balance, so their GHO vToken debt is reduced
    sender = _get_or_create_user(session=session, market=market, user_address=from_address)
    recipient = _get_or_create_user(session=session, market=market, user_address=to_address)
    assert sender is not recipient

    sender_debt_position = _get_or_create_debt_position(
        session=session,
        user=sender,
        asset_id=debt_position.asset_id,
    )
    recipient_debt_position = _get_or_create_debt_position(
        session=session,
        user=recipient,
        asset_id=debt_position.asset_id,
    )

    # Get the discount token balances (will reflect prior transfers in same block
    # since stkAAVE events are processed immediately)
    sender_discount_token_balance = _get_or_init_stk_aave_balance(
        user=sender,
        discount_token=discount_token,
        block_number=triggering_event["blockNumber"],
        w3=w3,
    )
    recipient_discount_token_balance = _get_or_init_stk_aave_balance(
        user=recipient,
        discount_token=discount_token,
        block_number=triggering_event["blockNumber"],
        w3=w3,
    )

    if VerboseConfig.is_verbose(
        user_address=sender.address, tx_hash=event_in_process["transactionHash"]
    ):
        logger.info(f"stkAAVE Transfer: {from_address} -> {to_address}")
        logger.info(f"{sender.address}: {sender_discount_token_balance} stkAAVE")
        logger.info(f"{recipient.address}: {recipient_discount_token_balance} stkAAVE")
        logger.info(f"{event_data.caller=}")
        logger.info(f"{event_data.on_behalf_of=}")

    # uint256 senderPreviousScaledBalance = super.balanceOf(sender)
    sender_previous_scaled_balance = sender_debt_position.balance
    if VerboseConfig.is_verbose(
        user_address=sender.address, tx_hash=event_in_process["transactionHash"]
    ):
        logger.info(f"{sender_previous_scaled_balance=}")

    # uint256 recipientPreviousScaledBalance = super.balanceOf(recipient)
    recipient_previous_scaled_balance = recipient_debt_position.balance
    if VerboseConfig.is_verbose(
        user_address=recipient.address, tx_hash=event_in_process["transactionHash"]
    ):
        logger.info(f"{recipient_previous_scaled_balance=}")

    # uint256 index = POOL.getReserveNormalizedVariableDebt(_underlyingAsset)
    # (accessed through event_data.index)

    # Multiple Mint events can be emitted by a single TX!
    # Only update the position if the event corresponds to the sender or receiver.
    # A sender->receiver Transfer where both users hold a balance should emit two events.
    if sender_previous_scaled_balance > 0:
        if VerboseConfig.is_verbose(
            user_address=sender.address, tx_hash=event_in_process["transactionHash"]
        ):
            logger.info("Processing case: senderPreviousScaledBalance > 0")

        # Get the effective discount percent for this transaction
        # Use the override if available (set by DiscountPercentUpdated event in same tx),
        # otherwise use the user's current discount
        tx_hash = triggering_event.get("transactionHash")
        sender_effective_discount = (
            tx_discount_overrides.get((tx_hash, sender.address), sender.gho_discount)
            if tx_hash
            else sender.gho_discount
        )

        # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(...)
        sender_discount_scaled = _accrue_debt_on_action(
            debt_position=sender_debt_position,
            percentage_math=percentage_math_library,
            wad_ray_math=wad_ray_math_library,
            previous_scaled_balance=sender_previous_scaled_balance,
            discount_percent=sender_effective_discount,
            index=event_data.index,
            token_revision=scaled_token_revision,
        )

        # _burn(sender, discountScaled.toUint128())
        sender_debt_position.balance -= sender_discount_scaled
        sender_debt_position.last_index = event_data.index

        sender_previous_discount_percent = sender.gho_discount
        # Skip if discount was already updated via DiscountPercentUpdated event in this tx
        if sender.address not in tx_discount_updated_users:
            _refresh_discount_rate(
                w3=w3,
                user=sender,
                discount_rate_strategy=discount_rate_strategy,
                discount_token_balance=sender_discount_token_balance - requested_amount,
                scaled_debt_balance=sender_debt_position.balance,
                debt_index=event_data.index,
                wad_ray_math=wad_ray_math_library,
            )
        sender_new_discount_percent = sender.gho_discount

        if VerboseConfig.is_verbose(
            user_address=sender.address, tx_hash=event_in_process["transactionHash"]
        ):
            logger.info(f"{sender.address=}")
            logger.info(f"{sender_discount_token_balance=}")
            logger.info(f"{requested_amount=}")
            logger.info(f"{recipient_previous_scaled_balance=}")
            logger.info(
                f"Discount Percent: {sender_previous_discount_percent} -> {sender_new_discount_percent}"
            )

    if recipient_previous_scaled_balance > 0:
        if VerboseConfig.is_verbose(
            user_address=recipient.address, tx_hash=event_in_process["transactionHash"]
        ):
            logger.info("Processing case: recipientPreviousScaledBalance > 0")

        # Get the effective discount percent for this transaction
        # Use the override if available (set by DiscountPercentUpdated event in same tx),
        # otherwise use the user's current discount
        tx_hash = triggering_event.get("transactionHash")
        recipient_effective_discount = (
            tx_discount_overrides.get((tx_hash, recipient.address), recipient.gho_discount)
            if tx_hash
            else recipient.gho_discount
        )

        # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(...)
        recipient_discount_scaled = _accrue_debt_on_action(
            debt_position=recipient_debt_position,
            percentage_math=percentage_math_library,
            wad_ray_math=wad_ray_math_library,
            previous_scaled_balance=recipient_previous_scaled_balance,
            discount_percent=recipient_effective_discount,
            index=event_data.index,
            token_revision=scaled_token_revision,
        )

        # _burn(recipient, discountScaled.toUint128())
        recipient_debt_position.balance -= recipient_discount_scaled
        recipient_new_scaled_balance = recipient_debt_position.balance
        recipient_debt_position.last_index = event_data.index

        recipient_previous_discount_percent = recipient.gho_discount
        # Skip if discount was already updated via DiscountPercentUpdated event in this tx
        if recipient.address not in tx_discount_updated_users:
            _refresh_discount_rate(
                w3=w3,
                user=recipient,
                discount_rate_strategy=discount_rate_strategy,
                discount_token_balance=recipient_discount_token_balance + requested_amount,
                scaled_debt_balance=recipient_new_scaled_balance,
                debt_index=event_data.index,
                wad_ray_math=wad_ray_math_library,
            )
        recipient_new_discount_percent = recipient.gho_discount

        if VerboseConfig.is_verbose(
            user_address=recipient.address, tx_hash=event_in_process["transactionHash"]
        ):
            logger.info(f"{recipient.address=}")
            logger.info(f"{recipient_discount_token_balance=}")
            logger.info(f"{requested_amount=}")
            logger.info(f"{recipient_previous_scaled_balance=}")
            logger.info(f"{recipient_new_scaled_balance=}")
            logger.info(
                f"Discount Percent: {recipient_previous_discount_percent} -> {recipient_new_discount_percent}"
            )

    return UserOperation.STKAAVE_TRANSFER


def _process_gho_debt_mint(
    *,
    w3: Web3,
    market: AaveV3MarketTable,
    session: Session,
    discount_token: ChecksumAddress,
    discount_rate_strategy: ChecksumAddress,
    event_data: DebtMintEvent,
    user: AaveV3UsersTable,
    scaled_token_revision: int,
    debt_position: AaveV3DebtPositionsTable,
    state_block: int,
    event: LogReceipt,
    cache: BlockStateCache,
    tx_discount_overrides: dict[tuple[HexBytes, ChecksumAddress], int],
    tx_discount_updated_users: set[ChecksumAddress],
) -> UserOperation:
    """
    Determine the user operation that triggered a GHO vToken Mint event and apply balance delta.

    Mint events can be triggered by different operations:
    - GHO BORROW: value > balanceIncrease (new debt issued)
    - GHO REPAY: balanceIncrease > value (debt partially repaid)
    - AAVE STAKED/REDEEM/STAKED TRANSFER: value == balanceIncrease with caller == ZERO_ADDRESS
      and accessory Staked/Redeem/Transfer events present
    """

    wad_ray_math_library, percentage_math_library = _get_math_libraries(scaled_token_revision)

    # Get the effective discount percent for this transaction
    # Use the override if available (set by DiscountPercentUpdated event in same tx),
    # otherwise use the user's current discount
    tx_hash = event.get("transactionHash") if event else None
    effective_discount = (
        tx_discount_overrides.get((tx_hash, user.address), user.gho_discount)
        if tx_hash
        else user.gho_discount
    )

    user_operation: UserOperation

    if scaled_token_revision == 1:
        discount_token_balance = _get_or_init_stk_aave_balance(
            user=user,
            discount_token=discount_token,
            block_number=state_block,
            w3=w3,
        )

        previous_scaled_balance = debt_position.balance

        # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(...)
        discount_scaled = _accrue_debt_on_action(
            debt_position=debt_position,
            percentage_math=percentage_math_library,
            wad_ray_math=wad_ray_math_library,
            previous_scaled_balance=previous_scaled_balance,
            discount_percent=effective_discount,
            index=event_data.index,
            token_revision=scaled_token_revision,
        )

        if event_data.value > event_data.balance_increase:
            # emitted in _mintScaled
            # uint256 amountToMint = amount + balanceIncrease;
            requested_amount = event_data.value - event_data.balance_increase
            user_operation = UserOperation.GHO_BORROW
        else:
            # emitted in _burnScaled:
            # uint256 amountToMint = balanceIncrease - amount;
            requested_amount = event_data.balance_increase - event_data.value
            user_operation = UserOperation.GHO_REPAY

        amount_scaled = wad_ray_math_library.ray_div(
            a=requested_amount,
            b=event_data.index,
        )

        if amount_scaled > discount_scaled:
            balance_delta = amount_scaled - discount_scaled
        else:
            balance_delta = -(discount_scaled - amount_scaled)

        # Skip if discount was already updated via DiscountPercentUpdated event in this tx
        if user.address not in tx_discount_updated_users:
            _refresh_discount_rate(
                w3=w3,
                user=user,
                discount_rate_strategy=discount_rate_strategy,
                discount_token_balance=discount_token_balance,
                scaled_debt_balance=debt_position.balance + balance_delta,
                debt_index=event_data.index,
                wad_ray_math=wad_ray_math_library,
            )

    elif scaled_token_revision in {2, 3}:
        # A user accruing GHO vToken debt is labeled the "recipient". A Mint event can be emitted
        # through several paths, and the GHO discount accounting depends on the discount
        # token balance. This variable tracks the role of the user holding the position.

        # Check for accessory events (Staked/Redeem/Transfer) to detect staking-related mints
        # These events may not have value == balance_increase, so check explicitly
        accessory_events = [
            e
            for e in fetch_logs_retrying(
                w3=w3,
                start_block=state_block,
                end_block=state_block,
                address=[discount_token],
                topic_signature=[
                    [
                        AaveV3Event.STAKED.value,
                        AaveV3Event.REDEEM.value,
                        AaveV3Event.TRANSFER.value,
                    ],
                ],
            )
            if e["transactionHash"] == event["transactionHash"]
            if e["topics"][0] in {AaveV3Event.STAKED.value, AaveV3Event.REDEEM.value}
            or (
                # For transfers, only include if it involves the Mint event user
                _decode_address(e["topics"][1]) == user.address  # from
                or _decode_address(e["topics"][2]) == user.address  # to
            )
        ]

        if accessory_events and event_data.caller == ZERO_ADDRESS:
            # This Mint was triggered by staking/transfer - use specialized handler
            staked_aave_result = _process_staked_aave_event(
                w3=w3,
                market=market,
                session=session,
                discount_token=discount_token,
                discount_rate_strategy=discount_rate_strategy,
                event_data=event_data,
                user=user,
                scaled_token_revision=scaled_token_revision,
                debt_position=debt_position,
                state_block=state_block,
                event=event,
                cache=cache,
                tx_discount_overrides=tx_discount_overrides,
                tx_discount_updated_users=tx_discount_updated_users,
            )
            if staked_aave_result is not None:
                return staked_aave_result
            # Fall through to standard mint processing if no accessory events were usable

        # A Mint event can be emitted from _mintScaled or _burnScaled.
        # Determine the source by comparing the event values:
        #   _mintScaled logic implies that amountToMint > balanceIncrease
        #           uint256 amountToMint = amount + balanceIncrease;
        #           emit Mint(caller, onBehalfOf, amountToMint, balanceIncrease, index);
        #   _burnScaled logic implies that balanceIncrease > amountToMint:
        #           uint256 amountToMint = balanceIncrease - amount;
        #           emit Mint(user, user, amountToMint, balanceIncrease, index);
        if event_data.value > event_data.balance_increase:
            user_operation = UserOperation.GHO_BORROW
            if VerboseConfig.is_verbose(
                user_address=user.address, tx_hash=event_in_process["transactionHash"]
            ):
                logger.info("_mintScaled (GHO vToken rev 2)")
                logger.info(f"{user_operation=}")

            requested_amount = event_data.value - event_data.balance_increase

            # uint256 amountScaled = amount.rayDiv(index);
            amount_scaled = wad_ray_math_library.ray_div(
                a=requested_amount,
                b=event_data.index,
            )

            # uint256 previousScaledBalance = super.balanceOf(user);
            previous_scaled_balance = debt_position.balance

            # uint256 discountPercent = _ghoUserState[user].discountPercent;
            # (available from `user`)

            # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(...)
            discount_scaled = _accrue_debt_on_action(
                debt_position=debt_position,
                percentage_math=percentage_math_library,
                wad_ray_math=wad_ray_math_library,
                previous_scaled_balance=previous_scaled_balance,
                discount_percent=effective_discount,
                index=event_data.index,
                token_revision=scaled_token_revision,
            )

            if amount_scaled > discount_scaled:
                # _mint(onBehalfOf, (amountScaled - discountScaled).toUint128()); # noqa:ERA001
                balance_delta = amount_scaled - discount_scaled
            else:
                # _burn(onBehalfOf, (discountScaled - amountScaled).toUint128()); # noqa:ERA001
                balance_delta = -(discount_scaled - amount_scaled)

            if VerboseConfig.is_verbose(
                user_address=user.address, tx_hash=event_in_process["transactionHash"]
            ):
                logger.info(f"{previous_scaled_balance=}")
                logger.info(f"{debt_position.last_index=}")
                logger.info(f"{event_data.index=}")
                logger.info(f"{requested_amount=}")
                logger.info(f"{amount_scaled=}")
                logger.info(f"{discount_scaled=}")
                logger.info(f"{balance_delta=}")

            # Skip if discount was already updated via DiscountPercentUpdated event in this tx
            if user.address not in tx_discount_updated_users:
                discount_token_balance = _get_or_init_stk_aave_balance(
                    user=user,
                    discount_token=discount_token,
                    block_number=state_block,
                    w3=w3,
                )
                _refresh_discount_rate(
                    w3=w3,
                    user=user,
                    discount_rate_strategy=discount_rate_strategy,
                    discount_token_balance=discount_token_balance,
                    scaled_debt_balance=debt_position.balance + balance_delta,
                    debt_index=event_data.index,
                    wad_ray_math=wad_ray_math_library,
                )

        elif event_data.balance_increase > event_data.value:
            user_operation = UserOperation.GHO_REPAY
            if VerboseConfig.is_verbose(
                user_address=user.address, tx_hash=event_in_process["transactionHash"]
            ):
                logger.info("_burnScaled (GHO vToken rev 2)")
                logger.info(f"{user_operation=}")

            requested_amount = event_data.balance_increase - event_data.value

            # uint256 amountScaled = amount.rayDiv(index);
            amount_scaled = wad_ray_math_library.ray_div(
                a=requested_amount,
                b=event_data.index,
            )

            # uint256 previousScaledBalance = super.balanceOf(user);
            previous_scaled_balance = debt_position.balance

            # uint256 balanceBeforeBurn = balanceOf(user);
            previous_index = debt_position.last_index or 0
            balance_before_burn = _get_discounted_balance(
                scaled_balance=previous_scaled_balance,
                previous_index=previous_index,
                current_index=event_data.index,
                user=user,
                ray_math_module=wad_ray_math_library,
                percentage_math=percentage_math_library,
                discount_percent=effective_discount,
            )

            # uint256 discountPercent = _ghoUserState[user].discountPercent;
            # (available from `user`)

            # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(...)
            discount_scaled = _accrue_debt_on_action(
                debt_position=debt_position,
                percentage_math=percentage_math_library,
                wad_ray_math=wad_ray_math_library,
                previous_scaled_balance=previous_scaled_balance,
                discount_percent=effective_discount,
                index=event_data.index,
                token_revision=scaled_token_revision,
            )

            if requested_amount == balance_before_burn:
                # _burn(user, previousScaledBalance.toUint128());
                balance_delta = -previous_scaled_balance
            else:
                # _burn(user, (amountScaled + discountScaled).toUint128());
                balance_delta = -(amount_scaled + discount_scaled)

            if VerboseConfig.is_verbose(
                user_address=user.address, tx_hash=event_in_process["transactionHash"]
            ):
                logger.info(f"{discount_scaled=}")
                logger.info(f"{requested_amount=}")
                logger.info(f"{amount_scaled=}")
                logger.info(f"{balance_delta=}")

            # Skip if discount was already updated via DiscountPercentUpdated event in this tx
            if user.address not in tx_discount_updated_users:
                discount_token_balance = _get_or_init_stk_aave_balance(
                    user=user,
                    discount_token=discount_token,
                    block_number=state_block,
                    w3=w3,
                )
                _refresh_discount_rate(
                    w3=w3,
                    user=user,
                    discount_rate_strategy=discount_rate_strategy,
                    discount_token_balance=discount_token_balance,
                    scaled_debt_balance=debt_position.balance + balance_delta,
                    debt_index=event_data.index,
                    wad_ray_math=wad_ray_math_library,
                )

        else:
            msg = (
                "Unexpected Mint event state: "
                f"value={event_data.value}, balance_increase={event_data.balance_increase}"
            )
            raise ValueError(msg)

    else:
        msg = f"Unknown token revision: {scaled_token_revision}"
        raise ValueError(msg)

    if VerboseConfig.is_verbose(
        user_address=user.address, tx_hash=event_in_process["transactionHash"]
    ):
        logger.info(f"{user.address=}")
        logger.info(f"{user.gho_discount=}")
        logger.info(f"{discount_scaled=}")
        logger.info(f"{balance_delta=}")
        logger.info(f"{discount_token=}")
        logger.info(f"{discount_rate_strategy=}")
        logger.info(f"{state_block=}")

    assert requested_amount >= 0
    assert debt_position.balance + balance_delta >= 0, (
        f"{debt_position.balance} + {balance_delta} < 0!"
    )

    # Update the debt position
    debt_position.balance += balance_delta
    debt_position.last_index = event_data.index

    return user_operation


def _get_scaled_token_asset_by_address(
    market: AaveV3MarketTable,
    token_address: ChecksumAddress,
) -> tuple[AaveV3AssetsTable | None, AaveV3AssetsTable | None]:
    """
    Get collateral and debt assets by token address.
    """
    collateral_asset = _get_asset_by_token_type(
        market=market,
        token_address=token_address,
        token_type=TokenType.COLLATERAL,
    )

    debt_asset = _get_asset_by_token_type(
        market=market,
        token_address=token_address,
        token_type=TokenType.DEBT,
    )

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
                AaveV3AssetsTable,
                AaveV3AssetsTable.a_token_id == Erc20TokenTable.id,
            )
            .where(Erc20TokenTable.chain == chain_id)
        ).all()
    )

    v_token_addresses = list(
        session.scalars(
            select(Erc20TokenTable.address)
            .join(
                AaveV3AssetsTable,
                AaveV3AssetsTable.v_token_id == Erc20TokenTable.id,
            )
            .where(Erc20TokenTable.chain == chain_id)
        ).all()
    )

    return a_token_addresses + v_token_addresses


def _update_contract_revision(
    *,
    w3: Web3,
    market: AaveV3MarketTable,
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

    contract = _get_contract(market=market, contract_name=contract_name)
    contract.revision = revision


def _process_proxy_creation_event(
    *,
    w3: Web3,
    session: Session,
    market: AaveV3MarketTable,
    event: LogReceipt,
    proxy_name: str,
    proxy_id: bytes,
    revision_function_prototype: str,
) -> None:
    """
    Process a proxy creation event (POOL or POOL_CONFIGURATOR).
    """
    (decoded_proxy_id,) = eth_abi.abi.decode(types=["bytes32"], data=event["topics"][1])

    if decoded_proxy_id != proxy_id:
        return

    proxy_address = _decode_address(event["topics"][2])
    implementation_address = _decode_address(event["topics"][3])

    if (
        session.scalar(
            select(AaveV3ContractsTable).where(AaveV3ContractsTable.address == proxy_address)
        )
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

    session.add(
        AaveV3ContractsTable(
            market_id=market.id,
            name=proxy_name,
            address=proxy_address,
            revision=revision,
        )
    )


def _process_discount_percent_updated_event(
    *,
    session: Session,
    market: AaveV3MarketTable,
    event: LogReceipt,
    tx_discount_overrides: dict[tuple[HexBytes, ChecksumAddress], int],
    tx_discount_updated_users: set[ChecksumAddress],
) -> None:
    """Process a GHO discount percent update event.

    Event definition:
    event DiscountPercentUpdated(
        address indexed user,
        uint256 oldDiscountPercent,
        uint256 indexed newDiscountPercent
    );
    """

    user_address = _decode_address(event["topics"][1])

    # Decode the old and new discount percentages from the event data
    # The event has: (address indexed user, uint256 oldDiscountPercent, uint256 indexed newDiscountPercent)
    # So topics[1] = user, topics[2] = newDiscountPercent (indexed), data = oldDiscountPercent
    (old_discount_percent,) = eth_abi.abi.decode(types=["uint256"], data=event["data"])
    new_discount_percent = int.from_bytes(event["topics"][2], "big")

    # Create user if they don't exist - discount can be updated before first debt event
    user = _get_or_create_user(session=session, market=market, user_address=user_address)

    # Store the old discount percent for this transaction so that subsequent
    # Mint/Burn events in the same transaction use the OLD discount value
    tx_hash = event["transactionHash"]
    tx_discount_overrides[tx_hash, user_address] = old_discount_percent
    user.gho_discount = new_discount_percent

    # Mark this user as having had their discount updated in this transaction
    # so that _refresh_discount_rate is not called later in the same tx
    tx_discount_updated_users.add(user_address)

    if VerboseConfig.is_verbose(
        user_address=user_address, tx_hash=event_in_process["transactionHash"]
    ):
        logger.info(f"DiscountPercentUpdated: {user_address}")
        logger.info(f"  old_discount_percent={old_discount_percent}")
        logger.info(f"  new_discount_percent={new_discount_percent}")


def _process_collateral_mint_event(
    *,
    session: Session,
    user: AaveV3UsersTable,
    collateral_asset: AaveV3AssetsTable,
    token_address: ChecksumAddress,
    event_amount: int,
    balance_increase: int,
    index: int,
    event: LogReceipt,
) -> None:
    """Process a collateral (aToken) mint event."""

    collateral_position = _get_or_create_collateral_position(
        session=session, user=user, asset_id=collateral_asset.id
    )

    user_starting_amount = collateral_position.balance

    user_operation = _process_scaled_token_operation(
        event=CollateralMintEvent(
            value=event_amount,
            balance_increase=balance_increase,
            index=index,
        ),
        scaled_token_revision=collateral_asset.a_token_revision,
        position=collateral_position,
    )

    if VerboseConfig.is_verbose(
        user_address=user.address, tx_hash=event_in_process["transactionHash"]
    ):
        _log_token_operation(
            user_operation=user_operation,
            user_address=user.address,
            token_type="aToken",  # noqa: S106
            token_address=token_address,
            index=index,
            balance_info=f"{user_starting_amount} -> {collateral_position.balance}",
            tx_hash=event["transactionHash"],
            block_info=f"{event['blockNumber']}.{event['logIndex']}",
            balance_delta=collateral_position.balance - user_starting_amount,
        )

    assert collateral_position.balance >= 0


def _process_gho_debt_mint_event(
    *,
    w3: Web3,
    session: Session,
    market: AaveV3MarketTable,
    user: AaveV3UsersTable,
    debt_asset: AaveV3AssetsTable,
    token_address: ChecksumAddress,
    event_amount: int,
    balance_increase: int,
    index: int,
    caller_address: ChecksumAddress,
    event: LogReceipt,
    gho_users_to_check: dict[ChecksumAddress, int],
    cache: BlockStateCache,
    tx_discount_overrides: dict[tuple[HexBytes, ChecksumAddress], int],
    tx_discount_updated_users: set[ChecksumAddress],
) -> None:
    """Process a GHO debt (vToken) mint event."""

    debt_position = _get_or_create_debt_position(session=session, user=user, asset_id=debt_asset.id)

    gho_users_to_check[user.address] = event["blockNumber"]
    gho_asset = _get_gho_asset(session=session, market=market)
    assert gho_asset.v_gho_discount_token is not None, "GHO discount token not initialized"
    assert gho_asset.v_gho_discount_rate_strategy is not None, (
        "GHO discount rate strategy not initialized"
    )

    user_starting_amount = debt_position.balance

    user_operation = _process_gho_debt_mint(
        w3=w3,
        market=market,
        session=session,
        discount_token=gho_asset.v_gho_discount_token,
        discount_rate_strategy=gho_asset.v_gho_discount_rate_strategy,
        event_data=DebtMintEvent(
            caller=caller_address,
            on_behalf_of=user.address,
            value=event_amount,
            balance_increase=balance_increase,
            index=index,
        ),
        user=user,
        scaled_token_revision=debt_asset.v_token_revision,
        debt_position=debt_position,
        state_block=event["blockNumber"],
        event=event,
        cache=cache,
        tx_discount_overrides=tx_discount_overrides,
        tx_discount_updated_users=tx_discount_updated_users,
    )

    if VerboseConfig.is_verbose(
        user_address=user.address, tx_hash=event_in_process["transactionHash"]
    ):
        _log_token_operation(
            user_operation=user_operation,
            user_address=user.address,
            token_type="vToken",  # noqa: S106
            token_address=token_address,
            index=index,
            balance_info=f"{user_starting_amount} -> {debt_position.balance}",
            tx_hash=event["transactionHash"],
            block_info=f"{event['blockNumber']}.{event['logIndex']}",
            balance_delta=debt_position.balance - user_starting_amount,
        )

    assert debt_position.balance >= 0


def _process_standard_debt_mint_event(
    *,
    session: Session,
    user: AaveV3UsersTable,
    debt_asset: AaveV3AssetsTable,
    token_address: ChecksumAddress,
    event_amount: int,
    balance_increase: int,
    index: int,
    caller_address: ChecksumAddress,
    event: LogReceipt,
) -> None:
    """Process a standard debt (vToken) mint event (non-GHO)."""

    debt_position = _get_or_create_debt_position(session=session, user=user, asset_id=debt_asset.id)

    user_starting_amount = debt_position.balance

    user_operation = _process_scaled_token_operation(
        event=DebtMintEvent(
            caller=caller_address,
            on_behalf_of=user.address,
            value=event_amount,
            balance_increase=balance_increase,
            index=index,
        ),
        scaled_token_revision=debt_asset.v_token_revision,
        position=debt_position,
    )

    if VerboseConfig.is_verbose(
        user_address=user.address, tx_hash=event_in_process["transactionHash"]
    ):
        _log_token_operation(
            user_operation=user_operation,
            user_address=user.address,
            token_type="vToken",  # noqa: S106
            token_address=token_address,
            index=index,
            balance_info=f"{user_starting_amount} -> {debt_position.balance}",
            tx_hash=event["transactionHash"],
            block_info=f"{event['blockNumber']}.{event['logIndex']}",
            balance_delta=debt_position.balance - user_starting_amount,
        )

    assert debt_position.balance >= 0


def _process_scaled_token_mint_event(context: EventHandlerContext) -> None:
    """
    Process a scaled token Mint event as collateral deposit or debt borrow.

    Mint events have three possible sources, determined by comparing value and
    balanceIncrease event parameters:

    - value > balanceIncrease: _mintScaled (user supply/borrow action)
    - balanceIncrease > value: _burnScaled (interest accrual during repayment)
    - value == balanceIncrease: _transfer (collateral transfer, skipped)

    For _mintScaled, the amount passed to _mint() is calculated as:
        amount = value - balanceIncrease
        amountScaled = ray_div(amount, index)

    For _burnScaled, the event_value represents interest earned and is added
    directly to the balance without conversion.

    All sources create user and position entries if they don't exist.
    """

    # EVENT DEFINITION
    # event Mint(
    #     address indexed caller,
    #     address indexed onBehalfOf,
    #     uint256 value,
    #     uint256 balanceIncrease,
    #     uint256 index
    # );

    caller_address = _decode_address(context.event["topics"][1])
    on_behalf_of_address = _decode_address(context.event["topics"][2])

    # Ignore the caller - all relevant actions apply to on_behalf_of_address
    context.users_to_check[on_behalf_of_address] = context.event["blockNumber"]

    user = _get_or_create_user(
        session=context.session, market=context.market, user_address=on_behalf_of_address
    )

    event_amount, balance_increase, index = _decode_uint_values(event=context.event, num_values=3)

    token_address = get_checksum_address(context.event["address"])
    collateral_asset, debt_asset = _get_scaled_token_asset_by_address(
        market=context.market, token_address=token_address
    )

    if collateral_asset is not None:
        _process_collateral_mint_event(
            session=context.session,
            user=user,
            collateral_asset=collateral_asset,
            token_address=token_address,
            event_amount=event_amount,
            balance_increase=balance_increase,
            index=index,
            event=context.event,
        )

    elif debt_asset is not None:
        if token_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS:
            _process_gho_debt_mint_event(
                w3=context.w3,
                session=context.session,
                market=context.market,
                user=user,
                debt_asset=debt_asset,
                token_address=token_address,
                event_amount=event_amount,
                balance_increase=balance_increase,
                index=index,
                caller_address=caller_address,
                event=context.event,
                gho_users_to_check=context.gho_users_to_check,
                cache=context.cache,
                tx_discount_overrides=context.tx_discount_overrides,
                tx_discount_updated_users=context.tx_discount_updated_users,
            )
        else:
            _process_standard_debt_mint_event(
                session=context.session,
                user=user,
                debt_asset=debt_asset,
                token_address=token_address,
                event_amount=event_amount,
                balance_increase=balance_increase,
                index=index,
                caller_address=caller_address,
                event=context.event,
            )

    else:
        msg = (
            f"Unknown token type for address {get_checksum_address(context.event['address'])}. "
            "Expected aToken or vToken."
        )
        raise ValueError(msg)


def _process_collateral_burn_event(
    *,
    session: Session,
    user: AaveV3UsersTable,
    collateral_asset: AaveV3AssetsTable,
    token_address: ChecksumAddress,
    event_amount: int,
    balance_increase: int,
    index: int,
    event: LogReceipt,
) -> None:
    """Process a collateral (aToken) burn event."""

    collateral_position = session.scalar(
        select(AaveV3CollateralPositionsTable).where(
            AaveV3CollateralPositionsTable.user_id == user.id,
            AaveV3CollateralPositionsTable.asset_id == collateral_asset.id,
        )
    )
    assert collateral_position is not None

    user_starting_amount = collateral_position.balance

    user_operation = _process_scaled_token_operation(
        event=CollateralBurnEvent(
            value=event_amount,
            balance_increase=balance_increase,
            index=index,
        ),
        scaled_token_revision=collateral_asset.a_token_revision,
        position=collateral_position,
    )

    if VerboseConfig.is_verbose(
        user_address=user.address, tx_hash=event_in_process["transactionHash"]
    ):
        _log_token_operation(
            user_operation=user_operation,
            user_address=user.address,
            token_type="aToken",  # noqa: S106
            token_address=token_address,
            index=index,
            balance_info=f"{user_starting_amount} -> {collateral_position.balance}",
            tx_hash=event["transactionHash"],
            block_info=f"{event['blockNumber']}.{event['logIndex']}",
            balance_delta=collateral_position.balance - user_starting_amount,
        )

    assert collateral_position.balance >= 0


def _process_gho_debt_burn_event(
    *,
    w3: Web3,
    session: Session,
    market: AaveV3MarketTable,
    user: AaveV3UsersTable,
    debt_asset: AaveV3AssetsTable,
    token_address: ChecksumAddress,
    event_amount: int,
    balance_increase: int,
    index: int,
    from_address: ChecksumAddress,
    target_address: ChecksumAddress,
    event: LogReceipt,
    gho_users_to_check: dict[ChecksumAddress, int],
    tx_discount_overrides: dict[tuple[HexBytes, ChecksumAddress], int],
    tx_discount_updated_users: set[ChecksumAddress],
) -> None:
    """Process a GHO debt (vToken) burn event."""

    debt_position = session.scalar(
        select(AaveV3DebtPositionsTable).where(
            AaveV3DebtPositionsTable.user_id == user.id,
            AaveV3DebtPositionsTable.asset_id == debt_asset.id,
        )
    )
    assert debt_position is not None

    gho_users_to_check[from_address] = event["blockNumber"]
    gho_asset = _get_gho_asset(session=session, market=market)
    assert gho_asset.v_gho_discount_token is not None, "GHO discount token not initialized"
    assert gho_asset.v_gho_discount_rate_strategy is not None, (
        "GHO discount rate strategy not initialized"
    )

    user_starting_amount = debt_position.balance

    user_operation = _process_gho_debt_burn(
        w3=w3,
        discount_token=gho_asset.v_gho_discount_token,
        discount_rate_strategy=gho_asset.v_gho_discount_rate_strategy,
        event_data=DebtBurnEvent(
            from_=from_address,
            target=target_address,
            value=event_amount,
            balance_increase=balance_increase,
            index=index,
        ),
        user=user,
        scaled_token_revision=debt_asset.v_token_revision,
        debt_position=debt_position,
        state_block=event["blockNumber"],
        event=event,
        tx_discount_overrides=tx_discount_overrides,
        tx_discount_updated_users=tx_discount_updated_users,
    )

    if VerboseConfig.is_verbose(
        user_address=user.address, tx_hash=event_in_process["transactionHash"]
    ):
        _log_token_operation(
            user_operation=user_operation,
            user_address=from_address,
            token_type="vToken",  # noqa: S106
            token_address=token_address,
            index=index,
            balance_info=f"{user_starting_amount} -> {debt_position.balance}",
            tx_hash=event["transactionHash"],
            block_info=f"{event['blockNumber']}.{event['logIndex']}",
            balance_delta=debt_position.balance - user_starting_amount,
        )

    assert debt_position.balance >= 0


def _process_standard_debt_burn_event(
    *,
    session: Session,
    user: AaveV3UsersTable,
    debt_asset: AaveV3AssetsTable,
    token_address: ChecksumAddress,
    event_amount: int,
    balance_increase: int,
    index: int,
    from_address: ChecksumAddress,
    target_address: ChecksumAddress,
    event: LogReceipt,
) -> None:
    """Process a standard debt (vToken) burn event (non-GHO)."""

    debt_position = session.scalar(
        select(AaveV3DebtPositionsTable).where(
            AaveV3DebtPositionsTable.user_id == user.id,
            AaveV3DebtPositionsTable.asset_id == debt_asset.id,
        )
    )
    assert debt_position is not None

    user_starting_amount = debt_position.balance

    user_operation = _process_scaled_token_operation(
        event=DebtBurnEvent(
            from_=from_address,
            target=target_address,
            value=event_amount,
            balance_increase=balance_increase,
            index=index,
        ),
        scaled_token_revision=debt_asset.v_token_revision,
        position=debt_position,
    )

    if VerboseConfig.is_verbose(
        user_address=user.address, tx_hash=event_in_process["transactionHash"]
    ):
        _log_token_operation(
            user_operation=user_operation,
            user_address=from_address,
            token_type="vToken",  # noqa: S106
            token_address=token_address,
            index=index,
            balance_info=f"{user_starting_amount} -> {debt_position.balance}",
            tx_hash=event["transactionHash"],
            block_info=f"{event['blockNumber']}.{event['logIndex']}",
            balance_delta=debt_position.balance - user_starting_amount,
        )

    assert debt_position.balance >= 0


def _process_scaled_token_burn_event(context: EventHandlerContext) -> None:
    """
    Process a scaled token Burn as a collateral withdrawal or debt repayment.
    """

    # EVENT DEFINITION
    # event Burn(
    #     address indexed from,
    #     address indexed target,
    #     uint256 value,
    #     uint256 balanceIncrease,
    #     uint256 index
    # );

    from_address = _decode_address(context.event["topics"][1])
    target_address = _decode_address(context.event["topics"][2])
    context.users_to_check[from_address] = context.event["blockNumber"]

    event_amount, balance_increase, index = _decode_uint_values(event=context.event, num_values=3)

    user = context.session.scalar(
        select(AaveV3UsersTable).where(
            AaveV3UsersTable.address == from_address,
            AaveV3UsersTable.market_id == context.market.id,
        )
    )
    assert user is not None

    token_address = get_checksum_address(context.event["address"])
    collateral_asset, debt_asset = _get_scaled_token_asset_by_address(
        market=context.market, token_address=token_address
    )

    if collateral_asset is not None:
        _process_collateral_burn_event(
            session=context.session,
            user=user,
            collateral_asset=collateral_asset,
            token_address=token_address,
            event_amount=event_amount,
            balance_increase=balance_increase,
            index=index,
            event=context.event,
        )

    elif debt_asset is not None:
        if token_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS:
            _process_gho_debt_burn_event(
                w3=context.w3,
                session=context.session,
                market=context.market,
                user=user,
                debt_asset=debt_asset,
                token_address=token_address,
                event_amount=event_amount,
                balance_increase=balance_increase,
                index=index,
                from_address=from_address,
                target_address=target_address,
                event=context.event,
                gho_users_to_check=context.gho_users_to_check,
                tx_discount_overrides=context.tx_discount_overrides,
                tx_discount_updated_users=context.tx_discount_updated_users,
            )
        else:
            _process_standard_debt_burn_event(
                session=context.session,
                user=user,
                debt_asset=debt_asset,
                token_address=token_address,
                event_amount=event_amount,
                balance_increase=balance_increase,
                index=index,
                from_address=from_address,
                target_address=target_address,
                event=context.event,
            )

    else:
        msg = f"Unknown token type for address {token_address}. Expected aToken or vToken."
        raise ValueError(msg)


def _process_scaled_token_balance_transfer_event(
    context: EventHandlerContext,
) -> None:
    """
    Process a scaled token balance transfer.

    This function assumes aToken collateral, since the transfer() function is disabled by vToken
    contracts to prohibit offloading debt
    """

    # EVENT DEFINITION
    # event BalanceTransfer(
    #     address indexed from,
    #     address indexed to,
    #     uint256 value,
    #     uint256 index
    # );

    from_address = _decode_address(context.event["topics"][1])
    to_address = _decode_address(context.event["topics"][2])

    event_amount, _ = _decode_uint_values(event=context.event, num_values=2)

    # Zero-amount transfers have no effect, so return early instead of adding special cases
    # ref: TX 0xd007ede5e5dcff5e30904db3d66a8e1926fd75742ca838636dd2d5730140dcc6
    if event_amount == 0:
        return

    context.users_to_check[from_address] = context.event["blockNumber"]
    context.users_to_check[to_address] = context.event["blockNumber"]

    aave_asset = _get_asset_by_token_type(
        market=context.market,
        token_address=get_checksum_address(context.event["address"]),
        token_type=TokenType.COLLATERAL,
    )
    assert aave_asset is not None

    from_user = _get_or_create_user(
        session=context.session, market=context.market, user_address=from_address
    )
    assert from_user is not None

    from_user_position = context.session.scalar(
        select(AaveV3CollateralPositionsTable).where(
            AaveV3CollateralPositionsTable.user_id == from_user.id,
            AaveV3CollateralPositionsTable.asset_id == aave_asset.id,
        )
    )
    assert from_user_position, f"{from_address}: TX {context.event['transactionHash'].to_0x_hex()}"

    from_user_starting_amount = from_user_position.balance
    from_user_position.balance -= event_amount

    to_user = _get_or_create_user(
        session=context.session, market=context.market, user_address=to_address
    )
    assert to_user is not None

    if (
        to_user_position := context.session.scalar(
            select(AaveV3CollateralPositionsTable).where(
                AaveV3CollateralPositionsTable.user_id == to_user.id,
                AaveV3CollateralPositionsTable.asset_id == aave_asset.id,
            )
        )
    ) is None:
        to_user_position = _get_or_create_collateral_position(
            session=context.session, user=to_user, asset_id=aave_asset.id
        )

    to_user_starting_amount = to_user_position.balance
    to_user_position.balance += event_amount

    if VerboseConfig.is_verbose(
        user_address=from_address,
        tx_hash=event_in_process["transactionHash"],
    ) or VerboseConfig.is_verbose(
        user_address=to_address,
        tx_hash=event_in_process["transactionHash"],
    ):
        _log_balance_transfer(
            token_address=get_checksum_address(context.event["address"]),
            from_address=from_address,
            from_balance_info=f"{from_user_starting_amount} -> {from_user_position.balance}",
            to_address=to_address,
            to_balance_info=f"{to_user_starting_amount} -> {to_user_position.balance}",
            tx_hash=context.event["transactionHash"],
            block_info=f"{context.event['blockNumber']}.{context.event['logIndex']}",
        )

    assert from_user_position.balance >= 0
    assert to_user_position.balance >= 0


def _process_discount_percent_updated_event_wrapper(
    context: EventHandlerContext,
) -> None:
    """Wrapper to adapt _process_discount_percent_updated_event to EventHandlerContext."""
    _process_discount_percent_updated_event(
        event=context.event,
        market=context.market,
        session=context.session,
        tx_discount_overrides=context.tx_discount_overrides,
        tx_discount_updated_users=context.tx_discount_updated_users,
    )


def _route_transfer_event(context: EventHandlerContext) -> None:
    """
    Route Transfer events to the appropriate handler based on contract address.

    Transfer events are emitted by both scaled tokens (aTokens/vTokens) and stkAAVE.
    Check the contract address to determine which handler to use.
    """
    gho_asset = _get_gho_asset(session=context.session, market=context.market)

    if (
        gho_asset.v_gho_discount_token is not None
        and context.contract_address == gho_asset.v_gho_discount_token
    ):
        # This is a stkAAVE transfer
        _process_stk_aave_transfer_event(context)
    else:
        # This is a scaled token transfer (handled by SCALED_TOKEN_BALANCE_TRANSFER)
        # or some other token - ignore since we handle scaled token transfers separately
        pass


EVENT_HANDLERS: dict[HexBytes, Callable[[EventHandlerContext], None]] = {
    AaveV3Event.USER_E_MODE_SET.value: _process_user_e_mode_set_event,
    AaveV3Event.RESERVE_DATA_UPDATED.value: _process_reserve_data_update_event,
    AaveV3Event.SCALED_TOKEN_BURN.value: _process_scaled_token_burn_event,
    AaveV3Event.SCALED_TOKEN_MINT.value: _process_scaled_token_mint_event,
    AaveV3Event.UPGRADED.value: _process_scaled_token_upgrade_event,
    AaveV3Event.SCALED_TOKEN_BALANCE_TRANSFER.value: _process_scaled_token_balance_transfer_event,
    AaveV3Event.DISCOUNT_RATE_STRATEGY_UPDATED.value: _process_discount_rate_strategy_updated_event,
    AaveV3Event.DISCOUNT_TOKEN_UPDATED.value: _process_discount_token_updated_event,
    AaveV3Event.DISCOUNT_PERCENT_UPDATED.value: _process_discount_percent_updated_event_wrapper,
    AaveV3Event.STAKED.value: _process_stk_aave_staked_event,
    AaveV3Event.REDEEM.value: _process_stk_aave_redeem_event,
    AaveV3Event.TRANSFER.value: _route_transfer_event,
    AaveV3Event.SLASHED.value: _process_stk_aave_slashed_event,
}


def update_aave_market(
    *,
    w3: Web3,
    start_block: int,
    end_block: int,
    market: AaveV3MarketTable,
    session: Session,
    verify_strict: bool,
    verify_chunk: bool,
    no_progress: bool,
) -> None:
    """
    Update the Aave V3 market.
    """

    users_to_check: dict[ChecksumAddress, int] = {}
    gho_users_to_check: dict[ChecksumAddress, int] = {}
    last_event_block = 0

    # Initialize block state cache (reset per block to cache values within a block)
    block_cache = BlockStateCache(w3=w3, block_number=start_block)

    # Get the contract addresses for this market
    pool_address_provider = EthereumMainnetAaveV3.pool_address_provider

    for proxy_creation_event in fetch_logs_retrying(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=[pool_address_provider],
        topic_signature=[
            [AaveV3Event.PROXY_CREATED.value],
        ],
    ):
        _process_proxy_creation_event(
            w3=w3,
            session=session,
            market=market,
            event=proxy_creation_event,
            proxy_name="POOL",
            proxy_id=eth_abi.abi.encode(["bytes32"], [b"POOL"]),
            revision_function_prototype="POOL_REVISION",
        )

        _process_proxy_creation_event(
            w3=w3,
            session=session,
            market=market,
            event=proxy_creation_event,
            proxy_name="POOL_CONFIGURATOR",
            proxy_id=eth_abi.abi.encode(["bytes32"], [b"POOL_CONFIGURATOR"]),
            revision_function_prototype="CONFIGURATOR_REVISION",
        )

    contract_update_events = _get_contract_update_events(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=pool_address_provider,
    )
    for contract_update_event in contract_update_events:
        match contract_update_event["topics"][0]:
            case AaveV3Event.POOL_CONFIGURATOR_UPDATED.value:
                _update_contract_revision(
                    w3=w3,
                    market=market,
                    contract_name="POOL_CONFIGURATOR",
                    new_address=_decode_address(contract_update_event["topics"][2]),
                    revision_function_prototype="CONFIGURATOR_REVISION",
                )

            case AaveV3Event.POOL_UPDATED.value:
                pool = _get_contract(market=market, contract_name="POOL")
                new_address = _decode_address(contract_update_event["topics"][2])
                _update_contract_revision(
                    w3=w3,
                    market=market,
                    contract_name="POOL",
                    new_address=new_address,
                    revision_function_prototype="POOL_REVISION",
                )

            case AaveV3Event.POOL_DATA_PROVIDER_UPDATED.value:
                (old_pool_data_provider_address,) = eth_abi.abi.decode(
                    types=["address"], data=contract_update_event["topics"][1]
                )
                old_pool_data_provider_address = get_checksum_address(
                    old_pool_data_provider_address
                )

                (new_pool_data_provider_address,) = eth_abi.abi.decode(
                    types=["address"], data=contract_update_event["topics"][2]
                )
                new_pool_data_provider_address = get_checksum_address(
                    new_pool_data_provider_address
                )

                if old_pool_data_provider_address == ZERO_ADDRESS:
                    session.add(
                        AaveV3ContractsTable(
                            market_id=market.id,
                            name="POOL_DATA_PROVIDER",
                            address=new_pool_data_provider_address,
                        )
                    )
                else:
                    pool_data_provider = session.scalar(
                        select(AaveV3ContractsTable).where(
                            AaveV3ContractsTable.address == old_pool_data_provider_address
                        )
                    )
                    assert pool_data_provider is not None
                    pool_data_provider.address = new_pool_data_provider_address

    pool = _get_contract(market=market, contract_name="POOL")
    pool_configurator = _get_contract(market=market, contract_name="POOL_CONFIGURATOR")

    # Get all ReserveInitialized events. These are used to mark reserves for further tracking
    reserve_initialization_events = _get_reserve_initialized_events(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=pool_configurator.address,
    )
    for reserve_initialization_event in reserve_initialization_events:
        # Add the new reserve asset
        _process_asset_initialization_event(
            w3=w3,
            event=reserve_initialization_event,
            market=market,
            session=session,
        )

    all_events: list[LogReceipt] = []

    all_events.extend(
        fetch_logs_retrying(
            w3=w3,
            start_block=start_block,
            end_block=end_block,
            address=[pool.address],
            topic_signature=[
                [
                    # Get ReserveDataUpdated events to update the rates and indices for all reserve
                    # assets
                    AaveV3Event.RESERVE_DATA_UPDATED.value,
                    # Get UserEModeSet events to update the EMode category for all users
                    AaveV3Event.USER_E_MODE_SET.value,
                ],
            ],
        )
    )

    known_scaled_token_addresses = _get_all_scaled_token_addresses(
        session=session,
        chain_id=w3.eth.chain_id,
    )

    if known_scaled_token_addresses:
        all_events.extend(
            fetch_logs_retrying(
                w3=w3,
                start_block=start_block,
                end_block=end_block,
                address=known_scaled_token_addresses,
                topic_signature=[
                    [
                        AaveV3Event.SCALED_TOKEN_BALANCE_TRANSFER.value,
                        AaveV3Event.SCALED_TOKEN_BURN.value,
                        AaveV3Event.SCALED_TOKEN_MINT.value,
                        AaveV3Event.UPGRADED.value,
                    ],
                ],
            )
        )

    all_events.extend(
        fetch_logs_retrying(
            w3=w3,
            start_block=start_block,
            end_block=end_block,
            topic_signature=[
                [
                    # Get DiscountRateStrategyUpdated events to set discount rate address for the
                    # GHO vToken
                    AaveV3Event.DISCOUNT_RATE_STRATEGY_UPDATED.value,
                ],
            ],
        )
    )

    discount_token_update_events = fetch_logs_retrying(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        topic_signature=[
            [
                # Get DiscountTokenUpdated events to set the discount token address for the
                # GHO vToken
                AaveV3Event.DISCOUNT_TOKEN_UPDATED.value,
            ],
        ],
    )

    all_events.extend(discount_token_update_events)

    # Fetch stkAAVE events (Staked, Redeem, Transfer, Slashed) from the discount token
    # These update user stkAAVE balances for GHO discount calculations
    if (
        gho_asset := session.scalar(
            select(AaveGhoTokenTable)
            .join(Erc20TokenTable)
            .where(Erc20TokenTable.chain == market.chain_id)
        )
    ) is not None and gho_asset.v_gho_discount_token is not None:
        all_events.extend(
            fetch_logs_retrying(
                w3=w3,
                start_block=start_block,
                end_block=end_block,
                address=[gho_asset.v_gho_discount_token],
                topic_signature=[
                    [
                        AaveV3Event.STAKED.value,
                        AaveV3Event.REDEEM.value,
                        AaveV3Event.TRANSFER.value,
                        AaveV3Event.SLASHED.value,
                    ],
                ],
            )
        )

    # Fetch DiscountPercentUpdated events from the GHO vToken
    discount_percent_update_events = fetch_logs_retrying(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=[GHO_VARIABLE_DEBT_TOKEN_ADDRESS],
        topic_signature=[
            [
                AaveV3Event.DISCOUNT_PERCENT_UPDATED.value,
            ],
        ],
    )

    all_events.extend(discount_percent_update_events)

    # Track per-transaction discount overrides
    # Key: (tx_hash, user_address), Value: old_discount_percent
    tx_discount_overrides: dict[tuple[HexBytes, ChecksumAddress], int] = {}
    # Track users who've had DiscountPercentUpdated in the current transaction
    tx_discount_updated_users: set[ChecksumAddress] = set()
    current_tx_hash: HexBytes | None = None

    for event in tqdm.tqdm(
        sorted(all_events, key=operator.itemgetter("blockNumber", "logIndex")),
        desc="Processing events",
        leave=False,
        disable=no_progress,
    ):
        if verify_strict and users_to_check and event["blockNumber"] > last_event_block:
            _verify_scaled_token_positions(
                w3=w3,
                market=market,
                session=session,
                users_to_check=users_to_check,
                position_table=AaveV3CollateralPositionsTable,
                no_progress=no_progress,
            )
            _verify_scaled_token_positions(
                w3=w3,
                market=market,
                session=session,
                users_to_check=users_to_check,
                position_table=AaveV3DebtPositionsTable,
                no_progress=no_progress,
            )
            users_to_check.clear()
        if verify_strict and gho_users_to_check and event["blockNumber"] > last_event_block:
            _verify_gho_discount_amounts(
                w3=w3,
                session=session,
                market=market,
                users_to_check=gho_users_to_check,
                no_progress=no_progress,
            )
            _verify_stk_aave_balances(
                w3=w3,
                session=session,
                market=market,
                gho_users_to_check=gho_users_to_check,
                no_progress=no_progress,
            )
            gho_users_to_check.clear()

        # Reset cache when moving to a new block
        if event["blockNumber"] != block_cache.block_number:
            block_cache = BlockStateCache(w3=w3, block_number=event["blockNumber"])

        # Clear discount overrides when moving to a new transaction
        if current_tx_hash != event["transactionHash"]:
            current_tx_hash = event["transactionHash"]
            # Only keep overrides for the current transaction
            keys_to_remove = [key for key in tx_discount_overrides if key[0] != current_tx_hash]
            for key in keys_to_remove:
                del tx_discount_overrides[key]
            # Clear the set of users with discount updates in this transaction
            tx_discount_updated_users.clear()

        # TODO: Remove debug variable after testing
        global event_in_process  # noqa: PLW0603
        event_in_process = event

        context = EventHandlerContext(
            w3=w3,
            event=event,
            market=market,
            session=session,
            users_to_check=users_to_check,
            gho_users_to_check=gho_users_to_check,
            cache=block_cache,
            tx_discount_overrides=tx_discount_overrides,
            tx_discount_updated_users=tx_discount_updated_users,
            contract_address=get_checksum_address(event["address"]),
        )
        _dispatch_event(context)

        last_event_block = event["blockNumber"]

    if verify_strict or verify_chunk:
        _verify_scaled_token_positions(
            w3=w3,
            market=market,
            session=session,
            users_to_check=users_to_check,
            position_table=AaveV3CollateralPositionsTable,
            no_progress=no_progress,
        )
        _verify_scaled_token_positions(
            w3=w3,
            market=market,
            session=session,
            users_to_check=users_to_check,
            position_table=AaveV3DebtPositionsTable,
            no_progress=no_progress,
        )
        users_to_check.clear()

        _verify_gho_discount_amounts(
            w3=w3,
            session=session,
            market=market,
            users_to_check=gho_users_to_check,
            no_progress=no_progress,
        )
        _verify_stk_aave_balances(
            w3=w3,
            session=session,
            market=market,
            gho_users_to_check=gho_users_to_check,
            no_progress=no_progress,
        )
        gho_users_to_check.clear()

    # Zero balance rows are not useful
    for table in (AaveV3CollateralPositionsTable, AaveV3DebtPositionsTable):
        session.execute(delete(table).where(table.balance == 0))


def _dispatch_event(context: EventHandlerContext) -> None:
    """
    Dispatch event to appropriate handler based on event topic.
    """
    topic = context.event["topics"][0]
    if topic not in EVENT_HANDLERS:
        msg = f"Unknown event topic: {topic.to_0x_hex()}"
        raise ValueError(msg)

    handler = EVENT_HANDLERS[topic]
    handler(context)
