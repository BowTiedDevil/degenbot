# TODO: add progress bars
# TODO: add scraper for collateral usage enabled events

import operator
from dataclasses import dataclass
from enum import Enum
from typing import TYPE_CHECKING, Literal, Protocol, TypedDict, cast

import click
import eth_abi.abi
import tqdm
from eth_typing import ChainId, ChecksumAddress
from hexbytes import HexBytes
from sqlalchemy import select
from sqlalchemy.orm import Session
from web3 import Web3
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
from degenbot.constants import ZERO_ADDRESS
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

type BalanceDelta = int
type TokenRevision = int
type UserOperation = Literal["DEPOSIT", "WITHDRAW", "BORROW", "REPAY", "GHO BORROW", "GHO REPAY"]

# GhoVariableDebtToken
# Rev 1: 0x3FEaB6F8510C73E05b8C0Fdf96Df012E3A144319
# Rev 2: 0x7aa606b1B341fFEeAfAdbbE4A2992EFB35972775

# GhoDiscountRateStrategy
# 0x4C38Ec4D1D2068540DfC11DFa4de41F733DDF812

GHO_VARIABLE_DEBT_TOKEN_ADDRESS = get_checksum_address("0x786dBff3f1292ae8F92ea68Cf93c30b34B1ed04B")

# TODO: debug variable, remove this after testing is complete
last_event_processed: LogReceipt | None = None

# Remove before using in production
VERBOSE_ALL = True

# User addresses that trigger verbose logging
VERBOSE_USERS: set[ChecksumAddress] = {
    get_checksum_address("0xA988afe23CD41Ca490f5cA239E0d1852B3e47c25"),
}


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


@cli.group()
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


