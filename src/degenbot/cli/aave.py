# TODO: add progress bars
# TODO: add scraper for collateral usage enabled events

import operator
from dataclasses import dataclass
from enum import Enum
from typing import TYPE_CHECKING, Protocol, cast

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
from degenbot.aave.deployments import EthereumMainnetAaveV3
from degenbot.checksum_cache import get_checksum_address
from degenbot.cli import cli
from degenbot.cli.utils import get_web3_from_config
from degenbot.constants import ZERO_ADDRESS
from degenbot.database import db_session
from degenbot.database.models.aave import (
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


# Selection of user addresses to show verbose logging during development
# Remove before using in production
VERBOSE_ALL = True
VERBOSE_USERS: set[ChecksumAddress] = set()
VERBOSE_USERS.update({
    get_checksum_address("0xaB1A2802F0Ba6F958009DE8739250e04BAE67E3b"),
})


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

    assert len(event["topics"]) == 3  # noqa: PLR2004

    (asset_address,) = eth_abi.abi.decode(types=["address"], data=event["topics"][1])
    asset_address = get_checksum_address(asset_address)

    (a_token_address,) = eth_abi.abi.decode(types=["address"], data=event["topics"][2])
    a_token_address = get_checksum_address(a_token_address)

    # Note: stableDebtToken is deprecated in Aave V3, so is ignored
    (_, v_token_address, _) = eth_abi.abi.decode(
        types=["address", "address", "address"], data=event["data"]
    )
    v_token_address = get_checksum_address(v_token_address)

    # Ensure an ERC-20 row exists for the underlying asset
    if (
        erc20_token_in_db := session.scalar(
            select(Erc20TokenTable).where(
                Erc20TokenTable.chain == market.chain_id,
                Erc20TokenTable.address == asset_address,
            )
        )
    ) is None:
        erc20_token_in_db = Erc20TokenTable(
            chain=market.chain_id,
            address=asset_address,
        )
        session.add(erc20_token_in_db)
        session.flush()

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

    if (
        v_token := session.scalar(
            select(Erc20TokenTable).where(
                Erc20TokenTable.chain == market.chain_id,
                Erc20TokenTable.address == v_token_address,
            )
        )
    ) is None:
        v_token = Erc20TokenTable(
            chain=market.chain_id,
            address=v_token_address,
        )
        session.add(v_token)
        session.flush()

    # Per EIP-1967, the implementation addresses is found by retrieving the storage slot
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


def get_aave_v3_contract_update_events(
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


def get_aave_v3_reserve_initialized_events(
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
                    update_aave_v3_market(
                        w3=w3,
                        start_block=working_start_block,
                        end_block=working_end_block,
                        market=market,
                        session=session,
                    )

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

    assert len(event["topics"]) == 2  # noqa: PLR2004

    (user_address,) = eth_abi.abi.decode(types=["address"], data=event["topics"][1])
    user_address = get_checksum_address(user_address)

    (e_mode,) = eth_abi.abi.decode(types=["uint8"], data=event["data"])

    if (
        user := session.scalar(
            select(AaveV3UsersTable).where(
                AaveV3UsersTable.market_id == market.id,
                AaveV3UsersTable.address == user_address,
            )
        )
    ) is None:
        user = AaveV3UsersTable(
            market_id=market.id,
            address=user_address,
            e_mode=0,
        )
        session.add(user)

    user.e_mode = e_mode


def _process_reserve_data_update_event(
    event: LogReceipt,
    market: AaveV3MarketTable,
    session: Session,
) -> None:
    """
    Process a ReserveDataUpdated event to update asset rates and indices.
    """
    assert len(event["topics"]) == 2  # noqa: PLR2004

    # EVENT DEFINITION
    # event ReserveDataUpdated(
    #     address indexed reserve,
    #     uint256 liquidityRate,
    #     uint256 stableBorrowRate,
    #     uint256 variableBorrowRate,
    #     uint256 liquidityIndex,
    #     uint256 variableBorrowIndex
    # );

    (reserve_asset_address,) = eth_abi.abi.decode(types=["address"], data=event["topics"][1])
    reserve_asset_address = get_checksum_address(reserve_asset_address)

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
    # EVENT DEFINITION
    # event Upgraded(
    #     address indexed implementation
    # );

    assert len(event["topics"]) == 2  # noqa: PLR2004

    (new_implementation_address,) = eth_abi.abi.decode(types=["address"], data=event["topics"][1])
    new_implementation_address = get_checksum_address(new_implementation_address)

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


class WadRayMathLibrary(Protocol):
    def ray_div(self, a: int, b: int) -> int: ...


def _get_wad_ray_math_library(token_revision: int) -> WadRayMathLibrary:
    if token_revision == 1:
        return aave_library_v3_1.wad_ray_math
    if token_revision == 2:  # noqa: PLR2004
        return aave_library_v3_2.wad_ray_math
    if token_revision == 3:  # noqa: PLR2004
        return aave_library_v3_3.wad_ray_math
    msg = f"Unsupported revision: {token_revision}"
    raise ValueError(msg)


type BalanceDelta = int
type UserOperation = str


@dataclass(frozen=True, slots=True)
class CollateralMintEvent:
    value: int
    balance_increase: int
    liquidity_index: int

    def __post_init__(self) -> None:
        if self.value == self.balance_increase:
            msg = f"CollateralMintEvent cannot have equal value and balance_increase: {self.value}"
            raise ValueError(msg)


@dataclass(frozen=True, slots=True)
class CollateralBurnEvent:
    value: int
    balance_increase: int
    liquidity_index: int

    def __post_init__(self) -> None:
        if self.value == self.balance_increase:
            msg = f"CollateralBurnEvent cannot have equal value and balance_increase: {self.value}"
            raise ValueError(msg)


@dataclass(frozen=True, slots=True)
class DebtMintEvent:
    value: int
    balance_increase: int
    liquidity_index: int

    def __post_init__(self) -> None:
        if self.value == self.balance_increase:
            msg = f"DebtMintEvent cannot have equal value and balance_increase: {self.value}"
            raise ValueError(msg)


@dataclass(frozen=True, slots=True)
class DebtBurnEvent:
    value: int
    balance_increase: int
    liquidity_index: int

    def __post_init__(self) -> None:
        if self.value == self.balance_increase:
            msg = f"DebtBurnEvent cannot have equal value and balance_increase: {self.value}"
            raise ValueError(msg)


def _process_collateral_mint(
    event_data: CollateralMintEvent,
    scaled_token_revision: int,
) -> tuple[BalanceDelta, UserOperation]:
    ray_math_module = _get_wad_ray_math_library(scaled_token_revision)

    if event_data.balance_increase > event_data.value:
        requested_amount = event_data.balance_increase - event_data.value
        delta = -ray_math_module.ray_div(
            a=requested_amount,
            b=event_data.liquidity_index,
        )
        operation = "WITHDRAW"
    else:
        requested_amount = event_data.value - event_data.balance_increase
        delta = ray_math_module.ray_div(
            a=requested_amount,
            b=event_data.liquidity_index,
        )
        operation = "DEPOSIT"

    assert requested_amount > 0
    return delta, operation


def _process_collateral_burn(
    event_data: CollateralBurnEvent,
    scaled_token_revision: int,
) -> tuple[BalanceDelta, UserOperation]:
    ray_math = _get_wad_ray_math_library(scaled_token_revision)
    requested_amount = event_data.value + event_data.balance_increase
    balance_delta = -ray_math.ray_div(
        a=requested_amount,
        b=event_data.liquidity_index,
    )

    assert requested_amount > 0
    return balance_delta, "WITHDRAW"


def _process_debt_mint(
    event_data: DebtMintEvent,
    scaled_token_revision: int,
) -> tuple[BalanceDelta, UserOperation]:
    ray_math = _get_wad_ray_math_library(scaled_token_revision)
    requested_amount = abs(event_data.value - event_data.balance_increase)
    balance_delta = ray_math.ray_div(
        a=requested_amount,
        b=event_data.liquidity_index,
    )

    assert requested_amount > 0
    return balance_delta, "BORROW"


def _process_debt_burn(
    event_data: DebtBurnEvent,
    scaled_token_revision: int,
) -> tuple[BalanceDelta, UserOperation]:
    ray_math = _get_wad_ray_math_library(scaled_token_revision)
    requested_amount = event_data.value + event_data.balance_increase
    balance_delta = -ray_math.ray_div(
        a=requested_amount,
        b=event_data.liquidity_index,
    )

    assert requested_amount > 0
    return balance_delta, "REPAY"


def _process_scaled_token_mint_event(
    event: LogReceipt,
    market: AaveV3MarketTable,
    session: Session,
    users_to_check: dict[ChecksumAddress, int],
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

    assert len(event["topics"]) == 3  # noqa: PLR2004

    (caller_address,) = eth_abi.abi.decode(types=["address"], data=event["topics"][1])
    caller_address = get_checksum_address(caller_address)

    (on_behalf_of_address,) = eth_abi.abi.decode(types=["address"], data=event["topics"][2])
    on_behalf_of_address = get_checksum_address(on_behalf_of_address)

    users_to_check[caller_address] = event["blockNumber"]
    users_to_check[on_behalf_of_address] = event["blockNumber"]

    if (
        user := session.scalar(
            select(AaveV3UsersTable).where(
                AaveV3UsersTable.address == on_behalf_of_address,
                AaveV3UsersTable.market_id == market.id,
            )
        )
    ) is None:
        user = AaveV3UsersTable(
            market_id=market.id,
            address=on_behalf_of_address,
            e_mode=0,
        )
        session.add(user)
        session.flush()

    event_amount, balance_increase, liquidity_index = eth_abi.abi.decode(
        types=["uint256", "uint256", "uint256"], data=event["data"]
    )

    if event_amount == balance_increase:
        return

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
        if (
            collateral_position := session.scalar(
                select(AaveV3CollateralPositionsTable).where(
                    AaveV3CollateralPositionsTable.user_id == user.id,
                    AaveV3CollateralPositionsTable.asset_id == aave_collateral_asset.id,
                )
            )
        ) is None:
            collateral_position = AaveV3CollateralPositionsTable(
                user_id=user.id,
                asset_id=aave_collateral_asset.id,
                balance=0,
            )
            session.add(collateral_position)

        collateral_position_starting_amount = collateral_position.balance
        assert collateral_position_starting_amount >= 0

        balance_delta, user_operation = _process_collateral_mint(
            event_data=CollateralMintEvent(
                value=event_amount,
                balance_increase=balance_increase,
                liquidity_index=liquidity_index,
            ),
            scaled_token_revision=aave_collateral_asset.a_token_revision,
        )

        collateral_position.balance += balance_delta

        if VERBOSE_ALL or on_behalf_of_address in VERBOSE_USERS:
            logger.info(user_operation)
            logger.info(f"aToken  : {get_checksum_address(event['address'])}")
            logger.info(f"Index   : {liquidity_index} ")
            logger.info(
                f"Balance : {collateral_position_starting_amount} -> {collateral_position.balance}"
            )
            logger.info(f"TX      : {event['transactionHash'].to_0x_hex()}")
            logger.info(f"Block   : {event['blockNumber']}.{event['logIndex']}")
            logger.info("")

        assert collateral_position.balance >= 0, f"{on_behalf_of_address}"

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
        if (
            debt_position := session.scalar(
                select(AaveV3DebtPositionsTable).where(
                    AaveV3DebtPositionsTable.user_id == user.id,
                    AaveV3DebtPositionsTable.asset_id == aave_debt_asset.id,
                )
            )
        ) is None:
            debt_position = AaveV3DebtPositionsTable(
                user_id=user.id,
                asset_id=aave_debt_asset.id,
                balance=0,
            )
            session.add(debt_position)

        debt_position_starting_amount = debt_position.balance
        assert debt_position_starting_amount >= 0

        balance_delta, user_operation = _process_debt_mint(
            event_data=DebtMintEvent(
                value=event_amount,
                balance_increase=balance_increase,
                liquidity_index=liquidity_index,
            ),
            scaled_token_revision=aave_debt_asset.v_token_revision,
        )
        debt_position.balance += balance_delta

        if VERBOSE_ALL or on_behalf_of_address in VERBOSE_USERS:
            logger.info(user_operation)
            logger.info(f"vToken  : {get_checksum_address(event['address'])}")
            logger.info(f"Index   : {liquidity_index} ")
            logger.info(f"Balance : {debt_position_starting_amount} -> {debt_position.balance}")
            logger.info(f"TX      : {event['transactionHash'].to_0x_hex()}")
            logger.info(f"Block   : {event['blockNumber']}.{event['logIndex']}")
            logger.info("")

        assert debt_position.balance >= 0

    else:
        msg = (
            f"Unknown token type for address {get_checksum_address(event['address'])}. "
            "Expected aToken or vToken."
        )
        raise ValueError(msg)


def _process_scaled_token_burn_event(
    event: LogReceipt,
    market: AaveV3MarketTable,
    session: Session,
    users_to_check: dict[ChecksumAddress, int],
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

    assert len(event["topics"]) == 3  # noqa: PLR2004

    (from_address,) = eth_abi.abi.decode(types=["address"], data=event["topics"][1])
    from_address = get_checksum_address(from_address)

    users_to_check[from_address] = event["blockNumber"]

    amount_burned, balance_increase, liquidity_index = eth_abi.abi.decode(
        types=["uint256", "uint256", "uint256"], data=event["data"]
    )

    user = session.scalar(
        select(AaveV3UsersTable).where(
            AaveV3UsersTable.market_id == market.id,
            AaveV3UsersTable.address == from_address,
        )
    )
    assert user is not None

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
        collateral_position = session.scalar(
            select(AaveV3CollateralPositionsTable).where(
                AaveV3CollateralPositionsTable.user_id == user.id,
                AaveV3CollateralPositionsTable.asset_id == aave_collateral_asset.id,
            )
        )
        assert collateral_position is not None

        balance_delta, user_operation = _process_collateral_burn(
            event_data=CollateralBurnEvent(
                value=amount_burned,
                balance_increase=balance_increase,
                liquidity_index=liquidity_index,
            ),
            scaled_token_revision=aave_collateral_asset.a_token_revision,
        )
        user_starting_amount = collateral_position.balance
        collateral_position.balance += balance_delta

        if VERBOSE_ALL or from_address in VERBOSE_USERS:
            logger.info(user_operation)
            logger.info(f"aToken  : {get_checksum_address(event['address'])}")
            logger.info(f"Index   : {liquidity_index}")
            logger.info(f"Balance : {user_starting_amount} -> {collateral_position.balance}")
            logger.info(f"TX      : {event['transactionHash'].to_0x_hex()}")
            logger.info(f"Block   : {event['blockNumber']}.{event['logIndex']}")
            logger.info("")

        assert collateral_position.balance >= 0, f"{from_address}"
        if collateral_position.balance == 0:
            session.delete(collateral_position)

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
        debt_position = session.scalar(
            select(AaveV3DebtPositionsTable).where(
                AaveV3DebtPositionsTable.user_id == user.id,
                AaveV3DebtPositionsTable.asset_id == aave_debt_asset.id,
            )
        )
        assert debt_position is not None

        balance_delta, user_operation = _process_debt_burn(
            event_data=DebtBurnEvent(
                value=amount_burned,
                balance_increase=balance_increase,
                liquidity_index=liquidity_index,
            ),
            scaled_token_revision=aave_debt_asset.v_token_revision,
        )

        user_starting_amount = debt_position.balance
        debt_position.balance += balance_delta

        if VERBOSE_ALL or from_address in VERBOSE_USERS:
            logger.info(user_operation)
            logger.info(f"vToken  : {get_checksum_address(event['address'])}")
            logger.info(f"Index   : {liquidity_index}")
            logger.info(f"Balance : {user_starting_amount} -> {debt_position.balance}")
            logger.info(f"TX      : {event['transactionHash'].to_0x_hex()}")
            logger.info(f"Block   : {event['blockNumber']}.{event['logIndex']}")
            logger.info("")

        assert debt_position.balance >= 0, f"{from_address}"
        if debt_position.balance == 0:
            session.delete(debt_position)
    else:
        msg = (
            f"Unknown token type for address {get_checksum_address(event['address'])}. "
            "Expected aToken or vToken."
        )
        raise ValueError(msg)


def _process_a_token_balance_transfer_event(
    event: LogReceipt,
    market: AaveV3MarketTable,
    session: Session,
) -> None:
    """
    Process a scaled token balance transfer.
    """

    # EVENT DEFINITION
    # event BalanceTransfer(
    #     address indexed from,
    #     address indexed to,
    #     uint256 value,
    #     uint256 index
    # );

    assert len(event["topics"]) == 3  # noqa: PLR2004

    (from_address,) = eth_abi.abi.decode(types=["address"], data=event["topics"][1])
    from_address = get_checksum_address(from_address)

    (to_address,) = eth_abi.abi.decode(types=["address"], data=event["topics"][2])
    to_address = get_checksum_address(to_address)

    event_amount, _ = eth_abi.abi.decode(types=["uint256", "uint256"], data=event["data"])

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

    # BalanceTransfer events should always correspond to collateral, since a user's debt can only
    # be modified by the Pool
    assert (
        session.scalar(
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
        is None
    )

    from_user = session.scalar(
        select(AaveV3UsersTable).where(
            AaveV3UsersTable.market_id == market.id,
            AaveV3UsersTable.address == from_address,
        )
    )
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

    if (
        to_user := session.scalar(
            select(AaveV3UsersTable).where(
                AaveV3UsersTable.address == to_address,
                AaveV3UsersTable.market_id == market.id,
            )
        )
    ) is None:
        to_user = AaveV3UsersTable(
            market_id=market.id,
            address=to_address,
            e_mode=0,
        )
        session.add(to_user)
        session.flush()

    if (
        to_user_position := session.scalar(
            select(AaveV3CollateralPositionsTable).where(
                AaveV3CollateralPositionsTable.user_id == to_user.id,
                AaveV3CollateralPositionsTable.asset_id == aave_asset.id,
            )
        )
    ) is None:
        to_user_position = AaveV3CollateralPositionsTable(
            user_id=to_user.id,
            asset_id=aave_asset.id,
            balance=0,
        )
        session.add(to_user_position)

    to_user_starting_amount = to_user_position.balance
    to_user_position.balance += event_amount

    if VERBOSE_ALL or VERBOSE_USERS & {from_address, to_address}:
        logger.info("BALANCE TRANSFER")
        logger.info(f"aToken  : {get_checksum_address(event['address'])}")
        logger.info(f"User    : {from_address}")
        logger.info(f"Balance : {from_user_starting_amount} -> {from_user_position.balance}")
        logger.info(f"User    : {to_address}")
        logger.info(f"Balance : {to_user_starting_amount} -> {to_user_position.balance}")
        logger.info(f"TX      : {event['transactionHash'].to_0x_hex()}")
        logger.info(f"Block   : {event['blockNumber']}.{event['logIndex']}")
        logger.info("")

    assert from_user_position.balance >= 0
    assert to_user_position.balance >= 0

    if from_user_position.balance == 0:
        session.delete(from_user_position)


def update_aave_v3_market(
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

    contract_update_events = get_aave_v3_contract_update_events(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=pool_address_provider,
    )
    for contract_update_event in contract_update_events:
        match contract_update_event["topics"][0]:
            case AaveV3Event.POOL_CONFIGURATOR_UPDATED.value:
                (new_address,) = eth_abi.abi.decode(
                    types=["address"], data=contract_update_event["topics"][2]
                )
                new_address = get_checksum_address(new_address)
                (configurator_revision,) = raw_call(
                    w3=w3,
                    address=new_address,
                    calldata=encode_function_calldata(
                        function_prototype="CONFIGURATOR_REVISION()",
                        function_arguments=None,
                    ),
                    return_types=["uint256"],
                )

                pool_configurator = session.scalar(
                    select(AaveV3ContractsTable).where(
                        AaveV3ContractsTable.name == "POOL_CONFIGURATOR",
                        AaveV3ContractsTable.market_id == market.id,
                    )
                )
                assert pool_configurator is not None
                pool_configurator.revision = configurator_revision

            case AaveV3Event.POOL_UPDATED.value:
                pool = session.scalar(
                    select(AaveV3ContractsTable).where(
                        AaveV3ContractsTable.name == "POOL",
                        AaveV3ContractsTable.market_id == market.id,
                    )
                )
                assert pool is not None

                (new_address,) = eth_abi.abi.decode(
                    types=["address"], data=contract_update_event["topics"][2]
                )
                new_address = get_checksum_address(new_address)
                (pool_revision,) = raw_call(
                    w3=w3,
                    address=new_address,
                    calldata=encode_function_calldata(
                        function_prototype="POOL_REVISION()",
                        function_arguments=None,
                    ),
                    return_types=["uint256"],
                )
                pool.revision = pool_revision

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
    reserve_initialization_events = get_aave_v3_reserve_initialized_events(
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

    known_scaled_token_addresses: list[ChecksumAddress] = list(
        session.scalars(
            select(Erc20TokenTable.address)
            .join(
                AaveV3AssetsTable,
                AaveV3AssetsTable.a_token_id == Erc20TokenTable.id,
            )
            .where(Erc20TokenTable.chain == w3.eth.chain_id)
        ).all()
    ) + list(
        session.scalars(
            select(Erc20TokenTable.address)
            .join(
                AaveV3AssetsTable,
                AaveV3AssetsTable.v_token_id == Erc20TokenTable.id,
            )
            .where(Erc20TokenTable.chain == w3.eth.chain_id)
        ).all()
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

    last_check = 0

    def verify_updated_collateral_positions() -> None:
        for user_address, last_update_block in users_to_check.items():
            user = session.scalar(
                select(AaveV3UsersTable).where(
                    AaveV3UsersTable.address == user_address,
                    AaveV3UsersTable.market_id == market.id,
                )
            )
            if user is None:
                continue

            # Get collateral positions for the user
            for collateral_position in session.scalars(
                select(AaveV3CollateralPositionsTable).where(
                    AaveV3CollateralPositionsTable.user_id == user.id,
                )
            ):
                collateral_asset = session.scalar(
                    select(AaveV3AssetsTable).where(
                        AaveV3AssetsTable.market_id == market.id,
                        AaveV3AssetsTable.id == collateral_position.asset_id,
                    )
                )
                assert collateral_asset is not None
                scaled_token_address = session.scalar(
                    select(Erc20TokenTable.address).where(
                        Erc20TokenTable.chain == market.chain_id,
                        Erc20TokenTable.id == collateral_asset.a_token_id,
                    )
                )
                scaled_token_address = get_checksum_address(scaled_token_address)
                (actual_scaled_balance,) = raw_call(
                    w3=w3,
                    address=scaled_token_address,
                    calldata=encode_function_calldata(
                        function_prototype="scaledBalanceOf(address)",
                        function_arguments=[user_address],
                    ),
                    return_types=["uint256"],
                    block_identifier=last_update_block,
                )
                assert actual_scaled_balance == collateral_position.balance, (
                    f"User {user_address}: balance ({collateral_position.balance}) "
                    f"does not match contract ({actual_scaled_balance}) "
                    f"@ {
                        session.scalar(
                            select(Erc20TokenTable.address).where(
                                Erc20TokenTable.chain == market.chain_id,
                                Erc20TokenTable.id == collateral_asset.a_token_id,
                            )
                        )
                    }"
                )
        users_to_check.clear()

    def verify_updated_debt_positions() -> None:
        for user_address, last_update_block in users_to_check.items():
            user = session.scalar(
                select(AaveV3UsersTable).where(
                    AaveV3UsersTable.address == user_address,
                    AaveV3UsersTable.market_id == market.id,
                )
            )
            if user is None:
                continue

            for debt_position in session.scalars(
                select(AaveV3DebtPositionsTable).where(
                    AaveV3DebtPositionsTable.user_id == user.id,
                )
            ):
                debt_asset = session.scalar(
                    select(AaveV3AssetsTable).where(
                        AaveV3AssetsTable.market_id == market.id,
                        AaveV3AssetsTable.id == debt_position.asset_id,
                    )
                )
                assert debt_asset is not None
                v_token_address = session.scalar(
                    select(Erc20TokenTable.address).where(
                        Erc20TokenTable.chain == market.chain_id,
                        Erc20TokenTable.id == debt_asset.v_token_id,
                    )
                )
                v_token_address = get_checksum_address(v_token_address)
                (actual_scaled_balance,) = raw_call(
                    w3=w3,
                    address=v_token_address,
                    calldata=encode_function_calldata(
                        function_prototype="scaledBalanceOf(address)",
                        function_arguments=[user_address],
                    ),
                    return_types=["uint256"],
                    block_identifier=last_update_block,
                )
                assert actual_scaled_balance == debt_position.balance, (
                    f"User {user_address}: debt balance ({debt_position.balance}) "
                    f"does not match contract ({actual_scaled_balance}) "
                    f"@ {
                        session.scalar(
                            select(Erc20TokenTable.address).where(
                                Erc20TokenTable.chain == market.chain_id,
                                Erc20TokenTable.id == debt_asset.v_token_id,
                            )
                        )
                    }"
                )

    for event in sorted(all_events, key=operator.itemgetter("blockNumber", "logIndex")):
        if event["blockNumber"] > last_check:
            verify_updated_collateral_positions()
            verify_updated_debt_positions()
            last_check = event["blockNumber"]

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
                    event=event,
                    market=market,
                    session=session,
                    users_to_check=users_to_check,
                )
            case AaveV3Event.SCALED_TOKEN_MINT.value:
                _process_scaled_token_mint_event(
                    event=event,
                    market=market,
                    session=session,
                    users_to_check=users_to_check,
                )
            case AaveV3Event.UPGRADED.value:
                _process_scaled_token_upgrade_event(
                    w3=w3,
                    event=event,
                    market=market,
                    session=session,
                )
            case AaveV3Event.SCALED_TOKEN_BALANCE_TRANSFER.value:
                _process_a_token_balance_transfer_event(
                    event=event,
                    market=market,
                    session=session,
                )
            case _:
                msg = (
                    f"Could not identify event with topic {event['topics'][0].to_0x_hex()}: {event}"
                )
                raise ValueError(msg)

    verify_updated_collateral_positions()
    verify_updated_debt_positions()
