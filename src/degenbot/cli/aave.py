import os
from dataclasses import dataclass, field
from enum import Enum
from operator import itemgetter
from typing import TYPE_CHECKING, ClassVar, Protocol, TypedDict, cast

import click
import eth_abi.abi
import eth_abi.exceptions
import tqdm
from eth_typing import ChainId, ChecksumAddress
from hexbytes import HexBytes
from sqlalchemy import select
from sqlalchemy.orm import Session
from tqdm.contrib.logging import logging_redirect_tqdm
from web3 import Web3
from web3.exceptions import ContractLogicError
from web3.types import LogReceipt

import degenbot.aave.libraries.v3_1 as aave_library_v3_1
import degenbot.aave.libraries.v3_2 as aave_library_v3_2
import degenbot.aave.libraries.v3_3 as aave_library_v3_3
import degenbot.aave.libraries.v3_4 as aave_library_v3_4
import degenbot.aave.libraries.v3_5 as aave_library_v3_5
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


type TokenRevision = int


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


# GhoVariableDebtToken
# Rev 1: 0x3FEaB6F8510C73E05b8C0Fdf96Df012E3A144319
# Rev 2: 0x7aa606b1B341fFEeAfAdbbE4A2992EFB35972775
# Rev 3: 0x20cb2f303ede313e2cc44549ad8653a5e8c0050e
#        GhoDiscountRateStrategy:
#        0x4C38Ec4D1D2068540DfC11DFa4de41F733DDF812
#        Discount Token: stkAAVE
#        0x4da27a545c0c5B758a6BA100e3a049001de870f5
# Rev 4: 0x9b2b73f9ddd830f82d61520388ccf4fc048f9953

# Revision 4 deprecates the discount mechanism
GHO_REVISION_DISCOUNT_DEPRECATED = 4

GHO_VARIABLE_DEBT_TOKEN_ADDRESS = get_checksum_address("0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B")


@dataclass
class TransactionContext:
    """Context for processing a single transaction as a sequence of events."""

    w3: Web3
    tx_hash: HexBytes
    block_number: int
    events: list[LogReceipt]
    market: AaveV3MarketTable
    session: Session
    gho_asset: AaveGhoTokenTable

    # Pre-categorized event lists for assertions and classification
    pool_events: list[LogReceipt] = field(default_factory=list)
    stk_aave_stakes: list[LogReceipt] = field(default_factory=list)
    stk_aave_redeems: list[LogReceipt] = field(default_factory=list)
    stk_aave_transfers: list[LogReceipt] = field(default_factory=list)
    gho_mints: list[LogReceipt] = field(default_factory=list)
    gho_burns: list[LogReceipt] = field(default_factory=list)
    collateral_mints: list[LogReceipt] = field(default_factory=list)
    collateral_burns: list[LogReceipt] = field(default_factory=list)
    balance_transfers: list[LogReceipt] = field(default_factory=list)
    discount_updates: list[LogReceipt] = field(default_factory=list)
    reserve_data_updates: list[LogReceipt] = field(default_factory=list)
    user_e_mode_sets: list[LogReceipt] = field(default_factory=list)
    upgraded_events: list[LogReceipt] = field(default_factory=list)

    # Snapshot of user discount percents at the start of transaction processing
    # Key: user address, Value: discount percent at transaction start
    user_discounts: dict[ChecksumAddress, int] = field(default_factory=dict)

    # Track discount percent updates by log index for transactions with multiple updates.
    # Key: user address, Value: list of (log_index, old_discount_percent) tuples sorted by
    # log_index. This allows determining the discount in effect at any point in the transaction.
    discount_updates_by_log_index: dict[ChecksumAddress, list[tuple[int, int]]] = field(
        default_factory=dict
    )

    # Set of user addresses that have DiscountPercentUpdated events in this transaction
    # Used to skip _refresh_discount_rate calls when the event provides the authoritative value
    discount_updated_users: set[ChecksumAddress] = field(default_factory=set)

    # Set of user addresses with stkAAVE Transfer events in this transaction
    # Used to skip balance initialization when events provide authoritative values
    stk_aave_transfer_users: set[ChecksumAddress] = field(default_factory=set)

    # Track which pool events have been matched to Mint/Burn events.
    # Key: pool event logIndex, Value: True if already matched
    # This prevents matching multiple Mint events to the same Pool event when
    # a transaction has multiple operations of the same type (e.g., two SUPPLY calls).
    matched_pool_events: dict[int, bool] = field(default_factory=dict)

    def get_events_by_topic(self, topic: HexBytes) -> list[LogReceipt]:
        """Get all events in this transaction with the given topic."""
        return [e for e in self.events if e["topics"][0] == topic]

    def get_prior_events(self, event: LogReceipt) -> list[LogReceipt]:
        """Get all events in this transaction that occurred before the given event."""
        return [e for e in self.events if e["logIndex"] < event["logIndex"]]

    def get_effective_discount_at_log_index(
        self,
        user_address: ChecksumAddress,
        log_index: int,
        default_discount: int,
    ) -> int:
        """
        Get the discount percent in effect at a specific log index.

        When a user has multiple DiscountPercentUpdated events in a transaction,
        each Mint/Burn event must use the discount that was in effect at that
        specific point in time (before any subsequent discount updates).

        Args:
            user_address: The user's address
            log_index: The log index to check
            default_discount: The fallback discount if no updates occurred before this log_index

        Returns:
            The discount percent in effect at the given log index
        """
        updates = self.discount_updates_by_log_index.get(user_address, [])
        if not updates:
            return default_discount

        # Find the most recent discount update before this log_index
        # updates is a list of (log_index, old_discount_percent) tuples sorted by log_index
        effective_discount = default_discount
        for update_log_index, old_discount in updates:
            if update_log_index < log_index:
                effective_discount = old_discount
            else:
                break

        return effective_discount

    def get_pending_stk_aave_delta_at_log_index(
        self,
        user_address: ChecksumAddress,
        log_index: int,
    ) -> int:
        """
        Calculate the net pending stkAAVE balance delta for a user at a specific log index.

        When stkAAVE transfer events occur after the current event (higher log index),
        but the transfer was initiated before the current event (due to reentrancy),
        the GHO debt token contract uses the post-transfer balance. This method
        calculates the net delta from pending transfers to determine the balance
        that was used by the contract.

        Args:
            user_address: The user's address
            log_index: The current log index being processed

        Returns:
            The net pending balance delta (positive for incoming, negative for outgoing)
        """
        net_delta = 0

        for transfer_event in self.stk_aave_transfers:
            transfer_log_index = transfer_event["logIndex"]
            # Only consider transfers that occur AFTER the current event
            if transfer_log_index <= log_index:
                continue

            from_addr = _decode_address(transfer_event["topics"][1])
            to_addr = _decode_address(transfer_event["topics"][2])
            (value,) = _decode_uint_values(event=transfer_event, num_values=1)

            if from_addr == user_address:
                net_delta -= value
            if to_addr == user_address:
                net_delta += value

        return net_delta


event_in_process: LogReceipt


@dataclass
class EventHandlerContext:
    """Context object passed to event handlers containing all necessary state."""

    w3: Web3
    event: LogReceipt
    market: AaveV3MarketTable
    session: Session
    gho_asset: AaveGhoTokenTable
    contract_address: ChecksumAddress
    tx_context: TransactionContext | None = None


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


def _log_if_verbose(
    user_address: ChecksumAddress,
    tx_hash: HexBytes | None,
    *messages: str,
) -> None:
    """Log messages if verbose mode is enabled for the given context."""
    if VerboseConfig.is_verbose(user_address=user_address, tx_hash=tx_hash):
        for msg in messages:
            logger.info(msg)


def _init_verbose_config_from_env() -> None:
    """Initialize VerboseConfig from environment variables."""
    # DEGENBOT_VERBOSE_ALL: Set to "1", "true", or "yes" to enable all verbose logging
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
    """Aave V3 event topic hashes.

    When adding a new event type, ensure it is categorized in:
    - _build_transaction_contexts() for transaction-level events
    Validation is performed at module load by _validate_event_coverage().
    """

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
    # Pool contract events
    SUPPLY = HexBytes("0x2b627736bca15cd5381dcf80b0bf11fd197d01a037c52b927a881a10fb73ba61")
    WITHDRAW = HexBytes("0x3115d1449a7b732c986cba18244e897a450f61e1bb8d589cd2e69e6c8924f9f7")
    BORROW = HexBytes("0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0")
    REPAY = HexBytes("0xa534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051")
    LIQUIDATION_CALL = HexBytes(
        "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286"
    )
    DEFICIT_CREATED = HexBytes("0x2bccfb3fad376d59d7accf970515eb77b2f27b082c90ed0fb15583dd5a942699")
    ADDRESS_SET = HexBytes("0x9ef0e8c8e52743bb38b83b17d9429141d494b8041ca6d616a6c77cebae9cd8b7")