@aave.command("update")
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
def aave_update(chunk_size: int, to_block: str) -> None:
    """
    Update positions for active Aave markets.
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
                desc="Processing new blocks",
                total=last_block - initial_start_block + 1,
                bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
                leave=False,
            )

            block_pbar.n = working_start_block - initial_start_block
            block_pbar.refresh()

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
                        )
                    except Exception as e:  # noqa: BLE001
                        logger.info(f"Processing failed on event: {last_event_processed}")
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

                if working_end_block == last_block:
                    break
                working_start_block = working_end_block + 1

                block_pbar.n = working_end_block - initial_start_block
                block_pbar.refresh()

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

    # Per EIP-1967, the implementation address is found by retrieving the storage slot
    # 0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc
    (atoken_implementation_address,) = eth_abi.abi.decode(
        types=["address"],
        data=w3.eth.get_storage_at(
            account=get_checksum_address(a_token_address),
            position=int.from_bytes(
                HexBytes(0x360894A13BA1A3210667C828492DB98DCA3E2076CC3735A920A3CA505D382BBC)
            ),
            block_identifier=event["blockNumber"],
        ),
    )
    atoken_implementation_address = get_checksum_address(atoken_implementation_address)

    (vtoken_implementation_address,) = eth_abi.abi.decode(
        types=["address"],
        data=w3.eth.get_storage_at(
            account=get_checksum_address(v_token_address),
            position=int.from_bytes(
                HexBytes(0x360894A13BA1A3210667C828492DB98DCA3E2076CC3735A920A3CA505D382BBC)
            ),
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
    event: LogReceipt,
    market: AaveV3MarketTable,
    session: Session,
) -> None:
    """
    Process a UserEModeSet event to update a user's E-Mode category.
    """

    # EVENT DEFINITION
    # event UserEModeSet(
    #     address indexed user,
    #     uint8 categoryId
    # );

    user_address = _decode_address(event["topics"][1])

    (e_mode,) = eth_abi.abi.decode(types=["uint8"], data=event["data"])

    user = _get_or_create_user(
        session=session,
        market=market,
        user_address=user_address,
    )
    user.e_mode = e_mode


def _process_discount_token_updated_event(
    event: LogReceipt,
    market: AaveV3MarketTable,
    session: Session,
) -> None:
    """
    Process a DiscountTokenUpdated event to set the GHO vToken discount token
    """

    # EVENT DEFINITION
    # event DiscountTokenUpdated(
    #     address indexed oldDiscountToken,
    #     address indexed newDiscountToken
    # );

    old_discount_token_address = _decode_address(event["topics"][1])
    new_discount_token_address = _decode_address(event["topics"][2])

    # TODO: Confirm that AaveGhoTokenTable correctly handles GHO tokens shared across
    # multiple markets on the same chain. The current design assumes GHO is
    # chain-unique, which may need validation.

    # Get Aave's GHO token asset, then look up the underlying ERC-20 token ID to identify the
    # special attributes
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

    gho_asset.v_gho_discount_token = new_discount_token_address

    logger.info(
        f"SET NEW DISCOUNT TOKEN: {old_discount_token_address} -> {new_discount_token_address}"
    )


def _process_discount_rate_strategy_updated_event(
    event: LogReceipt,
    market: AaveV3MarketTable,
    session: Session,
) -> None:
    """
    Process a DiscountRateStrategyUpdated event to set the GHO vToken attribute
    """

    # EVENT DEFINITION
    # event DiscountRateStrategyUpdated(
    #     address indexed oldDiscountRateStrategy,
    #     address indexed newDiscountRateStrategy
    # );

    (old_discount_rate_strategy_address,) = eth_abi.abi.decode(
        types=["address"], data=event["topics"][1]
    )
    old_discount_rate_strategy_address = get_checksum_address(old_discount_rate_strategy_address)

    (new_discount_rate_strategy_address,) = eth_abi.abi.decode(
        types=["address"], data=event["topics"][2]
    )
    new_discount_rate_strategy_address = get_checksum_address(new_discount_rate_strategy_address)

    # TODO: Confirm that AaveGhoTokenTable correctly handles GHO tokens shared
    # across multiple markets on the same chain. The current design assumes GHO
    # is chain-unique, which may need validation.

    # Get Aave's GHO token asset, then look up the underlying ERC-20 token ID to identify the
    # special attributes
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

    gho_asset.v_gho_discount_rate_strategy = new_discount_rate_strategy_address

    logger.info(
        f"SET NEW DISCOUNT RATE STRATEGY: {old_discount_rate_strategy_address} -> {new_discount_rate_strategy_address}"
    )


def _process_reserve_data_update_event(
    event: LogReceipt,
    market: AaveV3MarketTable,
    session: Session,
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

    reserve_asset_address = _decode_address(event["topics"][1])

    asset_in_db = session.scalar(
        select(AaveV3AssetsTable)
        .join(
            Erc20TokenTable,
            AaveV3AssetsTable.underlying_asset_id == Erc20TokenTable.id,
        )
        .where(
            Erc20TokenTable.chain == market.chain_id,
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
    w3: Web3,
    event: LogReceipt,
    market: AaveV3MarketTable,
    session: Session,
) -> None:
    """
    Process an Upgraded event to update the aToken or vToken revision.
    """

    # EVENT DEFINITION
    # event Upgraded(
    #     address indexed implementation
    # );

    new_implementation_address = _decode_address(event["topics"][1])

    if (
        aave_collateral_asset := session.scalar(
            select(AaveV3AssetsTable)
            .join(
                Erc20TokenTable,
                AaveV3AssetsTable.a_token_id == Erc20TokenTable.id,
            )
            .where(
                Erc20TokenTable.chain == market.chain_id,
                Erc20TokenTable.address == get_checksum_address(event["address"]),
            )
        )
    ) is not None:
        (atoken_revision,) = raw_call(
            w3=w3,
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
        aave_debt_asset := session.scalar(
            select(AaveV3AssetsTable)
            .join(
                Erc20TokenTable,
                AaveV3AssetsTable.v_token_id == Erc20TokenTable.id,
            )
            .where(
                Erc20TokenTable.chain == market.chain_id,
                Erc20TokenTable.address == get_checksum_address(event["address"]),
            )
        )
    ) is not None:
        (vtoken_revision,) = raw_call(
            w3=w3,
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
        token_address = get_checksum_address(event["address"])
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
    user_id: int,
    asset_id: int,
) -> AaveV3CollateralPositionsTable:
    """
    Get existing collateral position or create new one with zero balance.
    """

    if (
        position := session.scalar(
            select(AaveV3CollateralPositionsTable).where(
                AaveV3CollateralPositionsTable.user_id == user_id,
                AaveV3CollateralPositionsTable.asset_id == asset_id,
            )
        )
    ) is None:
        position = AaveV3CollateralPositionsTable(user_id=user_id, asset_id=asset_id, balance=0)
        session.add(position)
    return position


def _get_or_create_debt_position(
    session: Session,
    user_id: int,
    asset_id: int,
) -> AaveV3DebtPositionsTable:
    """
    Get existing debt position or create new one with zero balance.
    """

    if (
        position := session.scalar(
            select(AaveV3DebtPositionsTable).where(
                AaveV3DebtPositionsTable.user_id == user_id,
                AaveV3DebtPositionsTable.asset_id == asset_id,
            )
        )
    ) is None:
        position = AaveV3DebtPositionsTable(user_id=user_id, asset_id=asset_id, balance=0)
        session.add(position)
    return position


def _verify_gho_discount_amounts(
    w3: Web3,
    market: AaveV3MarketTable,
    session: Session,
    users_to_check: dict[ChecksumAddress, int],
) -> None:
    """
    Verify that the GHO discount values match the contract.
    """

    for user_address, last_update_block in users_to_check.items():
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


def _verify_scaled_token_positions(
    w3: Web3,
    market: AaveV3MarketTable,
    session: Session,
    users_to_check: dict[ChecksumAddress, int],
    position_table: type[AaveV3CollateralPositionsTable | AaveV3DebtPositionsTable],
) -> None:
    """
    Verify that the database position balances match the contract.
    """

    for user_address, last_update_block in users_to_check.items():
        # logger.info(f"Verifying positions for {user_address} at block {last_update_block}")
        user = session.scalar(
            select(AaveV3UsersTable).where(
                AaveV3UsersTable.address == user_address,
                AaveV3UsersTable.market_id == market.id,
            )
        )
        assert user is not None

        for position in session.scalars(
            select(position_table).where(position_table.user_id == user.id)
        ):
            position = cast("AaveV3CollateralPositionsTable | AaveV3DebtPositionsTable", position)
            asset = session.scalar(
                select(AaveV3AssetsTable).where(
                    AaveV3AssetsTable.market_id == market.id,
                    AaveV3AssetsTable.id == position.asset_id,
                )
            )
            assert asset is not None

            if position_table is AaveV3CollateralPositionsTable:
                token_address = session.scalar(
                    select(Erc20TokenTable.address).where(
                        Erc20TokenTable.chain == market.chain_id,
                        Erc20TokenTable.id == asset.a_token_id,
                    )
                )
            elif position_table is AaveV3DebtPositionsTable:
                token_address = session.scalar(
                    select(Erc20TokenTable.address).where(
                        Erc20TokenTable.chain == market.chain_id,
                        Erc20TokenTable.id == asset.v_token_id,
                    )
                )
            else:
                msg = f"Unknown position table type: {position_table}"
                raise ValueError(msg)
            token_address = get_checksum_address(token_address)

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
                f"User {user_address}: {'collateral' if position_table is AaveV3CollateralPositionsTable else 'debt'} balance ({position.balance}) "
                f"does not match scaled token contract ({actual_scaled_balance}) "
                f"@ {token_address} at block {last_update_block}"
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
    value: int
    balance_increase: int
    index: int


@dataclass(frozen=True, slots=True)
class DebtBurnEvent:
    value: int
    balance_increase: int
    index: int


def _log_token_operation(
    *,
    user_operation: str,
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

    logger.info(user_operation)
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
) -> tuple[BalanceDelta, UserOperation]:
    """
    Determine the balance delta and user operation for scaled token events.
    """

    ray_math, _ = _get_math_libraries(scaled_token_revision)
    operation: UserOperation

    match event:
        case CollateralMintEvent():
            if event.balance_increase > event.value:
                requested_amount = event.balance_increase - event.value
                delta = -ray_math.ray_div(a=requested_amount, b=event.index)
                operation = "WITHDRAW"
            else:
                requested_amount = event.value - event.balance_increase
                delta = ray_math.ray_div(a=requested_amount, b=event.index)
                operation = "DEPOSIT"

        case CollateralBurnEvent():
            requested_amount = event.value + event.balance_increase
            delta = -ray_math.ray_div(a=requested_amount, b=event.index)
            operation = "WITHDRAW"

        case DebtMintEvent():
            if event.balance_increase > event.value:
                requested_amount = event.balance_increase - event.value
                delta = -ray_math.ray_div(a=requested_amount, b=event.index)
                operation = "REPAY"
            else:
                requested_amount = event.value - event.balance_increase
                delta = ray_math.ray_div(a=requested_amount, b=event.index)
                operation = "BORROW"

        case DebtBurnEvent():
            requested_amount = event.value + event.balance_increase
            delta = -ray_math.ray_div(a=requested_amount, b=event.index)
            operation = "REPAY"

    assert requested_amount >= 0

    return delta, operation


def _accrue_debt_on_action(
    *,
    debt_position: AaveV3DebtPositionsTable,
    percentage_math: PercentageMathLibrary,
    wad_ray_math: WadRayMathLibrary,
    previous_scaled_balance: int,
    discount_percent: int,
    index: int,
    token_revision: int,
) -> tuple[int, int]:

    if token_revision in {1, 2}:
        balance_increase = wad_ray_math.ray_mul(
            a=previous_scaled_balance,
            b=index,
        ) - wad_ray_math.ray_mul(
            a=previous_scaled_balance,
            b=debt_position.last_index or 0,
        )

        discount_scaled = 0
        if balance_increase != 0 and discount_percent != 0:
            discount = percentage_math.percent_mul(
                value=balance_increase,
                percentage=discount_percent,
            )
            discount_scaled = wad_ray_math.ray_div(a=discount, b=index)
            balance_increase -= discount

        debt_position.last_index = index

    else:
        raise ValueError(f"Unsupported token revision {token_revision}")

    return balance_increase, discount_scaled


def _get_discount_rate(
    w3: Web3,
    state_block: int,
    discount_rate_strategy: ChecksumAddress,
    debt_token_balance: int,
    discount_token_balance: int,
) -> int:
    # Get the discount percentage from the discount rate strategy contract
    (new_discount_percentage,) = raw_call(
        w3=w3,
        address=discount_rate_strategy,
        calldata=encode_function_calldata(
            function_prototype="calculateDiscountRate(uint256,uint256)",
            function_arguments=[debt_token_balance, discount_token_balance],
        ),
        return_types=["uint256"],
        block_identifier=state_block,
    )
    # logger.info(
    #     f"Debt:{debt_token_balance}, Discount:{discount_token_balance} -> {new_discount_percentage}"
    # )

    return new_discount_percentage


def _process_gho_debt_burn(
    w3: Web3,
    gho_asset: AaveGhoTokenTable,
    event_data: DebtBurnEvent,
    user: AaveV3UsersTable,
    scaled_token_revision: int,
    debt_position: AaveV3DebtPositionsTable,
    state_block: int,
) -> tuple[BalanceDelta, UserOperation]:
    """
    Determine the balance delta and user operation that triggered a GHO vToken Burn event.
    """

    ray_math_module, percentage_math = _get_math_libraries(scaled_token_revision)

    if scaled_token_revision == 1:
        # uint256 amountToBurn = amount - balanceIncrease;
        requested_amount = event_data.value + event_data.balance_increase

        # uint256 amountScaled = amount.rayDiv(index);
        amount_scaled = ray_math_module.ray_div(
            a=requested_amount,
            b=event_data.index,
        )

        # uint256 previousScaledBalance = super.balanceOf(user);
        previous_scaled_balance = debt_position.balance

        # uint256 discountPercent = _ghoUserState[user].discountPercent;
        discount_percent = user.gho_discount

        # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(
        #     user,
        #     previousScaledBalance,
        #     discountPercent,
        #     index
        # );
        balance_increase, discount_scaled = _accrue_debt_on_action(
            debt_position=debt_position,
            percentage_math=percentage_math,
            wad_ray_math=ray_math_module,
            previous_scaled_balance=previous_scaled_balance,
            discount_percent=discount_percent,
            index=event_data.index,
            token_revision=scaled_token_revision,
        )
        # TODO: Re-enable this assertion after debugging is complete
        assert balance_increase == event_data.balance_increase, (
            f"{balance_increase=} != {event_data.balance_increase=}, {last_event_processed=}"
        )

        # _burn(user, (amountScaled + discountScaled).toUint128());
        balance_delta = -(amount_scaled + discount_scaled)

        # Update the discount percentage for the new balance
        # _refreshDiscountPercent(
        #     user,
        #     super.balanceOf(user).rayMul(index), <--- new vToken balance (includes delta)
        #     _discountToken.balanceOf(user), <--- fetched from contract
        #     discountPercent, <--- previous discount percent, only used by contract to avoid duplicate events
        # )
        (discount_token_balance,) = raw_call(
            w3=w3,
            address=gho_asset.v_gho_discount_token,
            calldata=encode_function_calldata(
                function_prototype="balanceOf(address)",
                function_arguments=[user.address],
            ),
            return_types=["uint256"],
            block_identifier=state_block,
        )
        user.gho_discount = _get_discount_rate(
            w3=w3,
            discount_rate_strategy=gho_asset.v_gho_discount_rate_strategy,
            debt_token_balance=ray_math_module.ray_mul(
                a=debt_position.balance + balance_delta,
                b=event_data.index,
            ),
            discount_token_balance=discount_token_balance,
            state_block=state_block,
        )

    elif scaled_token_revision == 2:

        def _get_discounted_balance() -> int:
            """
            Get the discounted balance for the user.

            Ref:
                Mainnet GhoVariableDebtToken version 2
                Address 0x7aa606b1B341fFEeAfAdbbE4A2992EFB35972775
            """

            # TODO: extract to standalone function

            scaled_balance = previous_scaled_balance
            if scaled_balance == 0:
                return 0

            # index = 1002061322521461323686960054
            # previousIndex = 1000297527129060066010642736

            # index = POOL.getReserveNormalizedVariableDebt(_underlyingAsset)
            index = event_data.index

            # previousIndex = _userState[user].additionalData
            previous_index = debt_position.last_index

            balance = ray_math_module.ray_mul(
                a=scaled_balance,
                b=index,
            )
            if VERBOSE_ALL or user.address in VERBOSE_USERS:
                logger.info(f"{index=}")
                logger.info(f"{previous_index=}")

            if index == previous_index:
                return balance

            discount_percent_ = user.gho_discount
            if discount_percent_ != 0:
                balance_increase = balance - ray_math_module.ray_mul(
                    a=scaled_balance,
                    b=previous_index,
                )
                balance -= ray_math_module.ray_mul(
                    a=balance_increase,
                    b=discount_percent_,
                )

            if VERBOSE_ALL or user.address in VERBOSE_USERS:
                logger.info(f"{balance_increase=}")
                logger.info(f"{discount_percent_=}")

            return balance

        # uint256 amountToBurn = amount - balanceIncrease;
        requested_amount = event_data.value + event_data.balance_increase

        # uint256 amountScaled = amount.rayDiv(index);
        amount_scaled = ray_math_module.ray_div(
            a=requested_amount,
            b=event_data.index,
        )

        # uint256 previousScaledBalance = super.balanceOf(user);
        previous_scaled_balance = debt_position.balance

        # uint256 balanceBeforeBurn = balanceOf(user);
        balance_before_burn = _get_discounted_balance()

        # uint256 discountPercent = _ghoUserState[user].discountPercent;
        discount_percent = user.gho_discount

        # (uint256 balanceIncrease, uint256 discountScaled) = _accrueDebtOnAction(
        #     user,
        #     previousScaledBalance,
        #     discountPercent,
        #     index
        # );
        balance_increase, discount_scaled = _accrue_debt_on_action(
            debt_position=debt_position,
            percentage_math=percentage_math,
            wad_ray_math=ray_math_module,
            previous_scaled_balance=previous_scaled_balance,
            discount_percent=discount_percent,
            index=event_data.index,
            token_revision=scaled_token_revision,
        )
        assert balance_increase == event_data.balance_increase, (
            f"{balance_increase=} != {event_data.balance_increase=}"
        )

        if VERBOSE_ALL or user.address in VERBOSE_USERS:
            logger.info(f"{requested_amount=}")
            logger.info(f"{balance_before_burn=}")

        if requested_amount == balance_before_burn:
            # _burn(user, previousScaledBalance.toUint128());
            balance_delta = -previous_scaled_balance
        else:
            # _burn(user, (amountScaled + discountScaled).toUint128());
            balance_delta = -(amount_scaled + discount_scaled)

        # Update the discount percentage for the new balance
        # _refreshDiscountPercent(
        #     user,
        #     super.balanceOf(user).rayMul(index), <--- new vToken balance (includes delta)
        #     _discountToken.balanceOf(user), <--- fetched from contract
        #     discountPercent, <--- previous discount percent, only used by contract to avoid duplicate events
        # )
        (discount_token_balance,) = raw_call(
            w3=w3,
            address=gho_asset.v_gho_discount_token,
            calldata=encode_function_calldata(
                function_prototype="balanceOf(address)",
                function_arguments=[user.address],
            ),
            return_types=["uint256"],
            block_identifier=state_block,
        )
        user.gho_discount = _get_discount_rate(
            w3=w3,
            discount_rate_strategy=gho_asset.v_gho_discount_rate_strategy,
            debt_token_balance=ray_math_module.ray_mul(
                a=debt_position.balance + balance_delta,
                b=event_data.index,
            ),
            discount_token_balance=discount_token_balance,
            state_block=state_block,
        )

    else:
        raise ValueError(f"Unknown token revision: {scaled_token_revision}")

    if VERBOSE_ALL or user.address in VERBOSE_USERS:
        logger.info(f"{debt_position.balance=}")
        logger.info(f"{debt_position.balance + balance_delta=}")
        logger.info(f"{last_event_processed=}")
        logger.info(f"{user.address=}")
        logger.info(f"{user.gho_discount=}")
        logger.info(f"{balance_increase=}")
        logger.info(f"{discount_scaled=}")
        logger.info(f"{balance_delta=}")
        logger.info(f"{gho_asset.v_gho_discount_token=}")
        logger.info(f"{gho_asset.v_gho_discount_rate_strategy=}")
        logger.info(f"{discount_token_balance=}")
        logger.info(f"{state_block=}")

    assert requested_amount >= 0
    assert debt_position.balance + balance_delta >= 0, (
        f"{debt_position.balance} + {balance_delta} < 0!"
    )

    user_operation = "GHO REPAY"
    return balance_delta, user_operation

    # TODO: apply balance delta directly, log within processing functions instead of higher level


def _process_gho_debt_mint(
    w3: Web3,
    gho_asset: AaveGhoTokenTable,
    event_data: DebtMintEvent,
    user: AaveV3UsersTable,
    scaled_token_revision: int,
    debt_position: AaveV3DebtPositionsTable,
    state_block: int,
) -> tuple[BalanceDelta, UserOperation]:
    """
    Determine the balance delta and user operation that triggered a GHO vToken Mint event.
    """

    # TODO: determine if looking up the balance on previous block negatively affects anything - see TX 0x9fe48a0a6454cc7a83b1ac4d3fc412f40792e2359709db4c1959170052a1d5a5 which involves a combined stake/mint operation

    (discount_token_balance,) = raw_call(
        w3=w3,
        address=gho_asset.v_gho_discount_token,
        calldata=encode_function_calldata(
            function_prototype="balanceOf(address)",
            function_arguments=[user.address],
        ),
        return_types=["uint256"],
        block_identifier=state_block,
    )

    ray_math_module, percentage_math = _get_math_libraries(scaled_token_revision)

    previous_scaled_balance = debt_position.balance

    balance_increase, discount_scaled = _accrue_debt_on_action(
        debt_position=debt_position,
        percentage_math=percentage_math,
        wad_ray_math=ray_math_module,
        previous_scaled_balance=previous_scaled_balance,
        discount_percent=user.gho_discount,
        index=event_data.index,
        token_revision=scaled_token_revision,
    )
    # TODO: Re-enable this assertion after debugging is complete
    assert balance_increase == event_data.balance_increase, (
        f"{balance_increase=} != {event_data.balance_increase=}"
    )

    if event_data.value > event_data.balance_increase:
        # emitted in _mintScaled
        # uint256 amountToMint = amount + balanceIncrease;
        requested_amount = event_data.value - event_data.balance_increase
        user_operation = "GHO BORROW"
    else:
        # emitted in _burnScaled:
        # uint256 amountToMint = balanceIncrease - amount;
        requested_amount = event_data.balance_increase - event_data.value
        user_operation = "GHO REPAY"

    amount_scaled = ray_math_module.ray_div(
        a=requested_amount,
        b=event_data.index,
    )

    if amount_scaled > discount_scaled:
        balance_delta = amount_scaled - discount_scaled
    else:
        balance_delta = -(discount_scaled - amount_scaled)

    # Update the discount percentage for the new balance
    user.gho_discount = _get_discount_rate(
        w3=w3,
        discount_rate_strategy=gho_asset.v_gho_discount_rate_strategy,
        debt_token_balance=ray_math_module.ray_mul(
            a=debt_position.balance + balance_delta,
            b=event_data.index,
        ),
        discount_token_balance=discount_token_balance,
        state_block=state_block,
    )

    if VERBOSE_ALL or user.address in VERBOSE_USERS:
        logger.info(f"{user.address=}")
        logger.info(f"{user.gho_discount=}")
        logger.info(f"{balance_increase=}")
        logger.info(f"{discount_scaled=}")
        logger.info(f"{balance_delta=}")
        logger.info(f"{gho_asset.v_gho_discount_token=}")
        logger.info(f"{gho_asset.v_gho_discount_rate_strategy=}")
        logger.info(f"{debt_position.balance=}")
        logger.info(f"{discount_token_balance=}")
        logger.info(f"{state_block=}")

    assert requested_amount >= 0
    assert debt_position.balance + balance_delta >= 0, (
        f"{debt_position.balance} + {balance_delta} < 0!"
    )

    return balance_delta, user_operation


def _get_scaled_token_asset_by_address(
    session: Session,
    market: AaveV3MarketTable,
    token_address: ChecksumAddress,
) -> tuple[AaveV3AssetsTable | None, AaveV3AssetsTable | None]:
    """
    Get collateralt and debt assets by token address.
    """
    collateral_asset = session.scalar(
        select(AaveV3AssetsTable)
        .join(
            Erc20TokenTable,
            AaveV3AssetsTable.a_token_id == Erc20TokenTable.id,
        )
        .where(
            Erc20TokenTable.chain == market.chain_id,
            Erc20TokenTable.address == token_address,
        )
    )

    debt_asset = session.scalar(
        select(AaveV3AssetsTable)
        .join(
            Erc20TokenTable,
            AaveV3AssetsTable.v_token_id == Erc20TokenTable.id,
        )
        .where(
            Erc20TokenTable.chain == market.chain_id,
            Erc20TokenTable.address == token_address,
        )
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
    w3: Web3,
    session: Session,
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

    contract = session.scalar(
        select(AaveV3ContractsTable).where(
            AaveV3ContractsTable.name == contract_name,
            AaveV3ContractsTable.market_id == market.id,
        )
    )
    assert contract is not None, f"{contract_name} not found for market {market.id}"
    contract.revision = revision


def _process_scaled_token_mint_event(
    w3: Web3,
    event: LogReceipt,
    market: AaveV3MarketTable,
    session: Session,
    users_to_check: dict[ChecksumAddress, int],
    gho_users_to_check: dict[ChecksumAddress, int],
) -> None:
    """
    Process a scaled token Mint event as a collateral deposit or debt borrow.
    """

    # EVENT DEFINITION
    # event Mint(
    #     address indexed caller,
    #     address indexed onBehalfOf,
    #     uint256 value,
    #     uint256 balanceIncrease,
    #     uint256 index
    # );

    # TODO: Remove this debug variable after testing is complete
    global last_event_processed
    last_event_processed = event

    on_behalf_of_address = _decode_address(event["topics"][2])
    users_to_check[on_behalf_of_address] = event["blockNumber"]
    user = _get_or_create_user(session=session, market=market, user_address=on_behalf_of_address)

    event_amount, balance_increase, index = _decode_uint_values(event=event, num_values=3)

    # if event_amount == balance_increase:
    #     return

    token_address = get_checksum_address(event["address"])
    collateral_asset, debt_asset = _get_scaled_token_asset_by_address(
        session=session, market=market, token_address=token_address
    )

    if collateral_asset is not None:
        collateral_position = _get_or_create_collateral_position(
            session=session, user_id=user.id, asset_id=collateral_asset.id
        )

        balance_delta, user_operation = _process_scaled_token_operation(
            event=CollateralMintEvent(
                value=event_amount,
                balance_increase=balance_increase,
                index=index,
            ),
            scaled_token_revision=collateral_asset.a_token_revision,
        )

        user_starting_amount = collateral_position.balance

        collateral_position.balance += balance_delta
        collateral_position.last_index = index

        if VERBOSE_ALL or on_behalf_of_address in VERBOSE_USERS:
            _log_token_operation(
                user_operation=user_operation,
                user_address=on_behalf_of_address,
                token_type="aToken",  # noqa: S106
                token_address=token_address,
                index=index,
                balance_info=f"{user_starting_amount} -> {collateral_position.balance}",
                tx_hash=event["transactionHash"],
                block_info=f"{event['blockNumber']}.{event['logIndex']}",
                balance_delta=balance_delta,
            )

        assert collateral_position.balance >= 0

    elif debt_asset is not None:
        debt_position = _get_or_create_debt_position(
            session=session, user_id=user.id, asset_id=debt_asset.id
        )

        if token_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS:
            gho_users_to_check[on_behalf_of_address] = event["blockNumber"]
            gho_asset = session.scalar(
                select(AaveGhoTokenTable)
                .join(Erc20TokenTable)
                .where(Erc20TokenTable.chain == market.chain_id)
            )
            assert gho_asset is not None

            balance_delta, user_operation = _process_gho_debt_mint(
                w3=w3,
                gho_asset=gho_asset,
                event_data=DebtMintEvent(
                    value=event_amount,
                    balance_increase=balance_increase,
                    index=index,
                ),
                user=user,
                scaled_token_revision=debt_asset.v_token_revision,
                debt_position=debt_position,
                state_block=event["blockNumber"],
            )

        else:
            balance_delta, user_operation = _process_scaled_token_operation(
                event=DebtMintEvent(
                    value=event_amount,
                    balance_increase=balance_increase,
                    index=index,
                ),
                scaled_token_revision=debt_asset.v_token_revision,
            )

        user_starting_amount = debt_position.balance

        debt_position.balance += balance_delta
        debt_position.last_index = index

        if VERBOSE_ALL or on_behalf_of_address in VERBOSE_USERS:
            _log_token_operation(
                user_operation=user_operation,
                user_address=on_behalf_of_address,
                token_type="vToken",  # noqa: S106
                token_address=token_address,
                index=index,
                balance_info=f"{user_starting_amount} -> {debt_position.balance}",
                tx_hash=event["transactionHash"],
                block_info=f"{event['blockNumber']}.{event['logIndex']}",
                balance_delta=balance_delta,
            )

        assert debt_position.balance >= 0

    else:
        msg = (
            f"Unknown token type for address {get_checksum_address(event['address'])}. "
            "Expected aToken or vToken."
        )
        raise ValueError(msg)


def _process_scaled_token_burn_event(
    w3: Web3,
    event: LogReceipt,
    market: AaveV3MarketTable,
    session: Session,
    users_to_check: dict[ChecksumAddress, int],
    gho_users_to_check: dict[ChecksumAddress, int],
) -> None:
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

    # TODO: Remove this debug variable after testing is complete
    global last_event_processed
    last_event_processed = event

    from_address = _decode_address(event["topics"][1])
    users_to_check[from_address] = event["blockNumber"]

    event_amount, balance_increase, index = _decode_uint_values(event=event, num_values=3)

    user = session.scalar(
        select(AaveV3UsersTable).where(
            AaveV3UsersTable.address == from_address,
            AaveV3UsersTable.market_id == market.id,
        )
    )
    assert user is not None

    token_address = get_checksum_address(event["address"])
    collateral_asset, debt_asset = _get_scaled_token_asset_by_address(
        session=session, market=market, token_address=token_address
    )

    if collateral_asset is not None:
        collateral_position = session.scalar(
            select(AaveV3CollateralPositionsTable).where(
                AaveV3CollateralPositionsTable.user_id == user.id,
                AaveV3CollateralPositionsTable.asset_id == collateral_asset.id,
            )
        )
        assert collateral_position is not None

        balance_delta, user_operation = _process_scaled_token_operation(
            event=CollateralBurnEvent(
                value=event_amount,
                balance_increase=balance_increase,
                index=index,
            ),
            scaled_token_revision=collateral_asset.a_token_revision,
        )

        user_starting_amount = collateral_position.balance

        collateral_position.balance += balance_delta
        collateral_position.last_index = index

        if VERBOSE_ALL or from_address in VERBOSE_USERS:
            _log_token_operation(
                user_operation=user_operation,
                user_address=from_address,
                token_type="aToken",  # noqa: S106
                token_address=token_address,
                index=index,
                balance_info=f"{user_starting_amount} -> {collateral_position.balance}",
                tx_hash=event["transactionHash"],
                block_info=f"{event['blockNumber']}.{event['logIndex']}",
            )

        assert collateral_position.balance >= 0

    elif debt_asset is not None:
        debt_position = session.scalar(
            select(AaveV3DebtPositionsTable).where(
                AaveV3DebtPositionsTable.user_id == user.id,
                AaveV3DebtPositionsTable.asset_id == debt_asset.id,
            )
        )
        assert debt_position is not None

        if token_address == GHO_VARIABLE_DEBT_TOKEN_ADDRESS:
            gho_users_to_check[from_address] = event["blockNumber"]
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

            balance_delta, user_operation = _process_gho_debt_burn(
                w3=w3,
                gho_asset=gho_asset,
                event_data=DebtBurnEvent(
                    value=event_amount,
                    balance_increase=balance_increase,
                    index=index,
                ),
                user=user,
                scaled_token_revision=debt_asset.v_token_revision,
                debt_position=debt_position,
                state_block=event["blockNumber"],
            )
        else:
            balance_delta, user_operation = _process_scaled_token_operation(
                event=DebtBurnEvent(
                    value=event_amount,
                    balance_increase=balance_increase,
                    index=index,
                ),
                scaled_token_revision=debt_asset.v_token_revision,
            )

        user_starting_amount = debt_position.balance

        debt_position.balance += balance_delta
        debt_position.last_index = index

        if VERBOSE_ALL or from_address in VERBOSE_USERS:
            _log_token_operation(
                user_operation=user_operation,
                user_address=from_address,
                token_type="vToken",  # noqa: S106
                token_address=token_address,
                index=index,
                balance_info=f"{user_starting_amount} -> {debt_position.balance}",
                tx_hash=event["transactionHash"],
                block_info=f"{event['blockNumber']}.{event['logIndex']}",
            )

        assert debt_position.balance >= 0

    else:
        msg = f"Unknown token type for address {token_address}. Expected aToken or vToken."
        raise ValueError(msg)


def _process_scaled_token_balance_transfer_event(
    event: LogReceipt,
    market: AaveV3MarketTable,
    session: Session,
    users_to_check: dict[ChecksumAddress, int],
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

    from_address = _decode_address(event["topics"][1])
    to_address = _decode_address(event["topics"][2])

    users_to_check[from_address] = event["blockNumber"]
    users_to_check[to_address] = event["blockNumber"]

    event_amount, _ = _decode_uint_values(event=event, num_values=2)

    # Special case for zero-amount transfers such as
    # TX 0x5fd783a51ae534a7d720ca8f1ce77657df5cc2564d1d81476be16078578d4142
    if event_amount == 0:
        return

    aave_asset = session.scalar(
        select(AaveV3AssetsTable)
        .join(
            Erc20TokenTable,
            AaveV3AssetsTable.a_token_id == Erc20TokenTable.id,
        )
        .where(
            Erc20TokenTable.chain == market.chain_id,
            Erc20TokenTable.address == get_checksum_address(event["address"]),
        )
    )
    assert aave_asset is not None

    from_user = _get_or_create_user(session=session, market=market, user_address=from_address)
    assert from_user is not None

    from_user_position = session.scalar(
        select(AaveV3CollateralPositionsTable).where(
            AaveV3CollateralPositionsTable.user_id == from_user.id,
            AaveV3CollateralPositionsTable.asset_id == aave_asset.id,
        )
    )
    assert from_user_position, f"{from_address}: TX {event['transactionHash'].to_0x_hex()}"

    from_user_starting_amount = from_user_position.balance
    from_user_position.balance -= event_amount

    to_user = _get_or_create_user(session=session, market=market, user_address=to_address)
    assert to_user is not None

    if (
        to_user_position := session.scalar(
            select(AaveV3CollateralPositionsTable).where(
                AaveV3CollateralPositionsTable.user_id == to_user.id,
                AaveV3CollateralPositionsTable.asset_id == aave_asset.id,
            )
        )
    ) is None:
        to_user_position = _get_or_create_collateral_position(
            session=session, user_id=to_user.id, asset_id=aave_asset.id
        )

    to_user_starting_amount = to_user_position.balance
    to_user_position.balance += event_amount

    if VERBOSE_ALL or any(addr in VERBOSE_USERS for addr in (from_address, to_address)):
        _log_balance_transfer(
            token_address=get_checksum_address(event["address"]),
            from_address=from_address,
            from_balance_info=f"{from_user_starting_amount} -> {from_user_position.balance}",
            to_address=to_address,
            to_balance_info=f"{to_user_starting_amount} -> {to_user_position.balance}",
            tx_hash=event["transactionHash"],
            block_info=f"{event['blockNumber']}.{event['logIndex']}",
        )

    assert from_user_position.balance >= 0
    assert to_user_position.balance >= 0


def update_aave_market(
    w3: Web3,
    start_block: int,
    end_block: int,
    market: AaveV3MarketTable,
    session: Session,
) -> None:
    """
    Update the Aave V3 market.
    """

    users_to_check: dict[ChecksumAddress, int] = {}
    gho_users_to_check: dict[ChecksumAddress, int] = {}
    last_event_block = 0

    # Get the contract addresses for this market
    pool_address_provider = EthereumMainnetAaveV3.pool_address_provider

    for proxy_creation_event in fetch_logs_retrying(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=[pool_address_provider],
        topic_signature=[
            [HexBytes("0x4a465a9bd819d9662563c1e11ae958f8109e437e7f4bf1c6ef0b9a7b3f35d478")],
        ],
    ):
        (proxy_id,) = eth_abi.abi.decode(types=["bytes32"], data=proxy_creation_event["topics"][1])

        if proxy_id == eth_abi.abi.encode(["bytes32"], [b"POOL"]):
            (pool_proxy_address,) = eth_abi.abi.decode(
                types=["address"], data=proxy_creation_event["topics"][2]
            )
            pool_proxy_address = get_checksum_address(pool_proxy_address)

            (implementation_address,) = eth_abi.abi.decode(
                types=["address"], data=proxy_creation_event["topics"][3]
            )
            implementation_address = get_checksum_address(implementation_address)

            assert (
                session.scalar(
                    select(AaveV3ContractsTable).where(
                        AaveV3ContractsTable.address == pool_proxy_address
                    )
                )
                is None
            )

            # Get the revision from the specific implementation
            (pool_revision,) = raw_call(
                w3=w3,
                address=implementation_address,
                calldata=encode_function_calldata(
                    function_prototype="POOL_REVISION()",
                    function_arguments=None,
                ),
                return_types=["uint256"],
            )

            session.add(
                AaveV3ContractsTable(
                    market_id=market.id,
                    name="POOL",
                    address=pool_proxy_address,
                    revision=pool_revision,
                )
            )

        elif proxy_id == eth_abi.abi.encode(["bytes32"], [b"POOL_CONFIGURATOR"]):
            (pool_configurator_proxy_address,) = eth_abi.abi.decode(
                types=["address"], data=proxy_creation_event["topics"][2]
            )
            pool_configurator_proxy_address = get_checksum_address(pool_configurator_proxy_address)

            (implementation_address,) = eth_abi.abi.decode(
                types=["address"], data=proxy_creation_event["topics"][3]
            )
            implementation_address = get_checksum_address(implementation_address)

            assert (
                session.scalar(
                    select(AaveV3ContractsTable).where(
                        AaveV3ContractsTable.address == pool_configurator_proxy_address
                    )
                )
                is None
            )

            (configurator_revision,) = raw_call(
                w3=w3,
                address=implementation_address,
                calldata=encode_function_calldata(
                    function_prototype="CONFIGURATOR_REVISION()",
                    function_arguments=None,
                ),
                return_types=["uint256"],
            )

            session.add(
                AaveV3ContractsTable(
                    market_id=market.id,
                    name="POOL_CONFIGURATOR",
                    address=pool_configurator_proxy_address,
                    revision=configurator_revision,
                )
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
                    session=session,
                    market=market,
                    contract_name="POOL_CONFIGURATOR",
                    new_address=_decode_address(contract_update_event["topics"][2]),
                    revision_function_prototype="CONFIGURATOR_REVISION",
                )

            case AaveV3Event.POOL_UPDATED.value:
                pool = session.scalar(
                    select(AaveV3ContractsTable).where(
                        AaveV3ContractsTable.name == "POOL",
                        AaveV3ContractsTable.market_id == market.id,
                    )
                )
                assert pool is not None

                new_address = _decode_address(contract_update_event["topics"][2])
                _update_contract_revision(
                    w3=w3,
                    session=session,
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

    pool = session.scalar(
        select(AaveV3ContractsTable).where(
            AaveV3ContractsTable.name == "POOL",
            AaveV3ContractsTable.market_id == market.id,
        )
    )
    assert pool is not None

    pool_configurator = session.scalar(
        select(AaveV3ContractsTable).where(
            AaveV3ContractsTable.name == "POOL_CONFIGURATOR",
            AaveV3ContractsTable.market_id == market.id,
        )
    )
    assert pool_configurator is not None

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
                    # Get DiscountTokenUpdated events to set the discount token address for the
                    # GHO vToken
                    AaveV3Event.DISCOUNT_TOKEN_UPDATED.value,
                ],
            ],
        )
    )

    for event in sorted(all_events, key=operator.itemgetter("blockNumber", "logIndex")):
        if users_to_check and event["blockNumber"] > last_event_block:
            _verify_scaled_token_positions(
                w3=w3,
                market=market,
                session=session,
                users_to_check=users_to_check,
                position_table=AaveV3CollateralPositionsTable,
            )
            _verify_scaled_token_positions(
                w3=w3,
                market=market,
                session=session,
                users_to_check=users_to_check,
                position_table=AaveV3DebtPositionsTable,
            )
            users_to_check.clear()
        if gho_users_to_check and event["blockNumber"] > last_event_block:
            _verify_gho_discount_amounts(
                w3=w3,
                market=market,
                session=session,
                users_to_check=gho_users_to_check,
            )

        match event["topics"][0]:
            case AaveV3Event.USER_E_MODE_SET.value:
                _process_user_e_mode_set_event(
                    event=event,
                    market=market,
                    session=session,
                )
            case AaveV3Event.RESERVE_DATA_UPDATED.value:
                _process_reserve_data_update_event(
                    event=event,
                    market=market,
                    session=session,
                )
            case AaveV3Event.SCALED_TOKEN_BURN.value:
                _process_scaled_token_burn_event(
                    w3=w3,
                    event=event,
                    market=market,
                    session=session,
                    users_to_check=users_to_check,
                    gho_users_to_check=gho_users_to_check,
                )
            case AaveV3Event.SCALED_TOKEN_MINT.value:
                _process_scaled_token_mint_event(
                    w3=w3,
                    event=event,
                    market=market,
                    session=session,
                    users_to_check=users_to_check,
                    gho_users_to_check=gho_users_to_check,
                )
            case AaveV3Event.UPGRADED.value:
                _process_scaled_token_upgrade_event(
                    w3=w3,
                    event=event,
                    market=market,
                    session=session,
                )
            case AaveV3Event.SCALED_TOKEN_BALANCE_TRANSFER.value:
                _process_scaled_token_balance_transfer_event(
                    event=event,
                    market=market,
                    session=session,
                    users_to_check=users_to_check,
                )
            case AaveV3Event.DISCOUNT_RATE_STRATEGY_UPDATED.value:
                _process_discount_rate_strategy_updated_event(
                    event=event,
                    market=market,
                    session=session,
                )
            case AaveV3Event.DISCOUNT_TOKEN_UPDATED.value:
                _process_discount_token_updated_event(
                    event=event,
                    market=market,
                    session=session,
                )
            case _:
                msg = (
                    f"Could not identify event with topic {event['topics'][0].to_0x_hex()}: {event}"
                )
                raise ValueError(msg)

        last_event_block = event["blockNumber"]

    _verify_scaled_token_positions(
        w3=w3,
        market=market,
        session=session,
        users_to_check=users_to_check,
        position_table=AaveV3CollateralPositionsTable,
    )
    _verify_scaled_token_positions(
        w3=w3,
        market=market,
        session=session,
        users_to_check=users_to_check,
        position_table=AaveV3DebtPositionsTable,
    )
    users_to_check.clear()

    _verify_gho_discount_amounts(
        w3=w3,
        market=market,
        session=session,
        users_to_check=gho_users_to_check,
    )
    gho_users_to_check.clear()

    # # Zero balance rows are not useful
    # for table in (AaveV3CollateralPositionsTable, AaveV3DebtPositionsTable):
    #     session.execute(delete(table).where(table.balance == 0))