# Events that trigger transaction-level processing
TRIGGER_EVENTS: set[HexBytes] = {
    AaveV3Event.SCALED_TOKEN_MINT.value,
    AaveV3Event.SCALED_TOKEN_BURN.value,
    AaveV3Event.SCALED_TOKEN_BALANCE_TRANSFER.value,
    AaveV3Event.TRANSFER.value,
}


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
    5: MathLibraries(
        wad_ray=aave_library_v3_5.wad_ray_math,
        percentage=aave_library_v3_5.percentage_math,
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


def _extract_user_addresses_from_event(event: LogReceipt) -> set[ChecksumAddress]:
    """
    Extract user addresses from an Aave event.

    Returns a set of all user addresses (senders, recipients, onBehalfOf, etc.)
    that are involved in the event.
    """
    user_addresses: set[ChecksumAddress] = set()
    topic = event["topics"][0]

    if topic == AaveV3Event.SCALED_TOKEN_MINT.value:
        user_addresses.add(_decode_address(event["topics"][2]))

    elif topic == AaveV3Event.SCALED_TOKEN_BURN.value:
        user_addresses.add(_decode_address(event["topics"][1]))

    elif topic == AaveV3Event.SCALED_TOKEN_BALANCE_TRANSFER.value:
        user_addresses.add(_decode_address(event["topics"][1]))
        user_addresses.add(_decode_address(event["topics"][2]))

    elif topic == AaveV3Event.TRANSFER.value:
        from_addr = _decode_address(event["topics"][1])
        to_addr = _decode_address(event["topics"][2])
        if from_addr != ZERO_ADDRESS:
            user_addresses.add(from_addr)
        if to_addr != ZERO_ADDRESS:
            user_addresses.add(to_addr)

    elif topic in {
        AaveV3Event.DISCOUNT_PERCENT_UPDATED.value,
        AaveV3Event.USER_E_MODE_SET.value,
        AaveV3Event.DEFICIT_CREATED.value,
    }:
        user_addresses.add(_decode_address(event["topics"][1]))

    elif topic in {
        AaveV3Event.BORROW.value,
        AaveV3Event.REPAY.value,
        AaveV3Event.SUPPLY.value,
        AaveV3Event.WITHDRAW.value,
    }:
        user_addresses.add(_decode_address(event["topics"][2]))

    elif topic == AaveV3Event.LIQUIDATION_CALL.value:
        user_addresses.add(_decode_address(event["topics"][3]))

    elif topic in {AaveV3Event.STAKED.value, AaveV3Event.REDEEM.value}:
        user_addresses.add(_decode_address(event["topics"][1]))
        user_addresses.add(_decode_address(event["topics"][2]))

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
            session.flush()
            session.add(
                AaveV3ContractsTable(
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
)
@click.option(
    "--no-progress-bar",
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
    verify: bool,
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
        verify: If True, verify position balances at every block boundary.
        stop_after_one_chunk: If True, stop after processing the first chunk.
        no_progress: If True, disable progress bars.
    """

    with db_session() as session, logging_redirect_tqdm(loggers=[logger]):
        active_chains = set(
            session.scalars(
                select(AaveV3MarketTable.chain_id).where(
                    AaveV3MarketTable.active,
                    AaveV3MarketTable.name.contains("aave"),
                )
            ).all()
        )

        for chain_id in active_chains:
            w3 = get_web3_from_config(chain_id=chain_id)

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
                    update_aave_market(
                        w3=w3,
                        start_block=working_start_block,
                        end_block=working_end_block,
                        market=market,
                        session=session,
                        verify=verify,
                        no_progress=no_progress,
                    )

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

    logger.info("Update successful")


def _process_asset_initialization_event(
    w3: Web3,
    event: LogReceipt,
    market: AaveV3MarketTable,
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

    asset_address = _decode_address(event["topics"][1])
    a_token_address = _decode_address(event["topics"][2])

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

    new_asset = AaveV3AssetsTable(
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
    market.assets.append(new_asset)
    session.flush()
    logger.info(f"Added new Aave V3 asset: {asset_address}")


def _process_user_e_mode_set_event(
    context: EventHandlerContext,
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

    user_address = _decode_address(context.event["topics"][1])

    (e_mode,) = eth_abi.abi.decode(types=["uint8"], data=context.event["data"])

    user = _get_or_create_user(
        context=context,
        market=context.market,
        user_address=user_address,
        w3=context.w3,
        block_number=context.event["blockNumber"],
    )
    user.e_mode = e_mode


def _process_discount_token_updated_event(
    context: EventHandlerContext,
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

    old_discount_token_address = _decode_address(context.event["topics"][1])
    new_discount_token_address = _decode_address(context.event["topics"][2])

    context.gho_asset.v_gho_discount_token = new_discount_token_address

    logger.info(
        f"SET NEW DISCOUNT TOKEN: {old_discount_token_address} -> {new_discount_token_address}"
    )


def _process_discount_rate_strategy_updated_event(
    context: EventHandlerContext,
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

    old_discount_rate_strategy_address = _decode_address(context.event["topics"][1])
    new_discount_rate_strategy_address = _decode_address(context.event["topics"][2])

    context.gho_asset.v_gho_discount_rate_strategy = new_discount_rate_strategy_address

    logger.info(
        f"SET NEW DISCOUNT RATE STRATEGY: {old_discount_rate_strategy_address} -> "
        f"{new_discount_rate_strategy_address}"
    )


def _get_or_init_stk_aave_balance(
    *,
    user: AaveV3UsersTable,
    discount_token: ChecksumAddress | None,
    block_number: int,
    w3: Web3,
    skip_init: bool = False,
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

    skip_init is unused but kept for API compatibility.
    """

    # If discount_token is None (revision 4+), return 0
    if discount_token is None:
        return 0

    if user.stk_aave_balance is None and not skip_init:
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
    elif user.stk_aave_balance is None and skip_init:
        # Fetch from contract at block_number - 1
        # skip_init only prevents double-fetching within the same transaction
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

    # At this point, stk_aave_balance should always be set
    assert user.stk_aave_balance is not None

    # Check if we need to account for pending transfers due to reentrancy
    # This happens when stkAAVE is transferred during GHO discount updates,
    # and the GHO contract sees the post-transfer balance before the Transfer event
    if (
        tx_context is not None
        and log_index is not None
        and user.address in tx_context.stk_aave_transfer_users
    ):
        pending_delta = tx_context.get_pending_stk_aave_delta_at_log_index(
            user_address=user.address,
            log_index=log_index,
        )
        return user.stk_aave_balance + pending_delta

    return user.stk_aave_balance


def _process_stk_aave_transfer_event(context: EventHandlerContext) -> None:
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

    gho_asset = context.gho_asset
    if gho_asset.v_gho_discount_token is None:
        # Ignore stkAAVE transfers until the discount token has been set
        return

    assert context.contract_address == gho_asset.v_gho_discount_token

    from_address = _decode_address(context.event["topics"][1])
    to_address = _decode_address(context.event["topics"][2])

    if from_address == to_address:
        return

    (value,) = _decode_uint_values(event=context.event, num_values=1)

    tx_hash = context.event.get("transactionHash")

    if VerboseConfig.is_verbose(
        user_address=from_address, tx_hash=tx_hash
    ) or VerboseConfig.is_verbose(user_address=to_address, tx_hash=tx_hash):
        logger.info(f"stkAAVE transfer: {from_address} -> {to_address}, value={value}")

    # Get or create users involved in the transfer
    block_number = context.event["blockNumber"]
    assert block_number is not None

    from_user = (
        _get_or_create_user(
            context=context,
            market=context.market,
            user_address=from_address,
            w3=context.w3,
            block_number=block_number,
        )
        if from_address != ZERO_ADDRESS
        else None
    )
    to_user = (
        _get_or_create_user(
            context=context,
            market=context.market,
            user_address=to_address,
            w3=context.w3,
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
            discount_token=gho_asset.v_gho_discount_token,
            block_number=context.event["blockNumber"],
            w3=context.w3,
            skip_init=context.tx_context is not None
            and from_user.address in context.tx_context.stk_aave_transfer_users,
        )
    if to_user is not None and to_user.stk_aave_balance is None:
        _get_or_init_stk_aave_balance(
            user=to_user,
            discount_token=gho_asset.v_gho_discount_token,
            block_number=context.event["blockNumber"],
            w3=context.w3,
            skip_init=context.tx_context is not None
            and to_user.address in context.tx_context.stk_aave_transfer_users,
        )

    # Apply balance changes
    if from_user is not None:
        assert from_user.stk_aave_balance is not None
        assert from_user.stk_aave_balance >= 0
        from_user_old_balance = from_user.stk_aave_balance
        from_user.stk_aave_balance -= value

        if VerboseConfig.is_verbose(user_address=from_address, tx_hash=tx_hash):
            logger.info(f"stkAAVE balance update: {from_address}")
            logger.info(f"  before: {from_user_old_balance}")
            logger.info(f"  after: {from_user.stk_aave_balance}")
            logger.info(f"  delta: -{value}")

    if to_user is not None:
        assert to_user.stk_aave_balance is not None
        assert to_user.stk_aave_balance >= 0
        to_user_old_balance = to_user.stk_aave_balance
        to_user.stk_aave_balance += value

        if VerboseConfig.is_verbose(user_address=to_address, tx_hash=tx_hash):
            logger.info(f"stkAAVE balance update: {to_address}")
            logger.info(f"  before: {to_user_old_balance}")
            logger.info(f"  after: {to_user.stk_aave_balance}")
            logger.info(f"  delta: +{value}")


def _process_reserve_data_update_event(
    context: EventHandlerContext,
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

    Reference:
    ```
    event Upgraded(
        address indexed implementation
    );
    ```
    """

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
        logger.info(
            f"Upgraded aToken revision for {aave_collateral_asset.a_token.address} "
            f"to {atoken_revision}"
        )
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

        # Handle GHO discount deprecation on upgrade to revision 4+
        if (
            aave_debt_asset.v_token.address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS
            and vtoken_revision >= GHO_REVISION_DISCOUNT_DEPRECATED
        ):
            gho_asset = _get_gho_asset(context.session, context.market)
            gho_asset.v_gho_discount_token = None
            gho_asset.v_gho_discount_rate_strategy = None
            logger.info(f"GHO discount mechanism deprecated at revision {vtoken_revision}")
    else:
        token_address = get_checksum_address(context.event["address"])
        msg = f"Unknown token type for address {token_address}. Expected aToken or vToken."
        raise ValueError(msg)


def _get_gho_vtoken_revision(market: AaveV3MarketTable) -> int | None:
    """Get the GHO vToken revision from market assets."""
    for asset in market.assets:
        if asset.v_token and asset.v_token.address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS:
            return asset.v_token_revision
    return None


def _is_discount_supported(market: AaveV3MarketTable) -> bool:
    """Check if GHO discount mechanism is supported (revision 2 or 3)."""
    revision = _get_gho_vtoken_revision(market)
    return revision is not None and revision < GHO_REVISION_DISCOUNT_DEPRECATED


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


def _matches_pool_event(
    event: LogReceipt,
    expected_type: HexBytes,
    user_address: ChecksumAddress,
    reserve_address: ChecksumAddress,
) -> bool:
    """Check if a Pool event matches the expected type and parameters."""
    event_topic = event["topics"][0]
    # Allow LIQUIDATION_CALL when expecting REPAY or WITHDRAW
    # Allow DEFICIT_CREATED when expecting REPAY (bad debt write-off during liquidation)
    if (
        event_topic != expected_type
        and not (
            event_topic == AaveV3Event.LIQUIDATION_CALL.value
            and expected_type in {AaveV3Event.REPAY.value, AaveV3Event.WITHDRAW.value}
        )
        and not (
            event_topic == AaveV3Event.DEFICIT_CREATED.value
            and expected_type == AaveV3Event.REPAY.value
        )
    ):
        return False

    if expected_type == AaveV3Event.BORROW.value:
        event_reserve = _decode_address(event["topics"][1])
        event_on_behalf_of = _decode_address(event["topics"][2])
        (_, _, interest_rate_mode, _) = eth_abi.abi.decode(
            types=["address", "uint256", "uint8", "uint256"],
            data=event["data"],
        )
        return (
            event_on_behalf_of == user_address
            and event_reserve == reserve_address
            and interest_rate_mode == 2  # Variable rate # noqa: PLR2004
        )

    if expected_type == AaveV3Event.REPAY.value:
        if event_topic == AaveV3Event.REPAY.value:
            # Normal REPAY event matching
            event_reserve = _decode_address(event["topics"][1])
            event_user = _decode_address(event["topics"][2])
            return event_user == user_address and event_reserve == reserve_address
        if event_topic == AaveV3Event.LIQUIDATION_CALL.value:
            # Liquidation matching - match on debtAsset
            event_debt_asset = _decode_address(event["topics"][2])
            event_user = _decode_address(event["topics"][3])
            return event_user == user_address and event_debt_asset == reserve_address
        if event_topic == AaveV3Event.DEFICIT_CREATED.value:
            # DeficitCreated matching - debt written off as bad debt during liquidation
            event_user = _decode_address(event["topics"][1])
            event_reserve = _decode_address(event["topics"][2])
            return event_user == user_address and event_reserve == reserve_address

    elif expected_type == AaveV3Event.SUPPLY.value:
        event_reserve = _decode_address(event["topics"][1])
        event_on_behalf_of = _decode_address(event["topics"][2])
        return event_on_behalf_of == user_address and event_reserve == reserve_address

    if expected_type == AaveV3Event.WITHDRAW.value:
        if event_topic == AaveV3Event.WITHDRAW.value:
            # Normal WITHDRAW event matching
            event_reserve = _decode_address(event["topics"][1])
            event_user = _decode_address(event["topics"][2])
            return event_user == user_address and event_reserve == reserve_address
        if event_topic == AaveV3Event.LIQUIDATION_CALL.value:
            # Liquidation matching - match on collateralAsset
            event_collateral_asset = _decode_address(event["topics"][1])
            event_user = _decode_address(event["topics"][3])
            return event_user == user_address and event_collateral_asset == reserve_address

    if expected_type == AaveV3Event.LIQUIDATION_CALL.value:
        event_collateral_asset = _decode_address(event["topics"][1])
        event_debt_asset = _decode_address(event["topics"][2])
        event_user = _decode_address(event["topics"][3])
        # When expecting REPAY, match on debtAsset
        # When expecting WITHDRAW, match on collateralAsset
        if event_user == user_address:
            return reserve_address in {event_debt_asset, event_collateral_asset}

    return False


def _verify_pool_event_for_transaction(
    *,
    w3: Web3,
    market: AaveV3MarketTable,
    event: LogReceipt,
    expected_event_type: HexBytes,
    user_address: ChecksumAddress,
    reserve_address: ChecksumAddress,
) -> LogReceipt:
    """
    DEPRECATED: Use _assert_pool_event_exists with TransactionContext instead.

    This function is kept for backward compatibility during the transition.
    It fetches Pool events on-demand, which is less efficient than using
    pre-fetched events from TransactionContext.

    TODO: Remove this function once all callers have been updated to use
    TransactionContext-based assertions.
    """
    pool_contract = _get_contract(market=market, contract_name="POOL")

    pool_events = fetch_logs_retrying(
        w3=w3,
        start_block=event["blockNumber"],
        end_block=event["blockNumber"],
        address=[pool_contract.address],
        topic_signature=[
            [
                AaveV3Event.BORROW.value,
                AaveV3Event.REPAY.value,
                AaveV3Event.SUPPLY.value,
                AaveV3Event.WITHDRAW.value,
                AaveV3Event.LIQUIDATION_CALL.value,
                AaveV3Event.DEFICIT_CREATED.value,
            ],
        ],
    )

    tx_hash = event["transactionHash"]

    for pool_event in pool_events:
        if pool_event["transactionHash"] != tx_hash:
            continue

        event_topic = pool_event["topics"][0]

        # Allow LIQUIDATION_CALL when expecting REPAY or WITHDRAW
        # Allow DEFICIT_CREATED when expecting REPAY (bad debt write-off during liquidation)
        if (
            event_topic != expected_event_type
            and not (
                event_topic == AaveV3Event.LIQUIDATION_CALL.value
                and expected_event_type in {AaveV3Event.REPAY.value, AaveV3Event.WITHDRAW.value}
            )
            and not (
                event_topic == AaveV3Event.DEFICIT_CREATED.value
                and expected_event_type == AaveV3Event.REPAY.value
            )
        ):
            continue

        if _matches_pool_event(pool_event, expected_event_type, user_address, reserve_address):
            return pool_event

    # No matching event found
    event_name = "UNKNOWN"
    if expected_event_type == AaveV3Event.BORROW.value:
        event_name = "BORROW"
    elif expected_event_type == AaveV3Event.REPAY.value:
        event_name = "REPAY"
    elif expected_event_type == AaveV3Event.SUPPLY.value:
        event_name = "SUPPLY"
    elif expected_event_type == AaveV3Event.WITHDRAW.value:
        event_name = "WITHDRAW"

    msg = (
        f"Expected {event_name} event for user {user_address} with reserve {reserve_address} "
        f"in transaction {tx_hash.to_0x_hex()} but no matching event found."
    )
    raise ValueError(msg)


def _verify_address_set_for_transaction(
    w3: Web3,
    market: AaveV3MarketTable,
    event: LogReceipt,
    user_address: ChecksumAddress,
) -> LogReceipt:
    """
    Verify that an AddressSet event exists for the given user in the transaction.

    This is used to validate that a contract address was registered as a system
    contract (e.g., UMBRELLA) before processing burns for that address.
    """
    provider_contract = _get_contract(market=market, contract_name="POOL_ADDRESS_PROVIDER")

    provider_events = fetch_logs_retrying(
        w3=w3,
        start_block=event["blockNumber"],
        end_block=event["blockNumber"],
        address=[provider_contract.address],
        topic_signature=[[AaveV3Event.ADDRESS_SET.value]],
    )

    tx_hash = event["transactionHash"]

    for provider_event in provider_events:
        if provider_event["transactionHash"] != tx_hash:
            continue

        new_address = _decode_address(provider_event["topics"][3])
        if new_address == user_address:
            return provider_event

    msg = (
        f"Expected AddressSet event for user {user_address} "
        f"in transaction {tx_hash.to_0x_hex()} but no matching event found."
    )
    raise ValueError(msg)


def _get_or_create_user(
    *,
    context: EventHandlerContext,
    market: AaveV3MarketTable,
    user_address: ChecksumAddress,
    w3: Web3,
    block_number: int,
) -> AaveV3UsersTable:
    """
    Get existing user or create new one with default e_mode.

    When creating a new user, if w3 and block_number are provided and the user
    has an existing GHO debt position, their discount percent will be fetched
    from the contract to properly initialize their gho_discount value.
    """

    user = context.session.scalar(
        select(AaveV3UsersTable).where(
            AaveV3UsersTable.address == user_address,
            AaveV3UsersTable.market_id == market.id,
        )
    )

    if user is None:
        # When creating a new user, check if they have a GHO discount on-chain
        # to properly initialize their gho_discount value
        gho_discount = 0

        # Only fetch discount if mechanism is supported (revision 2 or 3)
        if context.gho_asset.v_gho_discount_token is not None and _is_discount_supported(market):
            try:
                (discount_percent,) = raw_call(
                    w3=w3,
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

        user = AaveV3UsersTable(
            market_id=market.id,
            address=user_address,
            e_mode=0,
            gho_discount=gho_discount,
        )
        market.users.append(user)
        context.session.add(user)
        context.session.flush()

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


def _get_or_create_position[T: AaveV3CollateralPositionsTable | AaveV3DebtPositionsTable](
    session: Session,
    user: AaveV3UsersTable,
    asset_id: int,
    positions: list[T],
    position_table: type[T],
) -> T:
    """
    Get existing position or create new one with zero balance.
    """

    for position in positions:
        if position.asset_id == asset_id:
            return position

    new_position = cast("T", position_table(user_id=user.id, asset_id=asset_id, balance=0))
    session.add(new_position)
    positions.append(new_position)
    return new_position


def _get_or_create_collateral_position(
    session: Session,
    user: AaveV3UsersTable,
    asset_id: int,
) -> AaveV3CollateralPositionsTable:
    """Get existing collateral position or create new one with zero balance."""
    return _get_or_create_position(
        session=session,
        user=user,
        asset_id=asset_id,
        positions=user.collateral_positions,
        position_table=AaveV3CollateralPositionsTable,
    )


def _get_or_create_debt_position(
    session: Session,
    user: AaveV3UsersTable,
    asset_id: int,
) -> AaveV3DebtPositionsTable:
    """Get existing debt position or create new one with zero balance."""
    return _get_or_create_position(
        session=session,
        user=user,
        asset_id=asset_id,
        positions=user.debt_positions,
        position_table=AaveV3DebtPositionsTable,
    )


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
    gho_asset: AaveGhoTokenTable,
    block_number: int,
    no_progress: bool,
    user_addresses: set[ChecksumAddress] | None = None,
) -> None:
    """
    Verify that GHO discount values in the database match the contract.

    If user_addresses is provided, only verifies those specific users.
    Otherwise, verifies all users in the market.
    """

    # Skip verification if discount mechanism is not supported (revision 4+)
    if not _is_discount_supported(market):
        return

    if gho_asset.v_gho_discount_token is None:
        return

    if user_addresses is not None and len(user_addresses) == 0:
        return

    stmt = select(AaveV3UsersTable).where(AaveV3UsersTable.market_id == market.id)
    if user_addresses is not None:
        stmt = stmt.where(AaveV3UsersTable.address.in_(user_addresses))

    users_to_verify = session.scalars(stmt).all()

    for user in tqdm.tqdm(
        users_to_verify,
        desc="Verifying GHO discount amounts",
        leave=False,
        disable=no_progress,
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
    market: AaveV3MarketTable,
    gho_asset: AaveGhoTokenTable,
    block_number: int,
    no_progress: bool,
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

    stmt = select(AaveV3UsersTable).where(
        AaveV3UsersTable.market_id == market.id,
        AaveV3UsersTable.stk_aave_balance.is_not(None),
    )
    if user_addresses is not None:
        stmt = stmt.where(AaveV3UsersTable.address.in_(user_addresses))

    users_to_verify = session.scalars(stmt).all()

    for user in tqdm.tqdm(
        users_to_verify,
        desc="Verifying stkAAVE balances",
        leave=False,
        disable=no_progress,
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
    market: AaveV3MarketTable,
    no_progress: bool,
) -> None:
    """
    Delete all zero-balance debt and collateral positions for the market.

    This cleanup runs after chunk verification to remove positions that no longer
    hold any balance, keeping the database lean.
    """

    # Delete zero-balance collateral positions
    collateral_positions = session.scalars(
        select(AaveV3CollateralPositionsTable)
        .join(AaveV3UsersTable)
        .where(
            AaveV3UsersTable.market_id == market.id,
            AaveV3CollateralPositionsTable.balance == 0,
        )
    ).all()

    for position in tqdm.tqdm(
        collateral_positions,
        desc="Cleaning up zero-balance collateral positions",
        leave=False,
        disable=no_progress,
    ):
        session.delete(position)

    # Delete zero-balance debt positions
    debt_positions = session.scalars(
        select(AaveV3DebtPositionsTable)
        .join(AaveV3UsersTable)
        .where(
            AaveV3UsersTable.market_id == market.id,
            AaveV3DebtPositionsTable.balance == 0,
        )
    ).all()

    for position in tqdm.tqdm(
        debt_positions,
        desc="Cleaning up zero-balance debt positions",
        leave=False,
        disable=no_progress,
    ):
        session.delete(position)


def _verify_scaled_token_positions(
    *,
    w3: Web3,
    market: AaveV3MarketTable,
    session: Session,
    position_table: type[AaveV3CollateralPositionsTable | AaveV3DebtPositionsTable],
    block_number: int,
    no_progress: bool,
    user_addresses: set[ChecksumAddress] | None = None,
) -> None:
    """
    Verify that database position balances match the contract.

    If user_addresses is provided, only verifies positions for those specific users.
    Otherwise, verifies all users in the market.
    """

    desc = (
        "Verifying collateral positions"
        if position_table is AaveV3CollateralPositionsTable
        else "Verifying debt positions"
    )

    if user_addresses is not None and len(user_addresses) == 0:
        return

    # Query users for this market
    stmt = select(AaveV3UsersTable).where(AaveV3UsersTable.market_id == market.id)
    if user_addresses is not None:
        stmt = stmt.where(AaveV3UsersTable.address.in_(user_addresses))

    users_to_verify = session.scalars(stmt).all()

    for user in tqdm.tqdm(
        users_to_verify,
        desc=desc,
        leave=False,
        disable=no_progress,
    ):
        if user.address == ZERO_ADDRESS:
            logger.error("SKIPPED ZERO ADDRESS!")
            continue

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
                    function_arguments=[user.address],
                ),
                return_types=["uint256"],
                block_identifier=block_number,
            )

            assert actual_scaled_balance == position.balance, (
                f"User {user.address}: "
                f"{'collateral' if position_table is AaveV3CollateralPositionsTable else 'debt'} "
                f"balance ({position.balance}) does not match scaled token contract "
                f"({actual_scaled_balance}) @ {token_address} at block {block_number}"
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
                f"User {user.address}: "
                f"{'collateral' if position_table is AaveV3CollateralPositionsTable else 'debt'} "
                f"last_index ({position.last_index}) does not match contract "
                f"({actual_last_index}) @ {token_address} at block {block_number}"
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
    scaled_delta: int | None = None,
) -> UserOperation:
    """
    Determine the user operation for scaled token events and apply balance delta to position.

    Args:
        event: The scaled token event data
        scaled_token_revision: The token contract revision
        position: The user's position to update
        scaled_delta: Optional pre-calculated scaled amount delta. If provided, this value
                     is used directly instead of deriving it from event.value and
                     event.balance_increase (which can introduce rounding errors).
                     This is used by scaled token revision 4+.
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
                if scaled_delta is not None:
                    # Use pre-calculated scaled amount to avoid rounding errors
                    balance_delta = scaled_delta
                else:
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

    Reference:
    ```
    /**
    * @dev Accumulates debt of the user since last action.
    * @dev It skips applying discount in case there is no balance increase or discount percent is
           zero.
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

        # Update last_index to match contract behavior:
        # _userState[user].additionalData = index.toUint128(); # noqa: ERA001
        debt_position.last_index = index

    elif token_revision >= 4:
        # Revision 4+: Discount mechanism deprecated, _accrueDebtOnAction removed
        # Simply calculate interest accrual without discount
        balance_increase = wad_ray_math.ray_mul(
            a=previous_scaled_balance,
            b=index,
        ) - wad_ray_math.ray_mul(
            a=previous_scaled_balance,
            b=debt_position.last_index or 0,
        )

        discount_scaled = 0

        # Update last_index to match contract behavior:
        # _userState[user].additionalData = index.toUint128(); # noqa: ERA001
        debt_position.last_index = index

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

    Returns 0 if discount mechanism is deprecated (revision 4+).
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
    *,
    w3: Web3,
    user: AaveV3UsersTable,
    discount_rate_strategy: ChecksumAddress | None,
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

    # Skip if discount mechanism is not supported (revision 4+)
    if discount_rate_strategy is None:
        return

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
    *,
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
    discount_token: ChecksumAddress | None,
    discount_rate_strategy: ChecksumAddress | None,
    event_data: DebtBurnEvent,
    user: AaveV3UsersTable,
    scaled_token_revision: int,
    debt_position: AaveV3DebtPositionsTable,
    state_block: int,
    effective_discount: int,
    skip_discount_refresh: bool = False,
    tx_context: TransactionContext | None = None,
    log_index: int | None = None,
) -> UserOperation:
    """
    Determine the user operation that triggered a GHO vToken Burn event and apply balance delta.

    The effective_discount should be the discount percent in effect at the start of the
    transaction, not the potentially updated value from a DiscountPercentUpdated event
    in the same transaction.

    When tx_context and log_index are provided, accounts for pending stkAAVE transfers
    that may have occurred due to reentrancy (where the GHO contract sees post-transfer
    balances before the Transfer event is emitted).
    """

    wad_ray_math_library, percentage_math_library = _get_math_libraries(scaled_token_revision)

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
        # Skip if there's a DiscountPercentUpdated event for this user in this transaction,
        # as the event provides the authoritative discount value
        if not skip_discount_refresh:
            discount_token_balance = _get_or_init_stk_aave_balance(
                user=user,
                discount_token=discount_token,
                block_number=state_block,
                w3=w3,
                tx_context=tx_context,
                log_index=log_index,
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
        # Update the discount percentage for the new balance
        # Skip if there's a DiscountPercentUpdated event for this user in this transaction,
        # as the event provides the authoritative discount value
        if not skip_discount_refresh:
            discount_token_balance = _get_or_init_stk_aave_balance(
                user=user,
                discount_token=discount_token,
                block_number=state_block,
                w3=w3,
                tx_context=tx_context,
                log_index=log_index,
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

    elif scaled_token_revision >= 4:
        # Revision 4+: Discount mechanism deprecated
        discount_scaled = 0  # No discount mechanism in revision 4+

        # uint256 amountToBurn = amount - balanceIncrease;
        requested_amount = event_data.value + event_data.balance_increase

        # uint256 amountScaled = amount.rayDiv(index);
        amount_scaled = wad_ray_math_library.ray_div(
            a=requested_amount,
            b=event_data.index,
        )

        # uint256 previousScaledBalance = super.balanceOf(user);
        previous_scaled_balance = debt_position.balance

        # No discount in revision 4+, simply burn amount_scaled
        # _burn(user, amountScaled.toUint128());
        balance_delta = -amount_scaled

        # No discount refresh needed for revision 4+

    else:
        msg = f"Unknown token revision: {scaled_token_revision}"
        raise ValueError(msg)

    if VerboseConfig.is_verbose(
        user_address=user.address, tx_hash=event_in_process["transactionHash"]
    ):
        logger.info(f"{debt_position.balance=}")
        logger.info(f"{debt_position.balance + balance_delta=}")
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


def _process_transaction_with_context(
    *,
    tx_context: TransactionContext,
    market: AaveV3MarketTable,
    session: Session,
    w3: Web3,
    gho_asset: AaveGhoTokenTable,
) -> None:
    """Process all events in a transaction using pre-built TransactionContext.

    Events are processed in chronological order with assertions that classifying
    events were pre-fetched and exist in the transaction context.
    """
    # Capture user discount percents before processing events
    # This ensures calculations use the discount in effect at the start of the transaction

    # First, build a map of discount updates in this transaction to get old values
    # Track all updates with their log indices to handle multiple updates per transaction
    for event in tx_context.events:
        topic = event["topics"][0]
        if topic == AaveV3Event.DISCOUNT_PERCENT_UPDATED.value:
            user_address = _decode_address(event["topics"][1])
            (old_discount_percent,) = eth_abi.abi.decode(types=["uint256"], data=event["data"])
            tx_context.discount_updated_users.add(user_address)

            # Store update with its log index to track discount changes over time
            if user_address not in tx_context.discount_updates_by_log_index:
                tx_context.discount_updates_by_log_index[user_address] = []
            tx_context.discount_updates_by_log_index[user_address].append((
                event["logIndex"],
                old_discount_percent,
            ))

    # Sort updates by log index for each user
    for user_address in tx_context.discount_updates_by_log_index:
        tx_context.discount_updates_by_log_index[user_address].sort(key=itemgetter(0))

    for event in tx_context.events:
        topic = event["topics"][0]
        event_address = get_checksum_address(event["address"])

        # Capture GHO user discount percents for mint/burn events
        if (
            topic
            in {
                AaveV3Event.SCALED_TOKEN_MINT.value,
                AaveV3Event.SCALED_TOKEN_BURN.value,
            }
            and event_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS
        ):
            # Mint event: topics[1] = caller, topics[2] = onBehalfOf (user)
            # Burn event: topics[1] = from (user), topics[2] = target
            if topic == AaveV3Event.SCALED_TOKEN_MINT.value:
                user_address = _decode_address(event["topics"][2])
            else:  # SCALED_TOKEN_BURN
                user_address = _decode_address(event["topics"][1])
            if user_address not in tx_context.user_discounts:
                # If there are DiscountPercentUpdated events for this user in this
                # transaction, use the OLD discount value that was in effect at the
                # start of the transaction (before any updates in this tx)
                user = session.scalar(
                    select(AaveV3UsersTable).where(
                        AaveV3UsersTable.address == user_address,
                        AaveV3UsersTable.market_id == market.id,
                    )
                )
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
                if not _is_discount_supported(market):
                    # Discount mechanism deprecated (revision 4+)
                    tx_context.user_discounts[user_address] = 0
                    continue

                try:
                    (discount_percent,) = raw_call(
                        w3=w3,
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

    # Process all events in chronological order
    for event in tx_context.events:
        topic = event["topics"][0]
        event_address = get_checksum_address(event["address"])

        context = EventHandlerContext(
            w3=w3,
            event=event,
            market=market,
            session=session,
            gho_asset=gho_asset,
            contract_address=event_address,
            tx_context=tx_context,
        )

        # Dispatch to appropriate handler
        if topic == AaveV3Event.TRANSFER.value and event_address == (
            gho_asset.v_gho_discount_token if gho_asset else None
        ):
            _process_stk_aave_transfer_event(context)
        elif topic == AaveV3Event.DISCOUNT_PERCENT_UPDATED.value:
            _process_discount_percent_updated_event(context)
        elif topic == AaveV3Event.SCALED_TOKEN_MINT.value:
            _process_scaled_token_mint_event(context)
        elif topic == AaveV3Event.SCALED_TOKEN_BURN.value:
            _process_scaled_token_burn_event(context)
        elif topic == AaveV3Event.SCALED_TOKEN_BALANCE_TRANSFER.value:
            _process_scaled_token_balance_transfer_event(context)
        elif topic == AaveV3Event.USER_E_MODE_SET.value:
            _process_user_e_mode_set_event(context)
        elif topic == AaveV3Event.RESERVE_DATA_UPDATED.value:
            _process_reserve_data_update_event(context)
        elif topic == AaveV3Event.UPGRADED.value:
            _process_scaled_token_upgrade_event(context)
        elif topic == AaveV3Event.DISCOUNT_RATE_STRATEGY_UPDATED.value:
            _process_discount_rate_strategy_updated_event(context)
        elif topic == AaveV3Event.DISCOUNT_TOKEN_UPDATED.value:
            _process_discount_token_updated_event(context)


def _process_staked_aave_event(
    context: EventHandlerContext,
    *,
    discount_token: ChecksumAddress | None,
    discount_rate_strategy: ChecksumAddress | None,
    event_data: DebtMintEvent,
    user: AaveV3UsersTable,
    scaled_token_revision: int,
    debt_position: AaveV3DebtPositionsTable,
) -> UserOperation | None:
    """
    Process a GHO vToken Mint event triggered by an AAVE staking event or stkAAVE transfer.

    This occurs when updateDiscountDistribution is triggered externally, resulting in a Mint event
    where value equals balanceIncrease.

    Uses pre-categorized stkAAVE events from TransactionContext.
    """

    # Skip if discount mechanism is not supported (revision 4+)
    if discount_token is None or discount_rate_strategy is None:
        return None

    if VerboseConfig.is_verbose(
        user_address=user.address, tx_hash=event_in_process["transactionHash"]
    ):
        logger.info("_process_staked_aave_event")

    # Get classifying stkAAVE events from pre-built transaction context
    # These are already sorted by priority: STAKED/REDEEM > TRANSFER
    if context.tx_context is None:
        return None

    accessory_events = _get_stk_aave_classifying_events(context.tx_context, user.address)
    if not accessory_events:
        # All accessory events were filtered out (e.g., transfers already processed)
        # Fall back to standard mint processing
        return None

    discount_token_info_event = accessory_events[0]

    match discount_token_info_event["topics"][0]:
        case AaveV3Event.STAKED.value:
            return _process_aave_stake(
                context,
                discount_token=discount_token,
                discount_rate_strategy=discount_rate_strategy,
                event_data=event_data,
                recipient=user,
                scaled_token_revision=scaled_token_revision,
                debt_position=debt_position,
                triggering_event=discount_token_info_event,
            )
        case AaveV3Event.REDEEM.value:
            return _process_aave_redeem(
                context,
                discount_token=discount_token,
                discount_rate_strategy=discount_rate_strategy,
                event_data=event_data,
                sender=user,
                scaled_token_revision=scaled_token_revision,
                debt_position=debt_position,
                triggering_event=discount_token_info_event,
            )
        case AaveV3Event.TRANSFER.value:
            return _process_staked_aave_transfer(
                context,
                discount_token=discount_token,
                discount_rate_strategy=discount_rate_strategy,
                event_data=event_data,
                scaled_token_revision=scaled_token_revision,
                debt_position=debt_position,
                triggering_event=discount_token_info_event,
            )
        case _:
            msg = "Should be unreachable"
            raise ValueError(msg)


def _process_aave_stake(
    context: EventHandlerContext,
    *,
    discount_token: ChecksumAddress,
    discount_rate_strategy: ChecksumAddress,
    event_data: DebtMintEvent,
    recipient: AaveV3UsersTable,
    scaled_token_revision: int,
    debt_position: AaveV3DebtPositionsTable,
    triggering_event: LogReceipt,
) -> UserOperation:
    """
    Process a GHO vToken Mint event triggered by an AAVE staking event.

    This handles the discount distribution update when a user stakes AAVE tokens.

    Reference:
    ```
    event Staked(
        address indexed from,
        address indexed to,
        uint256 assets,
        uint256 shares
    );
    ```
    """

    operation: UserOperation = UserOperation.AAVE_STAKED

    wad_ray_math_library, percentage_math_library = _get_math_libraries(scaled_token_revision)

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

    _log_if_verbose(
        recipient.address,
        event_in_process["transactionHash"],
        f"{event_data.caller=}",
        f"{event_data.on_behalf_of=}",
    )

    # uint256 recipientPreviousScaledBalance = super.balanceOf(recipient)
    recipient_previous_scaled_balance = recipient_debt_position.balance

    if recipient_previous_scaled_balance > 0:
        _log_if_verbose(
            recipient.address,
            event_in_process["transactionHash"],
            f"{recipient_previous_scaled_balance=}",
            "Processing case: recipientPreviousScaledBalance > 0",
        )

        # Get the discount in effect at transaction start (before any DiscountPercentUpdated
        # events in this transaction). The contract uses the old discount for accrual,
        # then applies the new discount for future calculations.
        old_discount = (
            context.tx_context.user_discounts.get(recipient.address, recipient.gho_discount)
            if context.tx_context is not None
            else recipient.gho_discount
        )

        # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(...)
        # Use the OLD discount for interest accrual, matching contract behavior
        recipient_discount_scaled = _accrue_debt_on_action(
            debt_position=recipient_debt_position,
            percentage_math=percentage_math_library,
            wad_ray_math=wad_ray_math_library,
            previous_scaled_balance=recipient_previous_scaled_balance,
            discount_percent=old_discount,
            index=event_data.index,
            token_revision=scaled_token_revision,
        )

        # _burn(recipient, discountScaled.toUint128()) # noqa:ERA001
        recipient_debt_position.balance -= recipient_discount_scaled
        recipient_new_scaled_balance = recipient_debt_position.balance
        recipient_debt_position.last_index = event_data.index

        # Update the discount percentage for the new balance
        recipient_previous_discount_percent = old_discount
        # Skip discount refresh if there's a DiscountPercentUpdated event for recipient
        if recipient.address not in (
            context.tx_context.discount_updated_users if context.tx_context else set()
        ):
            recipient_new_discount_token_balance = _get_or_init_stk_aave_balance(
                user=recipient,
                discount_token=discount_token,
                block_number=triggering_event["blockNumber"],
                w3=context.w3,
                tx_context=context.tx_context,
                log_index=context.event["logIndex"],
            )
            _refresh_discount_rate(
                w3=context.w3,
                user=recipient,
                discount_rate_strategy=discount_rate_strategy,
                discount_token_balance=recipient_new_discount_token_balance,
                scaled_debt_balance=recipient_debt_position.balance,
                debt_index=event_data.index,
                wad_ray_math=wad_ray_math_library,
            )
        recipient_new_discount_percent = recipient.gho_discount

        _log_if_verbose(
            recipient.address,
            event_in_process["transactionHash"],
            f"{recipient.address=}",
            f"{requested_amount=}",
            f"{recipient_previous_scaled_balance=}",
            f"{recipient_new_scaled_balance=}",
            (
                f"Discount Percent: {recipient_previous_discount_percent} -> "
                f"{recipient_new_discount_percent}"
            ),
        )

    return operation


def _process_aave_redeem(
    context: EventHandlerContext,
    *,
    discount_token: ChecksumAddress,
    discount_rate_strategy: ChecksumAddress,
    event_data: DebtMintEvent,
    sender: AaveV3UsersTable,
    scaled_token_revision: int,
    debt_position: AaveV3DebtPositionsTable,
    triggering_event: LogReceipt,
) -> UserOperation:
    """
    Process a GHO vToken Mint event triggered by an AAVE redemption event.

    This handles the discount distribution update when a user redeems stkAAVE tokens.

    Reference:
    ```
    event Redeem(
        address indexed from,
        address indexed to,
        uint256 assets,
        uint256 shares
    );
    ```
    """

    operation: UserOperation = UserOperation.AAVE_REDEEM

    wad_ray_math_library, percentage_math_library = _get_math_libraries(scaled_token_revision)

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
        w3=context.w3,
        tx_context=context.tx_context,
        log_index=context.event["logIndex"],
    )

    _log_if_verbose(
        sender.address,
        event_in_process["transactionHash"],
        f"{event_data.caller=}",
        f"{event_data.on_behalf_of=}",
    )

    # uint256 recipientPreviousScaledBalance = super.balanceOf(recipient)
    sender_previous_scaled_balance = sender_debt_position.balance

    if sender_previous_scaled_balance > 0:
        _log_if_verbose(
            sender.address,
            event_in_process["transactionHash"],
            "Processing case: senderPreviousScaledBalance > 0",
        )

        # Get the discount in effect at transaction start (before any DiscountPercentUpdated
        # events in this transaction). The contract uses the old discount for accrual,
        # then applies the new discount for future calculations.
        old_discount = (
            context.tx_context.user_discounts.get(sender.address, sender.gho_discount)
            if context.tx_context is not None
            else sender.gho_discount
        )

        # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(...)
        # Use the OLD discount for interest accrual, matching contract behavior
        sender_discount_scaled = _accrue_debt_on_action(
            debt_position=sender_debt_position,
            percentage_math=percentage_math_library,
            wad_ray_math=wad_ray_math_library,
            previous_scaled_balance=sender_previous_scaled_balance,
            discount_percent=old_discount,
            index=event_data.index,
            token_revision=scaled_token_revision,
        )

        # _burn(recipient, discountScaled.toUint128()) # noqa: ERA001
        sender_debt_position.balance -= sender_discount_scaled
        sender_debt_position.last_index = event_data.index

        sender_previous_discount_percent = old_discount
        # Skip discount refresh if there's a DiscountPercentUpdated event for sender
        if sender.address not in (
            context.tx_context.discount_updated_users if context.tx_context else set()
        ):
            _refresh_discount_rate(
                w3=context.w3,
                user=sender,
                discount_rate_strategy=discount_rate_strategy,
                discount_token_balance=sender_discount_token_balance - requested_amount,
                scaled_debt_balance=sender_debt_position.balance,
                debt_index=event_data.index,
                wad_ray_math=wad_ray_math_library,
            )
        sender_new_discount_percent = sender.gho_discount

        _log_if_verbose(
            sender.address,
            event_in_process["transactionHash"],
            f"{sender.address=}",
            f"{sender_discount_token_balance=}",
            f"{requested_amount=}",
            f"{sender_previous_scaled_balance=}",
            (
                f"Discount Percent: {sender_previous_discount_percent} -> "
                f"{sender_new_discount_percent}"
            ),
        )

    return operation


def _process_staked_aave_transfer(
    context: EventHandlerContext,
    *,
    discount_token: ChecksumAddress,
    discount_rate_strategy: ChecksumAddress,
    event_data: DebtMintEvent,
    scaled_token_revision: int,
    debt_position: AaveV3DebtPositionsTable,
    triggering_event: LogReceipt,
) -> UserOperation:
    """
    Process a GHO vToken Mint event triggered by an stkAAVE transfer.

    This handles the discount distribution update when stkAAVE is transferred between users.
    Both sender and recipient's discount rates are updated.

    Reference:
    ```
    event Transfer(
        address indexed from,
        address indexed to,
        uint256 value
    );
    ```
    """

    wad_ray_math_library, percentage_math_library = _get_math_libraries(scaled_token_revision)

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
    sender = _get_or_create_user(
        context=context,
        market=context.market,
        user_address=from_address,
        w3=context.w3,
        block_number=triggering_event["blockNumber"],
    )
    recipient = _get_or_create_user(
        context=context,
        market=context.market,
        user_address=to_address,
        w3=context.w3,
        block_number=triggering_event["blockNumber"],
    )
    assert sender is not recipient

    sender_debt_position = _get_or_create_debt_position(
        session=context.session,
        user=sender,
        asset_id=debt_position.asset_id,
    )
    recipient_debt_position = _get_or_create_debt_position(
        session=context.session,
        user=recipient,
        asset_id=debt_position.asset_id,
    )

    # Get the discount token balances (will reflect prior transfers in same block
    # since stkAAVE events are processed immediately)
    sender_discount_token_balance = _get_or_init_stk_aave_balance(
        user=sender,
        discount_token=discount_token,
        block_number=triggering_event["blockNumber"],
        w3=context.w3,
        tx_context=context.tx_context,
        log_index=context.event["logIndex"],
    )
    recipient_discount_token_balance = _get_or_init_stk_aave_balance(
        user=recipient,
        discount_token=discount_token,
        block_number=triggering_event["blockNumber"],
        w3=context.w3,
        tx_context=context.tx_context,
        log_index=context.event["logIndex"],
    )

    _log_if_verbose(
        sender.address,
        event_in_process["transactionHash"],
        f"stkAAVE Transfer: {from_address} -> {to_address}",
        f"{sender.address}: {sender_discount_token_balance} stkAAVE",
        f"{recipient.address}: {recipient_discount_token_balance} stkAAVE",
        f"{event_data.caller=}",
        f"{event_data.on_behalf_of=}",
    )

    # uint256 senderPreviousScaledBalance = super.balanceOf(sender)
    sender_previous_scaled_balance = sender_debt_position.balance
    _log_if_verbose(
        sender.address,
        event_in_process["transactionHash"],
        f"{sender_previous_scaled_balance=}",
    )

    # uint256 recipientPreviousScaledBalance = super.balanceOf(recipient)
    recipient_previous_scaled_balance = recipient_debt_position.balance
    _log_if_verbose(
        recipient.address,
        event_in_process["transactionHash"],
        f"{recipient_previous_scaled_balance=}",
    )

    # uint256 index = POOL.getReserveNormalizedVariableDebt(_underlyingAsset)
    # (accessed through event_data.index)

    # Multiple Mint events can be emitted by a single TX!
    # Only update the position if the event corresponds to the sender or receiver.
    # A sender->receiver Transfer where both users hold a balance should emit two events.
    if sender_previous_scaled_balance > 0:
        _log_if_verbose(
            sender.address,
            event_in_process["transactionHash"],
            "Processing case: senderPreviousScaledBalance > 0",
        )

        # Get the discount in effect at transaction start (before any DiscountPercentUpdated
        # events in this transaction). The contract uses the old discount for accrual,
        # then applies the new discount for future calculations.
        sender_old_discount = (
            context.tx_context.user_discounts.get(sender.address, sender.gho_discount)
            if context.tx_context is not None
            else sender.gho_discount
        )

        # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(...)
        # Use the OLD discount for interest accrual, matching contract behavior
        sender_discount_scaled = _accrue_debt_on_action(
            debt_position=sender_debt_position,
            percentage_math=percentage_math_library,
            wad_ray_math=wad_ray_math_library,
            previous_scaled_balance=sender_previous_scaled_balance,
            discount_percent=sender_old_discount,
            index=event_data.index,
            token_revision=scaled_token_revision,
        )

        # _burn(sender, discountScaled.toUint128()) # noqa: ERA001
        sender_debt_position.balance -= sender_discount_scaled
        sender_debt_position.last_index = event_data.index

        sender_previous_discount_percent = sender_old_discount
        # Skip discount refresh if there's a DiscountPercentUpdated event for sender
        if sender.address not in (
            context.tx_context.discount_updated_users if context.tx_context else set()
        ):
            _refresh_discount_rate(
                w3=context.w3,
                user=sender,
                discount_rate_strategy=discount_rate_strategy,
                discount_token_balance=sender_discount_token_balance,
                scaled_debt_balance=sender_debt_position.balance,
                debt_index=event_data.index,
                wad_ray_math=wad_ray_math_library,
            )
        sender_new_discount_percent = sender.gho_discount

        _log_if_verbose(
            sender.address,
            event_in_process["transactionHash"],
            f"{sender.address=}",
            f"{sender_discount_token_balance=}",
            f"{requested_amount=}",
            f"{recipient_previous_scaled_balance=}",
            (
                f"Discount Percent: {sender_previous_discount_percent} -> "
                f"{sender_new_discount_percent}"
            ),
        )

    if recipient_previous_scaled_balance > 0:
        _log_if_verbose(
            recipient.address,
            event_in_process["transactionHash"],
            "Processing case: recipientPreviousScaledBalance > 0",
        )

        # Get the discount in effect at transaction start (before any DiscountPercentUpdated
        # events in this transaction). The contract uses the old discount for accrual,
        # then applies the new discount for future calculations.
        recipient_old_discount = (
            context.tx_context.user_discounts.get(recipient.address, recipient.gho_discount)
            if context.tx_context is not None
            else recipient.gho_discount
        )

        # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(...)
        # Use the OLD discount for interest accrual, matching contract behavior
        recipient_discount_scaled = _accrue_debt_on_action(
            debt_position=recipient_debt_position,
            percentage_math=percentage_math_library,
            wad_ray_math=wad_ray_math_library,
            previous_scaled_balance=recipient_previous_scaled_balance,
            discount_percent=recipient_old_discount,
            index=event_data.index,
            token_revision=scaled_token_revision,
        )

        # _burn(recipient, discountScaled.toUint128()) # noqa:ERA001
        recipient_debt_position.balance -= recipient_discount_scaled
        recipient_new_scaled_balance = recipient_debt_position.balance
        recipient_debt_position.last_index = event_data.index

        recipient_previous_discount_percent = recipient_old_discount
        # Skip discount refresh if there's a DiscountPercentUpdated event for recipient,
        # unless they were also involved in a stkAAVE transfer (balance changed, needs recalc)
        if recipient.address not in (
            context.tx_context.discount_updated_users if context.tx_context else set()
        ) or (
            context.tx_context is not None
            and recipient.address in context.tx_context.stk_aave_transfer_users
        ):
            _refresh_discount_rate(
                w3=context.w3,
                user=recipient,
                discount_rate_strategy=discount_rate_strategy,
                discount_token_balance=recipient_discount_token_balance,
                scaled_debt_balance=recipient_new_scaled_balance,
                debt_index=event_data.index,
                wad_ray_math=wad_ray_math_library,
            )
        recipient_new_discount_percent = recipient.gho_discount

        _log_if_verbose(
            recipient.address,
            event_in_process["transactionHash"],
            f"{recipient.address=}",
            f"{recipient_discount_token_balance=}",
            f"{requested_amount=}",
            f"{recipient_previous_scaled_balance=}",
            f"{recipient_new_scaled_balance=}",
            (
                f"Discount Percent: {recipient_previous_discount_percent} -> "
                f"{recipient_new_discount_percent}"
            ),
        )

    return UserOperation.STKAAVE_TRANSFER


def _process_gho_debt_mint(
    context: EventHandlerContext,
    *,
    discount_token: ChecksumAddress | None,
    discount_rate_strategy: ChecksumAddress | None,
    event_data: DebtMintEvent,
    user: AaveV3UsersTable,
    scaled_token_revision: int,
    debt_position: AaveV3DebtPositionsTable,
    state_block: int,
    effective_discount: int,
) -> UserOperation:
    """
    Determine the user operation that triggered a GHO vToken Mint event and apply balance delta.

    Mint events can be triggered by different operations:
    - GHO BORROW: value > balanceIncrease (new debt issued)
    - GHO REPAY: balanceIncrease > value (debt partially repaid)
    - AAVE STAKED/REDEEM/STAKED TRANSFER: value == balanceIncrease with caller == ZERO_ADDRESS
      and accessory Staked/Redeem/Transfer events present

    The effective_discount should be the discount percent in effect at the start of the
    transaction, not the potentially updated value from a DiscountPercentUpdated event
    in the same transaction.
    """

    wad_ray_math_library, percentage_math_library = _get_math_libraries(scaled_token_revision)

    user_operation: UserOperation
    requested_amount = 0  # Default for interest accrual case

    if scaled_token_revision == 1:
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

        # Update the discount percentage for the new balance
        # Skip discount refresh if there's a DiscountPercentUpdated event for user
        if user.address not in (
            context.tx_context.discount_updated_users if context.tx_context else set()
        ):
            discount_token_balance = _get_or_init_stk_aave_balance(
                user=user,
                discount_token=discount_token,
                block_number=state_block,
                w3=context.w3,
                tx_context=context.tx_context,
                log_index=context.event["logIndex"],
            )
            _refresh_discount_rate(
                w3=context.w3,
                user=user,
                discount_rate_strategy=discount_rate_strategy,
                discount_token_balance=discount_token_balance,
                scaled_debt_balance=debt_position.balance + balance_delta,
                debt_index=event_data.index,
                wad_ray_math=wad_ray_math_library,
            )

    elif scaled_token_revision in {2, 3}:
        # A user who is accruing GHO vToken debt is labeled the "recipient". A Mint event can be
        # emitted through several paths, and the GHO discount accounting depends on the discount
        # token balance. This variable tracks the role of the user holding the position.

        # Check for accessory events (Staked/Redeem/Transfer) to detect staking-related mints
        # These events may not have value == balance_increase, so check explicitly
        accessory_events = (
            _get_stk_aave_classifying_events(context.tx_context, user.address)
            if context.tx_context is not None
            else []
        )

        # Check for accessory events (Staked/Redeem/Transfer) to detect staking-related mints
        # Skip if this is pure interest accrual (value == balance_increase), which should be
        # processed as a standard interest accrual event, not a staking event
        if (
            accessory_events
            and event_data.caller == ZERO_ADDRESS
            and event_data.value != event_data.balance_increase
        ):
            # This Mint was triggered by staking/transfer - use specialized handler
            staked_aave_result = _process_staked_aave_event(
                context,
                discount_token=discount_token,
                discount_rate_strategy=discount_rate_strategy,
                event_data=event_data,
                user=user,
                scaled_token_revision=scaled_token_revision,
                debt_position=debt_position,
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
            _log_if_verbose(
                user.address,
                event_in_process["transactionHash"],
                "_mintScaled (GHO vToken rev 2)",
                f"{user_operation=}",
            )

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

            _log_if_verbose(
                user.address,
                event_in_process["transactionHash"],
                f"{previous_scaled_balance=}",
                f"{debt_position.last_index=}",
                f"{event_data.index=}",
                f"{requested_amount=}",
                f"{amount_scaled=}",
                f"{discount_scaled=}",
                f"{balance_delta=}",
            )

            # Update the discount percentage for the new balance
            # Skip discount refresh if there's a DiscountPercentUpdated event for user
            if user.address not in (
                context.tx_context.discount_updated_users if context.tx_context else set()
            ):
                discount_token_balance = _get_or_init_stk_aave_balance(
                    user=user,
                    discount_token=discount_token,
                    block_number=state_block,
                    w3=context.w3,
                    tx_context=context.tx_context,
                    log_index=context.event["logIndex"],
                )
                _refresh_discount_rate(
                    w3=context.w3,
                    user=user,
                    discount_rate_strategy=discount_rate_strategy,
                    discount_token_balance=discount_token_balance,
                    scaled_debt_balance=debt_position.balance + balance_delta,
                    debt_index=event_data.index,
                    wad_ray_math=wad_ray_math_library,
                )

        elif event_data.balance_increase > event_data.value:
            user_operation = UserOperation.GHO_REPAY
            _log_if_verbose(
                user.address,
                event_in_process["transactionHash"],
                "_burnScaled (GHO vToken rev 2)",
                f"{user_operation=}",
            )

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
                # _burn(user, previousScaledBalance.toUint128()); # noqa:ERA001
                balance_delta = -previous_scaled_balance
            else:
                # _burn(user, (amountScaled + discountScaled).toUint128()); # noqa:ERA001
                balance_delta = -(amount_scaled + discount_scaled)

            _log_if_verbose(
                user.address,
                event_in_process["transactionHash"],
                f"{discount_scaled=}",
                f"{requested_amount=}",
                f"{amount_scaled=}",
                f"{balance_delta=}",
            )

            # Update the discount percentage for the new balance
            # Skip discount refresh if there's a DiscountPercentUpdated event for user
            if user.address not in (
                context.tx_context.discount_updated_users if context.tx_context else set()
            ):
                discount_token_balance = _get_or_init_stk_aave_balance(
                    user=user,
                    discount_token=discount_token,
                    block_number=state_block,
                    w3=context.w3,
                    tx_context=context.tx_context,
                    log_index=context.event["logIndex"],
                )
                _refresh_discount_rate(
                    w3=context.w3,
                    user=user,
                    discount_rate_strategy=discount_rate_strategy,
                    discount_token_balance=discount_token_balance,
                    scaled_debt_balance=debt_position.balance + balance_delta,
                    debt_index=event_data.index,
                    wad_ray_math=wad_ray_math_library,
                )

        elif event_data.value == event_data.balance_increase:
            # Pure interest accrual - emitted from _accrueDebtOnAction during discount updates
            # This occurs when a user stakes/redeems stkAAVE, triggering a discount rate change
            # The Mint event has value == balanceIncrease (no actual borrow/repay)
            user_operation = UserOperation.GHO_INTEREST_ACCRUAL
            _log_if_verbose(
                user.address,
                event_in_process["transactionHash"],
                "_accrueDebtOnAction (GHO vToken rev 2)",
                f"{user_operation=}",
            )

            # uint256 previousScaledBalance = super.balanceOf(user);
            previous_scaled_balance = debt_position.balance

            # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(...)
            # When stkAAVE is redeemed and discount changes, the contract:
            # 1. Accrues interest using the OLD discount rate
            # 2. Burns the discount amount from the scaled balance
            # 3. The net effect is the balance decreases by discount_scaled
            discount_scaled = _accrue_debt_on_action(
                debt_position=debt_position,
                percentage_math=percentage_math_library,
                wad_ray_math=wad_ray_math_library,
                previous_scaled_balance=previous_scaled_balance,
                discount_percent=effective_discount,
                index=event_data.index,
                token_revision=scaled_token_revision,
            )

            # The balance decreases by the discount amount (burned by contract)
            balance_delta = -discount_scaled

            _log_if_verbose(
                user.address,
                event_in_process["transactionHash"],
                f"{previous_scaled_balance=}",
                f"{debt_position.last_index=}",
                f"{event_data.index=}",
                f"{discount_scaled=}",
                f"{balance_delta=}",
            )

            # Update the discount percentage for the new balance
            # Skip discount refresh if there's a DiscountPercentUpdated event for user
            if user.address not in (
                context.tx_context.discount_updated_users if context.tx_context else set()
            ):
                discount_token_balance = _get_or_init_stk_aave_balance(
                    user=user,
                    discount_token=discount_token,
                    block_number=state_block,
                    w3=context.w3,
                    tx_context=context.tx_context,
                    log_index=context.event["logIndex"],
                )
                _refresh_discount_rate(
                    w3=context.w3,
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

    elif scaled_token_revision >= 4:
        # Revision 4+: Discount mechanism deprecated
        # Standard borrow/repay/interest logic without discount
        discount_scaled = 0  # No discount mechanism in revision 4+

        if event_data.value > event_data.balance_increase:
            # GHO BORROW - emitted in _mintScaled
            # uint256 amountToMint = amount + balanceIncrease;
            user_operation = UserOperation.GHO_BORROW
            requested_amount = event_data.value - event_data.balance_increase

            # uint256 amountScaled = amount.rayDiv(index);
            amount_scaled = wad_ray_math_library.ray_div(
                a=requested_amount,
                b=event_data.index,
            )

            # No discount in revision 4+, balance_delta = amount_scaled
            balance_delta = amount_scaled

        elif event_data.balance_increase > event_data.value:
            # GHO REPAY - emitted in _burnScaled
            # uint256 amountToMint = balanceIncrease - amount;
            user_operation = UserOperation.GHO_REPAY
            requested_amount = event_data.balance_increase - event_data.value

            # uint256 amountScaled = amount.rayDiv(index);
            amount_scaled = wad_ray_math_library.ray_div(
                a=requested_amount,
                b=event_data.index,
            )

            # No discount in revision 4+, balance decreases by amount_scaled
            # _burn(user, amountScaled.toUint128());
            balance_delta = -amount_scaled

        elif event_data.value == event_data.balance_increase:
            # Pure interest accrual - no actual borrow/repay
            # Emitted from interest accrual without user action
            user_operation = UserOperation.GHO_INTEREST_ACCRUAL
            requested_amount = 0

            # Calculate interest accrual without discount
            previous_scaled_balance = debt_position.balance
            balance_increase = wad_ray_math_library.ray_mul(
                a=previous_scaled_balance,
                b=event_data.index,
            ) - wad_ray_math_library.ray_mul(
                a=previous_scaled_balance,
                b=debt_position.last_index or 0,
            )

            # balanceIncrease is the interest accrued (in underlying)
            # Convert back to scaled: balance_increase / index
            balance_increase_scaled = wad_ray_math_library.ray_div(
                a=balance_increase,
                b=event_data.index,
            )

            balance_delta = balance_increase_scaled

            # Update last_index
            debt_position.last_index = event_data.index

        else:
            msg = (
                "Unexpected Mint event state: "
                f"value={event_data.value}, balance_increase={event_data.balance_increase}"
            )
            raise ValueError(msg)

        # Note: For revision 4+, discount_rate_strategy is None, so
        # _refresh_discount_rate returns early. No need to call it.

    else:
        msg = f"Unknown token revision: {scaled_token_revision}"
        raise ValueError(msg)

    _log_if_verbose(
        user.address,
        event_in_process["transactionHash"],
        f"{user.address=}",
        f"{user.gho_discount=}",
        f"{discount_scaled=}",
        f"{balance_delta=}",
        f"{discount_token=}",
        f"{discount_rate_strategy=}",
        f"{state_block=}",
    )

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

    market.contracts.append(
        AaveV3ContractsTable(
            market_id=market.id,
            name=proxy_name,
            address=proxy_address,
            revision=revision,
        )
    )


def _process_umbrella_creation_event(
    *,
    session: Session,
    market: AaveV3MarketTable,
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
    (decoded_proxy_id,) = eth_abi.abi.decode(types=["bytes32"], data=event["topics"][1])

    if decoded_proxy_id != proxy_id:
        return

    new_address = _decode_address(event["topics"][3])

    if (
        session.scalar(
            select(AaveV3ContractsTable).where(AaveV3ContractsTable.address == new_address)
        )
        is not None
    ):
        return

    market.contracts.append(
        AaveV3ContractsTable(
            market_id=market.id,
            name=proxy_name,
            address=new_address,
            revision=None,
        )
    )


def _process_discount_percent_updated_event(
    context: EventHandlerContext,
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

    user_address = _decode_address(context.event["topics"][1])

    (old_discount_percent,) = eth_abi.abi.decode(types=["uint256"], data=context.event["data"])
    (new_discount_percent,) = eth_abi.abi.decode(types=["uint256"], data=context.event["topics"][2])

    # Create user if they don't exist - discount can be updated before first debt event
    user = _get_or_create_user(
        context=context,
        market=context.market,
        user_address=user_address,
        w3=context.w3,
        block_number=context.event["blockNumber"],
    )

    if VerboseConfig.is_verbose(
        user_address=user_address, tx_hash=event_in_process["transactionHash"]
    ):
        logger.info(f"DiscountPercentUpdated: {user_address}")
        logger.info(
            f"  BEFORE UPDATE: user.id={user.id}, "
            f"user.gho_discount={user.gho_discount}, id(user)={id(user)}"
        )

    # With transaction-level processing, the discount is updated here
    # and subsequent Mint/Burn events in the same transaction will see
    # the updated value
    if VerboseConfig.is_verbose(
        user_address=user_address, tx_hash=event_in_process["transactionHash"]
    ):
        logger.info(
            f"DiscountPercentUpdated for {user_address}: "
            f"{user.gho_discount} -> {new_discount_percent} "
            f"(user.id={user.id})"
        )
    user.gho_discount = new_discount_percent
    context.session.flush()  # Ensure the discount update is visible to subsequent queries

    if VerboseConfig.is_verbose(
        user_address=user_address, tx_hash=event_in_process["transactionHash"]
    ):
        logger.info(f"DiscountPercentUpdated: {user_address}")
        logger.info(f"  old_discount_percent={old_discount_percent}")
        logger.info(f"  new_discount_percent={new_discount_percent}")


def _process_collateral_mint_event(
    *,
    session: Session,
    market: AaveV3MarketTable,
    user: AaveV3UsersTable,
    collateral_asset: AaveV3AssetsTable,
    token_address: ChecksumAddress,
    event_amount: int,
    balance_increase: int,
    index: int,
    event: LogReceipt,
    caller_address: ChecksumAddress,
    tx_context: TransactionContext | None = None,
) -> None:
    """Process a collateral (aToken) mint event."""

    # A SCALED_TOKEN_MINT can be triggered by:
    # - SUPPLY: user supplies collateral, onBehalfOf = user
    # - WITHDRAW: user withdraws, interest is minted first, caller = withdraw initiator
    # - REPAY: repayWithATokens accrues interest first, then repays with aTokens
    # - Interest accrual: value == balanceIncrease (no Pool event)
    # - mintToTreasury: Pool contract mints accrued fees to treasury (caller = Pool)
    reserve_address = get_checksum_address(collateral_asset.underlying_token.address)

    # Track matched pool event and calculated scaled delta
    matched_pool_event: LogReceipt | None = None
    scaled_delta: int | None = None

    # Skip verification for pure interest accrual (value == balanceIncrease)
    # These mints don't have a corresponding Pool event
    if event_amount != balance_increase:
        # Check if this is a mintToTreasury call (caller is the Pool contract itself)
        # The Pool contract emits MintedToTreasury events, not SUPPLY/WITHDRAW
        pool_contract = _get_contract(market=market, contract_name="POOL")
        if caller_address == pool_contract.address:
            # Skip verification for treasury mints - they have no SUPPLY/WITHDRAW event
            pass
        elif tx_context is not None:
            # Use pre-fetched pool events from transaction context
            # Try to find matching event: SUPPLY first, then WITHDRAW, then REPAY
            found = False
            for expected_type, check_user in [
                (AaveV3Event.SUPPLY.value, user.address),
                (AaveV3Event.WITHDRAW.value, caller_address),
                (AaveV3Event.REPAY.value, user.address),
            ]:
                for pool_event in tx_context.pool_events:
                    # Skip pool events that have already been matched to other Mint/Burn events
                    if tx_context.matched_pool_events.get(pool_event["logIndex"], False):
                        continue
                    if _matches_pool_event(pool_event, expected_type, check_user, reserve_address):
                        found = True
                        matched_pool_event = pool_event
                        # Mark this pool event as matched so it won't be used again
                        tx_context.matched_pool_events[pool_event["logIndex"]] = True
                        break
                if found:
                    break

            if not found:
                # No matching Pool event found - this is an error
                available = [e["topics"][0].hex()[:10] for e in tx_context.pool_events]
                msg = (
                    f"No matching Pool event for collateral mint in tx {tx_context.tx_hash.hex()}. "
                    f"User: {user.address}, Reserve: {reserve_address}. "
                    f"Available: {available}"
                )
                raise AssertionError(msg)
        else:
            # Fallback: try each event type in order
            # This path should not be reached in normal operation with transaction-level processing
            pass

    # For SUPPLY events, calculate scaled amount from raw underlying amount
    # to avoid rounding errors from reverse-calculating from event.value
    if (
        matched_pool_event is not None
        and matched_pool_event["topics"][0] == AaveV3Event.SUPPLY.value
    ):
        # SUPPLY event data: (address caller, uint256 amount)
        # Note: reserve, onBehalfOf are indexed and in topics
        (_, raw_amount) = eth_abi.abi.decode(
            types=["address", "uint256"],
            data=matched_pool_event["data"],
        )
        # Calculate scaled amount: ray_div(raw_amount, index)
        # This matches the contract's calculation in getATokenMintScaledAmount
        ray_math, _ = _get_math_libraries(collateral_asset.a_token_revision)
        scaled_delta = ray_math.ray_div(a=raw_amount, b=index)
    if (
        matched_pool_event is not None
        and matched_pool_event["topics"][0] == AaveV3Event.SUPPLY.value
    ):
        # SUPPLY event data: (address caller, uint256 amount)
        # Note: reserve, onBehalfOf are indexed and in topics
        (_, raw_amount) = eth_abi.abi.decode(
            types=["address", "uint256"],
            data=matched_pool_event["data"],
        )
        # Calculate scaled amount: ray_div(raw_amount, index)
        # This matches the contract's calculation in getATokenMintScaledAmount
        ray_math, _ = _get_math_libraries(collateral_asset.a_token_revision)
        scaled_delta = ray_math.ray_div(a=raw_amount, b=index)
        if VerboseConfig.is_verbose(user_address=user.address):
            logger.info(
                f"SUPPLY: raw_amount={raw_amount}, index={index}, scaled_delta={scaled_delta}"
            )

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
        scaled_delta=scaled_delta,
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
    context: EventHandlerContext,
    *,
    user: AaveV3UsersTable,
    debt_asset: AaveV3AssetsTable,
    token_address: ChecksumAddress,
    event_amount: int,
    balance_increase: int,
    index: int,
    caller_address: ChecksumAddress,
) -> None:
    """Process a GHO debt (vToken) mint event."""

    # Verify the corresponding Pool Borrow event exists for GHO
    # Use the underlying GHO token address, not the variable debt token address
    # because Pool BORROW events reference the underlying asset
    # Skip verification for pure interest accrual (value == balanceIncrease)
    # These mints don't have a corresponding Pool event
    if event_amount != balance_increase:
        reserve_address = get_checksum_address(debt_asset.underlying_token.address)
        try:
            _verify_pool_event_for_transaction(
                w3=context.w3,
                market=context.market,
                event=context.event,
                expected_event_type=AaveV3Event.BORROW.value,
                user_address=user.address,
                reserve_address=reserve_address,
            )
        except ValueError:
            # If no BORROW event, try REPAY (for interest accrual during repayment)
            _verify_pool_event_for_transaction(
                w3=context.w3,
                market=context.market,
                event=context.event,
                expected_event_type=AaveV3Event.REPAY.value,
                user_address=user.address,
                reserve_address=reserve_address,
            )

    debt_position = _get_or_create_debt_position(
        session=context.session, user=user, asset_id=debt_asset.id
    )

    gho_asset = context.gho_asset

    # Discount mechanism is only supported in revisions 2 and 3
    # Revision 4+ deprecates discounts, so these will be None
    if _is_discount_supported(context.market):
        assert gho_asset.v_gho_discount_token is not None, "GHO discount token not initialized"
        assert gho_asset.v_gho_discount_rate_strategy is not None, (
            "GHO discount rate strategy not initialized"
        )
        discount_token = gho_asset.v_gho_discount_token
        discount_rate_strategy = gho_asset.v_gho_discount_rate_strategy
    else:
        discount_token = None
        discount_rate_strategy = None

    user_starting_amount = debt_position.balance

    # Use the discount percent in effect at transaction start, not the potentially
    # updated value from a DiscountPercentUpdated event in the same transaction
    effective_discount = (
        context.tx_context.user_discounts.get(user.address, user.gho_discount)
        if context.tx_context is not None
        else user.gho_discount
    )

    user_operation = _process_gho_debt_mint(
        context,
        discount_token=discount_token,
        discount_rate_strategy=discount_rate_strategy,
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
        state_block=context.event["blockNumber"],
        effective_discount=effective_discount,
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
            tx_hash=context.event["transactionHash"],
            block_info=f"{context.event['blockNumber']}.{context.event['logIndex']}",
            balance_delta=debt_position.balance - user_starting_amount,
        )

    assert debt_position.balance >= 0


def _process_standard_debt_mint_event(
    *,
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
    w3: Web3,
) -> None:
    """Process a standard debt (vToken) mint event (non-GHO)."""

    # A SCALED_TOKEN_MINT for debt can be triggered by:
    # - BORROW: onBehalfOf is usually the user, but when borrowed via an adapter,
    #   onBehalfOf can be the adapter (caller)
    # - REPAY: interest accrues before repayment, minting debt tokens first
    reserve_address = get_checksum_address(debt_asset.underlying_token.address)

    # Skip verification for pure interest accrual (value == balanceIncrease)
    # These mints don't have a corresponding Pool event
    if event_amount != balance_increase:
        # Try with user.address first (most common case - BORROW)
        try:
            _verify_pool_event_for_transaction(
                w3=w3,
                market=market,
                event=event,
                expected_event_type=AaveV3Event.BORROW.value,
                user_address=user.address,
                reserve_address=reserve_address,
            )
        except ValueError:
            # If that fails, try with caller_address (adapter case)
            try:
                _verify_pool_event_for_transaction(
                    w3=w3,
                    market=market,
                    event=event,
                    expected_event_type=AaveV3Event.BORROW.value,
                    user_address=caller_address,
                    reserve_address=reserve_address,
                )
            except ValueError:
                # If no BORROW event, try REPAY (for interest accrual during repayment)
                _verify_pool_event_for_transaction(
                    w3=w3,
                    market=market,
                    event=event,
                    expected_event_type=AaveV3Event.REPAY.value,
                    user_address=user.address,
                    reserve_address=reserve_address,
                )

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

    Reference:
    ```
    event Mint(
        address indexed caller,
        address indexed onBehalfOf,
        uint256 value,
        uint256 balanceIncrease,
        uint256 index
    );
    ```
    """

    caller_address = _decode_address(context.event["topics"][1])
    on_behalf_of_address = _decode_address(context.event["topics"][2])

    user = _get_or_create_user(
        context=context,
        market=context.market,
        user_address=on_behalf_of_address,
        w3=context.w3,
        block_number=context.event["blockNumber"],
    )

    event_amount, balance_increase, index = _decode_uint_values(event=context.event, num_values=3)

    token_address = get_checksum_address(context.event["address"])
    collateral_asset, debt_asset = _get_scaled_token_asset_by_address(
        market=context.market, token_address=token_address
    )

    if collateral_asset is not None:
        try:
            _process_collateral_mint_event(
                session=context.session,
                market=context.market,
                user=user,
                collateral_asset=collateral_asset,
                token_address=token_address,
                event_amount=event_amount,
                balance_increase=balance_increase,
                index=index,
                event=context.event,
                caller_address=caller_address,
                tx_context=context.tx_context,
            )
        except ValueError:
            # If collateral verification fails and we have a debt asset, try that
            if debt_asset is not None:
                if token_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS:
                    _process_gho_debt_mint_event(
                        context,
                        user=user,
                        debt_asset=debt_asset,
                        token_address=token_address,
                        event_amount=event_amount,
                        balance_increase=balance_increase,
                        index=index,
                        caller_address=caller_address,
                    )
                else:
                    _process_standard_debt_mint_event(
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
                        w3=context.w3,
                    )
            else:
                raise

    elif debt_asset is not None:
        if token_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS:
            _process_gho_debt_mint_event(
                context,
                user=user,
                debt_asset=debt_asset,
                token_address=token_address,
                event_amount=event_amount,
                balance_increase=balance_increase,
                index=index,
                caller_address=caller_address,
            )
        else:
            _process_standard_debt_mint_event(
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
                w3=context.w3,
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
    market: AaveV3MarketTable,
    user: AaveV3UsersTable,
    collateral_asset: AaveV3AssetsTable,
    token_address: ChecksumAddress,
    event_amount: int,
    balance_increase: int,
    index: int,
    event: LogReceipt,
    w3: Web3,
) -> None:
    """Process a collateral (aToken) burn event."""

    # A SCALED_TOKEN_BURN can be triggered by:
    # - WITHDRAW: user withdraws collateral (has WITHDRAW event)
    # - REPAY with useATokens=true: burns aTokens directly (no WITHDRAW event)
    reserve_address = get_checksum_address(collateral_asset.underlying_token.address)

    # Try WITHDRAW first (most common case)
    try:
        _verify_pool_event_for_transaction(
            w3=w3,
            market=market,
            event=event,
            expected_event_type=AaveV3Event.WITHDRAW.value,
            user_address=user.address,
            reserve_address=reserve_address,
        )
    except ValueError:
        try:
            # If no WITHDRAW event, try REPAY (for repay with aTokens)
            _verify_pool_event_for_transaction(
                w3=w3,
                market=market,
                event=event,
                expected_event_type=AaveV3Event.REPAY.value,
                user_address=user.address,
                reserve_address=reserve_address,
            )
        except ValueError:
            # If no REPAY event, check for AddressSet (for UMBRELLA setup)
            _verify_address_set_for_transaction(
                w3=w3,
                market=market,
                event=event,
                user_address=user.address,
            )

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
    context: EventHandlerContext,
    *,
    user: AaveV3UsersTable,
    debt_asset: AaveV3AssetsTable,
    token_address: ChecksumAddress,
    event_amount: int,
    balance_increase: int,
    index: int,
    from_address: ChecksumAddress,
    target_address: ChecksumAddress,
) -> None:
    """Process a GHO debt (vToken) burn event."""

    # Verify the corresponding Pool Repay event exists for GHO
    # Use the underlying GHO token address, not the variable debt token address
    # because Pool REPAY events reference the underlying asset
    reserve_address = get_checksum_address(debt_asset.underlying_token.address)
    _verify_pool_event_for_transaction(
        w3=context.w3,
        market=context.market,
        event=context.event,
        expected_event_type=AaveV3Event.REPAY.value,
        user_address=user.address,
        reserve_address=reserve_address,
    )

    debt_position = context.session.scalar(
        select(AaveV3DebtPositionsTable).where(
            AaveV3DebtPositionsTable.user_id == user.id,
            AaveV3DebtPositionsTable.asset_id == debt_asset.id,
        )
    )
    assert debt_position is not None

    gho_asset = context.gho_asset

    # Discount mechanism is only supported in revisions 2 and 3
    # Revision 4+ deprecates discounts, so these will be None
    if _is_discount_supported(context.market):
        assert gho_asset.v_gho_discount_token is not None, "GHO discount token not initialized"
        assert gho_asset.v_gho_discount_rate_strategy is not None, (
            "GHO discount rate strategy not initialized"
        )
        discount_token = gho_asset.v_gho_discount_token
        discount_rate_strategy = gho_asset.v_gho_discount_rate_strategy
    else:
        discount_token = None
        discount_rate_strategy = None

    user_starting_amount = debt_position.balance

    # Use the discount percent in effect at transaction start, not the potentially
    # updated value from a DiscountPercentUpdated event in the same transaction
    effective_discount = (
        context.tx_context.user_discounts.get(user.address, user.gho_discount)
        if context.tx_context is not None
        else user.gho_discount
    )

    # Skip discount refresh if there's a DiscountPercentUpdated event for this user
    # in this transaction, as the event provides the authoritative discount value
    skip_discount_refresh = (
        context.tx_context is not None and user.address in context.tx_context.discount_updated_users
    )

    user_operation = _process_gho_debt_burn(
        w3=context.w3,
        discount_token=discount_token,
        discount_rate_strategy=discount_rate_strategy,
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
        state_block=context.event["blockNumber"],
        effective_discount=effective_discount,
        skip_discount_refresh=skip_discount_refresh,
        tx_context=context.tx_context,
        log_index=context.event["logIndex"],
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
            tx_hash=context.event["transactionHash"],
            block_info=f"{context.event['blockNumber']}.{context.event['logIndex']}",
            balance_delta=debt_position.balance - user_starting_amount,
        )

    assert debt_position.balance >= 0


def _process_standard_debt_burn_event(
    *,
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
    w3: Web3,
) -> None:
    """Process a standard debt (vToken) burn event (non-GHO)."""

    # Verify the corresponding Pool Repay event exists
    _verify_pool_event_for_transaction(
        w3=w3,
        market=market,
        event=event,
        expected_event_type=AaveV3Event.REPAY.value,
        user_address=user.address,
        reserve_address=get_checksum_address(debt_asset.underlying_token.address),
    )

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

    Reference:
    ```
    event Burn(
        address indexed from,
        address indexed target,
        uint256 value,
        uint256 balanceIncrease,
        uint256 index
    );
    ```
    """

    from_address = _decode_address(context.event["topics"][1])
    target_address = _decode_address(context.event["topics"][2])

    event_amount, balance_increase, index = _decode_uint_values(event=context.event, num_values=3)

    user = _get_or_create_user(
        context=context,
        market=context.market,
        user_address=from_address,
        w3=context.w3,
        block_number=context.event["blockNumber"],
    )

    token_address = get_checksum_address(context.event["address"])
    collateral_asset, debt_asset = _get_scaled_token_asset_by_address(
        market=context.market, token_address=token_address
    )

    if collateral_asset is not None:
        _process_collateral_burn_event(
            session=context.session,
            market=context.market,
            user=user,
            collateral_asset=collateral_asset,
            token_address=token_address,
            event_amount=event_amount,
            balance_increase=balance_increase,
            index=index,
            event=context.event,
            w3=context.w3,
        )

    elif debt_asset is not None:
        if token_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS:
            _process_gho_debt_burn_event(
                context,
                user=user,
                debt_asset=debt_asset,
                token_address=token_address,
                event_amount=event_amount,
                balance_increase=balance_increase,
                index=index,
                from_address=from_address,
                target_address=target_address,
            )
        else:
            _process_standard_debt_burn_event(
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
                w3=context.w3,
            )

    else:
        msg = f"Unknown token type for address {token_address}. Expected aToken or vToken."
        raise ValueError(msg)


def _process_scaled_token_balance_transfer_event(
    context: EventHandlerContext,
) -> None:
    """
    Process a scaled token balance transfer.

    The transfer() function is disabled by vToken (debt) contracts to prohibit offloading debt to
    another user. Therefore BalanceTransfer events will only be emitted by aToken (collateral)
    contracts.

    Reference:
    ```
    event BalanceTransfer(
        address indexed from,
        address indexed to,
        uint256 value,
        uint256 index
    );
    ```
    """

    from_address = _decode_address(context.event["topics"][1])
    to_address = _decode_address(context.event["topics"][2])

    event_amount, index = _decode_uint_values(event=context.event, num_values=2)

    # Zero-amount transfers have no balance effect, but the Aave contract still updates
    # last_index for both sender and recipient (ScaledBalanceTokenBase._transfer lines 152-153).
    # ref: TX 0xd007ede5e5dcff5e30904db3d66a8e1926fd75742ca838636dd2d5730140dcc6
    # ref: TX 0x1d30b4d7ff65d58fa2314b23744202f80b36760143b3e779f31d9010925d8b7e

    aave_asset = _get_asset_by_token_type(
        market=context.market,
        token_address=get_checksum_address(context.event["address"]),
        token_type=TokenType.COLLATERAL,
    )
    assert aave_asset is not None

    from_user = _get_or_create_user(
        context=context,
        market=context.market,
        user_address=from_address,
        w3=context.w3,
        block_number=context.event["blockNumber"],
    )
    assert from_user is not None

    from_user_position = context.session.scalar(
        select(AaveV3CollateralPositionsTable).where(
            AaveV3CollateralPositionsTable.user_id == from_user.id,
            AaveV3CollateralPositionsTable.asset_id == aave_asset.id,
        )
    )

    # Zero-amount transfers can be performed by users who don't have a position, so skip further
    # processing
    # ref: TX 0x37CB48358CE4E26AC1193415003A33538B239E7D6D5FB826998666912937B71D
    if from_user_position is None:
        return

    # Always update last_index (even for zero-amount transfers)
    from_user_position.last_index = index
    to_user = _get_or_create_user(
        context=context,
        market=context.market,
        user_address=to_address,
        w3=context.w3,
        block_number=context.event["blockNumber"],
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

    # Always update last_index (even for zero-amount transfers)
    to_user_position.last_index = index

    # Skip balance updates and logging for zero-amount transfers
    if event_amount == 0:
        return

    from_user_starting_amount = from_user_position.balance
    from_user_position.balance -= event_amount

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
                AaveV3Event.SUPPLY.value,
                AaveV3Event.WITHDRAW.value,
                AaveV3Event.BORROW.value,
                AaveV3Event.REPAY.value,
                AaveV3Event.LIQUIDATION_CALL.value,
                AaveV3Event.RESERVE_DATA_UPDATED.value,
                AaveV3Event.USER_E_MODE_SET.value,
            ]
        ],
    )


def _fetch_pool_configurator_events(
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
        topic_signature=[[AaveV3Event.RESERVE_INITIALIZED.value]],
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
                AaveV3Event.SCALED_TOKEN_MINT.value,
                AaveV3Event.SCALED_TOKEN_BURN.value,
                AaveV3Event.SCALED_TOKEN_BALANCE_TRANSFER.value,
                AaveV3Event.UPGRADED.value,
                AaveV3Event.DISCOUNT_PERCENT_UPDATED.value,
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
                AaveV3Event.STAKED.value,
                AaveV3Event.REDEEM.value,
                AaveV3Event.TRANSFER.value,
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
                AaveV3Event.PROXY_CREATED.value,
                AaveV3Event.POOL_CONFIGURATOR_UPDATED.value,
                AaveV3Event.POOL_DATA_PROVIDER_UPDATED.value,
                AaveV3Event.POOL_UPDATED.value,
                AaveV3Event.ADDRESS_SET.value,
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
                AaveV3Event.DISCOUNT_RATE_STRATEGY_UPDATED.value,
                AaveV3Event.DISCOUNT_TOKEN_UPDATED.value,
            ]
        ],
    )


def _build_transaction_contexts(
    *,
    events: list[LogReceipt],
    market: AaveV3MarketTable,
    session: Session,
    w3: Web3,
    gho_asset: AaveGhoTokenTable,
    known_scaled_token_addresses: set[ChecksumAddress],
) -> dict[HexBytes, TransactionContext]:
    """Group events by transaction with full categorization.

    Every AaveV3Event enum value must have categorization logic in this function.
    Missing categorization will be caught at module load by _validate_event_coverage().
    """
    contexts: dict[HexBytes, TransactionContext] = {}

    for event in sorted(events, key=_event_sort_key):
        tx_hash = event["transactionHash"]

        if tx_hash not in contexts:
            contexts[tx_hash] = TransactionContext(
                w3=w3,
                tx_hash=tx_hash,
                block_number=event["blockNumber"],
                events=[],
                market=market,
                session=session,
                gho_asset=gho_asset,
            )

        ctx = contexts[tx_hash]
        ctx.events.append(event)

        # Categorize by event type
        topic = event["topics"][0]
        event_address = get_checksum_address(event["address"])

        # Skip scaled token events from non-token addresses (e.g., Pool contract)
        # The Pool contract emits Mint/Burn/Transfer events with the same topic
        # signature as aToken/vToken events but should not be processed
        if (
            topic
            in {
                AaveV3Event.SCALED_TOKEN_MINT.value,
                AaveV3Event.SCALED_TOKEN_BURN.value,
                AaveV3Event.SCALED_TOKEN_BALANCE_TRANSFER.value,
            }
            and event_address not in known_scaled_token_addresses
        ):
            continue

        if topic in {
            AaveV3Event.SUPPLY.value,
            AaveV3Event.WITHDRAW.value,
            AaveV3Event.BORROW.value,
            AaveV3Event.REPAY.value,
            AaveV3Event.LIQUIDATION_CALL.value,
        }:
            ctx.pool_events.append(event)
        elif topic == AaveV3Event.STAKED.value:
            ctx.stk_aave_stakes.append(event)
        elif topic == AaveV3Event.REDEEM.value:
            ctx.stk_aave_redeems.append(event)
        elif topic == AaveV3Event.TRANSFER.value and event_address == (
            gho_asset.v_gho_discount_token if gho_asset else None
        ):
            ctx.stk_aave_transfers.append(event)
            # Track users involved in this transfer
            from_addr = _decode_address(event["topics"][1])
            to_addr = _decode_address(event["topics"][2])
            if from_addr != ZERO_ADDRESS:
                ctx.stk_aave_transfer_users.add(from_addr)
            if to_addr != ZERO_ADDRESS:
                ctx.stk_aave_transfer_users.add(to_addr)
            to_addr = _decode_address(event["topics"][2])
            if from_addr != ZERO_ADDRESS:
                ctx.stk_aave_transfer_users.add(from_addr)
            if to_addr != ZERO_ADDRESS:
                ctx.stk_aave_transfer_users.add(to_addr)
        elif topic == AaveV3Event.SCALED_TOKEN_MINT.value:
            if event_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS:
                ctx.gho_mints.append(event)
            else:
                ctx.collateral_mints.append(event)
        elif topic == AaveV3Event.SCALED_TOKEN_BURN.value:
            if event_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS:
                ctx.gho_burns.append(event)
            else:
                ctx.collateral_burns.append(event)
        elif topic == AaveV3Event.SCALED_TOKEN_BALANCE_TRANSFER.value:
            ctx.balance_transfers.append(event)
        elif topic in {
            AaveV3Event.DISCOUNT_PERCENT_UPDATED.value,
            AaveV3Event.DISCOUNT_RATE_STRATEGY_UPDATED.value,
            AaveV3Event.DISCOUNT_TOKEN_UPDATED.value,
        }:
            ctx.discount_updates.append(event)
        elif topic == AaveV3Event.RESERVE_DATA_UPDATED.value:
            ctx.reserve_data_updates.append(event)
        elif topic == AaveV3Event.USER_E_MODE_SET.value:
            ctx.user_e_mode_sets.append(event)
        elif topic == AaveV3Event.UPGRADED.value:
            ctx.upgraded_events.append(event)
        else:
            msg = f"Could not identify topic: {topic}"
            raise ValueError(msg)

    return contexts


def _get_stk_aave_classifying_events(
    tx_context: TransactionContext,
    user_address: ChecksumAddress,
) -> list[LogReceipt]:
    """Get stkAAVE events that classify this user's operation.

    Priority: STAKED/REDEEM > TRANSFER
    """
    events: list[LogReceipt] = []

    # STAKED events (user is the 'to' address)
    for event in tx_context.stk_aave_stakes:
        to_addr = _decode_address(event["topics"][2])
        if to_addr == user_address:
            events.append(event)

    # REDEEM events (user is the 'from' address)
    for event in tx_context.stk_aave_redeems:
        from_addr = _decode_address(event["topics"][1])
        if from_addr == user_address:
            events.append(event)

    # TRANSFER events (user is either from or to)
    for event in tx_context.stk_aave_transfers:
        from_addr = _decode_address(event["topics"][1])
        to_addr = _decode_address(event["topics"][2])
        if user_address in {from_addr, to_addr}:
            events.append(event)

    # Sort by priority
    priority = {
        AaveV3Event.STAKED.value: 0,
        AaveV3Event.REDEEM.value: 0,
        AaveV3Event.TRANSFER.value: 1,
    }
    events.sort(key=lambda e: priority.get(e["topics"][0], 2))

    return events


def update_aave_market(
    *,
    w3: Web3,
    start_block: int,
    end_block: int,
    market: AaveV3MarketTable,
    session: Session,
    verify: bool,
    no_progress: bool,
) -> None:
    """
    Update the Aave V3 market.

    Processes events in three phases:
    1. Bootstrap: Fetch and process proxy creation events to discover Pool and PoolConfigurator
       contracts.
    2. Asset Discovery: Fetch all targeted events and build transaction contexts
    3. User Event Processing: Process transactions with assertions that classifying events exist
    """

    logger.info(f"Updating market {market.id}: block range {start_block}-{end_block}")

    # Phase 1
    for event in _fetch_address_provider_events(
        w3=w3,
        provider_address=_get_contract(
            market=market,
            contract_name="POOL_ADDRESS_PROVIDER",
        ).address,
        start_block=start_block,
        end_block=end_block,
    ):
        topic = event["topics"][0]

        if topic == AaveV3Event.PROXY_CREATED.value:
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
        elif topic == AaveV3Event.POOL_UPDATED.value:
            _update_contract_revision(
                w3=w3,
                market=market,
                contract_name="POOL",
                new_address=_decode_address(event["topics"][2]),
                revision_function_prototype="POOL_REVISION",
            )
        elif topic == AaveV3Event.POOL_CONFIGURATOR_UPDATED.value:
            _update_contract_revision(
                w3=w3,
                market=market,
                contract_name="POOL_CONFIGURATOR",
                new_address=_decode_address(event["topics"][2]),
                revision_function_prototype="CONFIGURATOR_REVISION",
            )
        elif topic == AaveV3Event.POOL_DATA_PROVIDER_UPDATED.value:
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
        elif topic == AaveV3Event.ADDRESS_SET.value:
            _process_umbrella_creation_event(
                session=session,
                market=market,
                event=event,
                proxy_name="UMBRELLA",
                proxy_id=eth_abi.abi.encode(["bytes32"], [b"UMBRELLA"]),
            )

    # Phase 2
    try:
        pool_configurator = _get_contract(market=market, contract_name="POOL_CONFIGURATOR")
    except ValueError:
        # Configurator not initialized yet, skip reserve initialization
        pool_configurator = None
    if pool_configurator is not None:
        for event in _fetch_pool_configurator_events(
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
        pool = _get_contract(market=market, contract_name="POOL")
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
            if topic == AaveV3Event.DISCOUNT_TOKEN_UPDATED.value:
                # Update the discount token directly
                new_discount_token_address = _decode_address(event["topics"][2])
                gho_asset.v_gho_discount_token = new_discount_token_address
                logger.info(
                    f"SET NEW DISCOUNT TOKEN: {_decode_address(event['topics'][1])} -> "
                    f"{new_discount_token_address}"
                )

        all_events.extend(
            _fetch_stk_aave_events(
                w3=w3,
                discount_token=gho_asset.v_gho_discount_token,
                start_block=start_block,
                end_block=end_block,
            )
        )

    # Extract all user addresses involved in events for targeted verification
    users_in_chunk: set[ChecksumAddress] = set()
    for event in all_events:
        users_in_chunk.update(_extract_user_addresses_from_event(event))

    # Group the events into transaction bundles with a shared context
    tx_contexts = _build_transaction_contexts(
        events=all_events,
        market=market,
        session=session,
        w3=w3,
        gho_asset=gho_asset,
        known_scaled_token_addresses=known_scaled_token_addresses,
    )

    processed_txs: set[HexBytes] = set()

    # Process all events within transaction context to ensure correct ordering
    # and atomic state updates
    for event in tqdm.tqdm(
        sorted(all_events, key=_event_sort_key),
        desc="Processing events",
        leave=False,
        disable=no_progress,
    ):
        tx_hash = event["transactionHash"]

        # Skip if this transaction was already processed
        if tx_hash in processed_txs:
            continue

        global event_in_process  # noqa: PLW0603
        event_in_process = event

        # Process entire transaction atomically with full context
        tx_context = tx_contexts[tx_hash]
        _process_transaction_with_context(
            tx_context=tx_context,
            market=market,
            session=session,
            w3=w3,
            gho_asset=gho_asset,
        )
        processed_txs.add(tx_hash)

    # Perform verification at chunk boundary for users involved in this chunk
    if verify:
        session.flush()
        _verify_scaled_token_positions(
            w3=w3,
            market=market,
            session=session,
            position_table=AaveV3CollateralPositionsTable,
            block_number=end_block,
            no_progress=no_progress,
            user_addresses=users_in_chunk,
        )
        _verify_scaled_token_positions(
            w3=w3,
            market=market,
            session=session,
            position_table=AaveV3DebtPositionsTable,
            block_number=end_block,
            no_progress=no_progress,
            user_addresses=users_in_chunk,
        )
        _verify_stk_aave_balances(
            w3=w3,
            session=session,
            market=market,
            gho_asset=gho_asset,
            block_number=end_block,
            no_progress=no_progress,
            user_addresses=users_in_chunk,
        )
        _verify_gho_discount_amounts(
            w3=w3,
            session=session,
            market=market,
            gho_asset=gho_asset,
            block_number=end_block,
            no_progress=no_progress,
            user_addresses=users_in_chunk,
        )

    _cleanup_zero_balance_positions(
        session=session,
        market=market,
        no_progress=no_progress,
    )
