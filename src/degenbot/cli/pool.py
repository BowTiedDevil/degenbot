"""CLI commands for pool state queries."""

from collections import defaultdict
from collections.abc import Callable
from typing import cast

import click
import eth_typing
import tqdm
from eth_typing.evm import BlockParams, ChecksumAddress
from hexbytes import HexBytes
from sqlalchemy import select
from tqdm.contrib.logging import logging_redirect_tqdm
from web3.types import LogReceipt

from degenbot import abi_decode
from degenbot.bot import Bot
from degenbot.checksum_cache import get_checksum_address
from degenbot.cli import cli
from degenbot.cli.pool_updater_configs import (
    V2PoolUpdateConfig,
    V3PoolUpdateConfig,
    V4PoolUpdateConfig,
    update_v2_pools,
    update_v3_pools,
    update_v4_pools,
)
from degenbot.cli.utils import get_provider_from_config
from degenbot.database.models.base import ExchangeTable
from degenbot.database.models.pools import (
    PoolManagerTable,
)
from degenbot.degenbot_rs import (
    LiquidityUpdateEvent,
    db_apply_v3_liquidity_updates,
    db_apply_v4_liquidity_updates,
    db_fetch_exchange,
    db_fetch_pool_row,
    db_set_exchange_last_update_block,
)
from degenbot.logging import logger
from degenbot.provider import ProviderAdapter
from degenbot.provider.block_helpers import get_number_for_block_identifier
from degenbot.provider.log_fetching import fetch_logs_retrying
from degenbot.types.aliases import ChainId

AERODROME_V2_POOLCREATED_EVENT_HASH = HexBytes(
    "0x2128d88d14c80cb081c1252a5acff7a264671bf199ce226b53788fb26065005e",
)
AERODROME_V3_POOLCREATED_EVENT_HASH = HexBytes(
    "0xab0d57f0df537bb25e80245ef7748fa62353808c54d6e528a9dd20887aed9ac2",
)

UNISWAP_V2_PAIRCREATED_EVENT_HASH = HexBytes(
    "0x0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9",
)
PANCAKESWAP_V2_PAIRCREATED_EVENT_HASH = UNISWAP_V2_PAIRCREATED_EVENT_HASH
SUSHISWAP_V2_PAIRCREATED_EVENT_HASH = UNISWAP_V2_PAIRCREATED_EVENT_HASH
SWAPBASED_V2_PAIRCREATED_EVENT_HASH = UNISWAP_V2_PAIRCREATED_EVENT_HASH

UNISWAP_V3_MINT_EVENT_HASH = HexBytes(
    "0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde",
)
UNISWAP_V3_BURN_EVENT_HASH = HexBytes(
    "0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c",
)
UNISWAP_V3_POOLCREATED_EVENT_HASH = HexBytes(
    "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118",
)
PANCAKESWAP_V3_POOLCREATED_EVENT_HASH = UNISWAP_V3_POOLCREATED_EVENT_HASH
SUSHISWAP_V3_POOLCREATED_EVENT_HASH = UNISWAP_V3_POOLCREATED_EVENT_HASH

UNISWAP_V4_POOLCREATED_EVENT_HASH = HexBytes(
    "0xdd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438",
)

UNISWAP_V4_MODIFYLIQUIDITY_EVENT_HASH = HexBytes(
    "0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec",
)


def apply_v3_liquidity_updates(
    provider: ProviderAdapter,
    pool_address: ChecksumAddress,
    liquidity_events: list[LogReceipt],
    exchanges_in_scope: set[ExchangeTable],
    *,
    database_path: str,
) -> None:
    """Apply the liquidity updates to the provided pool.

    This function assumes that the liquidity updates are ordered by block number and log index,
    ascending.

    Two invariants must be met:
        The block number for a new event must be equal to or greater than the last update stamp.
        For events from the same block as the last update stamp, the log index must be greater.

    A set of assertions guards these invariants, but the function otherwise makes no effort to
    verify the updates or validate the resulting mapping against the chain state.
    Omitting updates will corrupt the liquidity map!

    QJSCA5 §4.3: the reconstitute→apply-math→persist pipeline is owned by the
    Rust core (`db_apply_v3_liquidity_updates`); this shell decodes the raw
    `LogReceipt`s into `LiquidityUpdateEvent` records + delegates. The
    `exchanges_in_scope` precondition (a backfill double-apply guard) stays in
    Python (orchestration) — it reads the pool's `exchange_id` via the
    `db_fetch_pool_row` seam.
    """
    # Precondition: the pool's exchange must be in the in-scope set (backfill
    # double-apply guard). `db_fetch_pool_row` carries `exchange_id`.
    in_scope_exchange_ids = {exchange.id for exchange in exchanges_in_scope}
    pool_row = db_fetch_pool_row(
        database_path=database_path,
        chain_id=provider.chain_id,
        address=pool_address,
    )
    if pool_row is None or pool_row.exchange_id not in in_scope_exchange_ids:
        return

    # Decode the raw log receipts into the event records the Rust apply loop
    # consumes. The V3 decode: tick bounds from topics[2..3]; a Burn-aware
    # signed `liquidity_delta` (Burn negates). amount==0 events are skipped
    # (the Rust loop would no-op them anyway, but skipping avoids the record).
    decoded_events: list[LiquidityUpdateEvent] = []
    for liquidity_event in liquidity_events:
        (tick_lower,) = abi_decode(["int24"], liquidity_event["topics"][2])
        (tick_upper,) = abi_decode(["int24"], liquidity_event["topics"][3])

        if liquidity_event["topics"][0] == UNISWAP_V3_BURN_EVENT_HASH:
            amount, _, _ = abi_decode(
                ["uint128", "uint256", "uint256"],
                liquidity_event["data"],
            )
            amount = -amount
        else:
            _, amount, _, _ = abi_decode(
                ["address", "uint128", "uint256", "uint256"],
                liquidity_event["data"],
            )

        if amount == 0:
            continue

        decoded_events.append(
            LiquidityUpdateEvent(
                liquidity_event["blockNumber"],
                liquidity_event["logIndex"],
                tick_lower,
                tick_upper,
                amount,
            )
        )

    # Delegate the reconstitute→apply→persist→stamp pipeline to the Rust core.
    # The block/log-index ordering invariant is enforced in Rust (panics on
    # violation, matching the prior Python `assert`s).
    db_apply_v3_liquidity_updates(
        database_path=database_path,
        chain_id=provider.chain_id,
        pool_address=pool_address,
        events=decoded_events,
    )


def apply_v4_liquidity_updates(
    pool_id: HexBytes,
    liquidity_events: list[LogReceipt],
    pool_manager: PoolManagerTable,
    *,
    database_path: str,
) -> None:
    """Apply the liquidity updates to the provided pool.

    This function assumes that the liquidity updates are ordered by block number and log index,
    ascending.

    Two invariants must be met:
        The block number for a new event must be equal to or greater than the last update stamp.
        For events from the same block as the last update stamp, the log index must be greater.

    A set of assertions guards these invariants, but the function otherwise makes no effort to
    verify the updates or validate the resulting mapping against the chain state.
    Omitting updates will corrupt the liquidity map!

    QJSCA5 §4.3: the reconstitute→apply-math→persist pipeline is owned by the
    Rust core (`db_apply_v4_liquidity_updates`); this shell decodes the raw
    `LogReceipt`s into `LiquidityUpdateEvent` records + delegates. V4 emits a
    single signed `Modify` event (no Burn/Mint split), so the decode unpacks
    `(tick_lower, tick_upper, liquidity_delta, _)` straight from the `data` blob.
    """
    decoded_events: list[LiquidityUpdateEvent] = []
    for liquidity_event in liquidity_events:
        tick_lower, tick_upper, liquidity_delta, _ = abi_decode(
            types=["int24", "int24", "int256", "bytes32"],
            data=liquidity_event["data"],
        )

        if liquidity_delta == 0:
            continue

        decoded_events.append(
            LiquidityUpdateEvent(
                liquidity_event["blockNumber"],
                liquidity_event["logIndex"],
                tick_lower,
                tick_upper,
                liquidity_delta,
            )
        )

    # Delegate the reconstitute→apply→persist→stamp pipeline to the Rust core
    # (the block/log-index ordering invariant is enforced in Rust).
    db_apply_v4_liquidity_updates(
        database_path=database_path,
        pool_hash_hex=pool_id.to_0x_hex(),
        pool_manager_chain=pool_manager.chain,
        events=decoded_events,
    )


# --- Pool updater configurations ---

_V2_CONFIGS: dict[str, V2PoolUpdateConfig] = {
    "aerodrome_v2": V2PoolUpdateConfig(
        name="aerodrome_v2",
        event_hash=AERODROME_V2_POOLCREATED_EVENT_HASH,
        fee_token0=0,  # Overridden by RPC
        fee_token1=0,  # Overridden by RPC
        fee_denominator=10_000,
        has_stable_flag=True,
        rpc_fee_call="getFee(address,bool)",
        rpc_fee_return_types=["uint256"],
        rpc_fee_includes_stable=True,
    ),
    "pancakeswap_v2": V2PoolUpdateConfig(
        name="pancakeswap_v2",
        event_hash=PANCAKESWAP_V2_PAIRCREATED_EVENT_HASH,
        fee_token0=25,
        fee_token1=25,
        fee_denominator=10000,
    ),
    "sushiswap_v2": V2PoolUpdateConfig(
        name="sushiswap_v2",
        event_hash=SUSHISWAP_V2_PAIRCREATED_EVENT_HASH,
        fee_token0=3,
        fee_token1=3,
        fee_denominator=1000,
    ),
    "swapbased_v2": V2PoolUpdateConfig(
        name="swapbased_v2",
        event_hash=SWAPBASED_V2_PAIRCREATED_EVENT_HASH,
        fee_token0=3,
        fee_token1=3,
        fee_denominator=1000,
    ),
    "uniswap_v2": V2PoolUpdateConfig(
        name="uniswap_v2",
        event_hash=UNISWAP_V2_PAIRCREATED_EVENT_HASH,
        fee_token0=3,
        fee_token1=3,
        fee_denominator=1000,
    ),
}

_V3_CONFIGS: dict[str, V3PoolUpdateConfig] = {
    "aerodrome_v3": V3PoolUpdateConfig(
        name="aerodrome_v3",
        event_hash=AERODROME_V3_POOLCREATED_EVENT_HASH,
        fee_denominator=1_000_000,
        rpc_fee_call="getSwapFee(address)",
        rpc_fee_return_types=["uint24"],
    ),
    "pancakeswap_v3": V3PoolUpdateConfig(
        name="pancakeswap_v3",
        event_hash=PANCAKESWAP_V3_POOLCREATED_EVENT_HASH,
        fee_denominator=1_000_000,
    ),
    "sushiswap_v3": V3PoolUpdateConfig(
        name="sushiswap_v3",
        event_hash=SUSHISWAP_V3_POOLCREATED_EVENT_HASH,
        fee_denominator=1_000_000,
    ),
    "uniswap_v3": V3PoolUpdateConfig(
        name="uniswap_v3",
        event_hash=UNISWAP_V3_POOLCREATED_EVENT_HASH,
        fee_denominator=1_000_000,
    ),
}

_V4_CONFIGS: dict[str, V4PoolUpdateConfig] = {
    "uniswap_v4": V4PoolUpdateConfig(
        name="uniswap_v4",
        event_hash=UNISWAP_V4_POOLCREATED_EVENT_HASH,
        fee_denominator=1_000_000,
    ),
}


def _pool_updater(
    provider: ProviderAdapter,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
    *,
    database_path: str,
) -> None:
    """Dispatch to the appropriate parameterized pool updater.

    Raises:
        ValueError: See function documentation.

    """
    exchange_name = exchange.name

    if exchange_name in _V2_CONFIGS:
        update_v2_pools(
            provider,
            start_block,
            end_block,
            exchange,
            database_path=database_path,
            config=_V2_CONFIGS[exchange_name],
            get_events_fn=get_events_from_contract,
        )
    elif exchange_name in _V3_CONFIGS:
        update_v3_pools(
            provider,
            start_block,
            end_block,
            exchange,
            database_path=database_path,
            config=_V3_CONFIGS[exchange_name],
            get_events_fn=get_events_from_contract,
        )
    elif exchange_name in _V4_CONFIGS:
        update_v4_pools(
            provider,
            start_block,
            end_block,
            exchange,
            database_path=database_path,
            config=_V4_CONFIGS[exchange_name],
            get_events_fn=get_events_from_contract,
        )
    else:
        msg = f"No updater configuration for exchange {exchange_name!r}"
        raise ValueError(msg)


@cli.group()
def pool() -> None:
    """Pool commands."""


@pool.command("update")
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
@click.pass_obj
def pool_update(bot: Bot, chunk_size: int, to_block: str) -> None:
    """Update liquidity pool information for activated exchanges.

    Raises:
        ValueError: See function documentation.

    """
    with bot.db() as session, logging_redirect_tqdm(loggers=[logger]):
        active_chains = set(
            session.scalars(select(ExchangeTable.chain_id).where(ExchangeTable.active)).all(),
        )

        for chain_id in active_chains:
            provider = get_provider_from_config(chain_id=chain_id)

            active_exchanges = session.scalars(
                select(ExchangeTable).where(
                    ExchangeTable.active,
                    ExchangeTable.chain_id == chain_id,
                ),
            ).all()

            initial_start_block = working_start_block = min(
                0 if exchange.last_update_block is None else exchange.last_update_block + 1
                for exchange in active_exchanges
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
                    get_number_for_block_identifier(identifier=block_tag, provider=provider)
                    + block_offset
                )

            latest_block = provider.get_block("latest")
            if latest_block is None:
                msg = "Could not fetch latest block"
                raise ValueError(msg)
            if last_block > latest_block["number"]:
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

            exchanges_to_update: set[ExchangeTable] = set()

            while True:
                # Read ``last_update_block`` ground-truth from the Rust core
                # (a fresh connection per call → a fresh WAL snapshot). The
                # stamp is written by the ``db_set_exchange_last_update_block``
                # seam on its **own** connection, which the long-lived
                # SQLAlchemy session's read snapshot cannot see — trusting the
                # stale ORM ``exchange.last_update_block`` attribute here would
                # freeze chunk advancement after the second chunk.
                fresh_last_update_block: dict[int, int | None] = {}
                for exchange in active_exchanges:
                    row = db_fetch_exchange(
                        database_path=str(bot.config.database.path),
                        exchange_id=exchange.id,
                    )
                    fresh_last_update_block[exchange.id] = (
                        row.last_update_block if row is not None else None
                    )
                # Cap the working end block at the lowest of:
                # - the safe block for the chain
                # - the end of the working chunk size
                # - all update blocks for active exchanges
                working_end_block = min(
                    [last_block]
                    + [working_start_block + chunk_size - 1]
                    + [
                        fresh_last_update_block[exchange.id]
                        for exchange in active_exchanges
                        if fresh_last_update_block[exchange.id] is not None
                        if fresh_last_update_block[exchange.id] > working_start_block
                    ],
                )
                assert working_end_block >= working_start_block

                exchanges_to_update = {
                    exchange
                    for exchange in active_exchanges
                    if (
                        fresh_last_update_block[exchange.id] is None
                        or fresh_last_update_block[exchange.id] + 1 == working_start_block
                    )
                }

                for exchange in exchanges_to_update:
                    pool_updater = POOL_UPDATER[chain_id, exchange.name]
                    pool_updater(
                        provider,
                        working_start_block,
                        working_end_block,
                        exchange,
                        database_path=str(bot.config.database.path),
                    )

                # Fetch and process V3 liquidity events
                if any("_v3" in exchange.name for exchange in exchanges_to_update):
                    for pool_address, liquidity_events in tqdm.tqdm(
                        get_v3_liquidity_events(
                            provider=provider,
                            start_block=working_start_block,
                            end_block=working_end_block,
                        ).items(),
                        desc="Updating V3 pool liquidity",
                        bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
                        leave=False,
                    ):
                        # V3 events are emitted by individual pools, which cannot efficiently be
                        # filtered by eth_getLogs to only include in-scope exchanges — some may have
                        # millions of deployed pools, which quickly scales beyond JSON-RPC query
                        # limits.
                        # Nevertheless filtering is required to avoid double-applying events during
                        # backfills, so the updater function looks up the exchange for each pool,
                        # checks if it is included in the in-scope set, and returns early if not.
                        apply_v3_liquidity_updates(
                            provider=provider,
                            pool_address=pool_address,
                            liquidity_events=liquidity_events,
                            exchanges_in_scope=exchanges_to_update,
                            database_path=str(bot.config.database.path),
                        )

                # Fetch and process V4 liquidity events
                for v4_exchange in (
                    exchange for exchange in exchanges_to_update if "_v4" in exchange.name
                ):
                    pool_manager_in_db = session.scalar(
                        select(PoolManagerTable).where(
                            PoolManagerTable.address == v4_exchange.factory,
                            PoolManagerTable.chain == chain_id,
                        ),
                    )
                    assert pool_manager_in_db is not None
                    pool_manager_address = get_checksum_address(pool_manager_in_db.address)

                    for pool_id, liquidity_events in tqdm.tqdm(
                        get_v4_liquidity_events(
                            provider=provider,
                            start_block=working_start_block,
                            end_block=working_end_block,
                            address=pool_manager_address,
                        ).items(),
                        desc="Updating V4 pool liquidity",
                        bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
                        leave=False,
                    ):
                        apply_v4_liquidity_updates(
                            pool_id=pool_id,
                            liquidity_events=liquidity_events,
                            pool_manager=pool_manager_in_db,
                            database_path=str(bot.config.database.path),
                        )

                # At this point, all exchanges have been updated and the invariant checks have
                # passed, so stamp the update block and commit to the DB
                for exchange in exchanges_to_update:
                    db_set_exchange_last_update_block(
                        database_path=str(bot.config.database.path),
                        chain_id=exchange.chain_id,
                        exchange_id=exchange.id,
                        block=working_end_block,
                    )
                exchanges_to_update.clear()
                session.commit()

                if working_end_block == last_block:
                    break
                working_start_block = working_end_block + 1

                block_pbar.n = working_end_block - initial_start_block
                block_pbar.refresh()

            block_pbar.close()


def get_events_from_contract(
    provider: ProviderAdapter,
    start_block: int,
    end_block: int,
    address: ChecksumAddress,
    event_hash: HexBytes,
) -> list[LogReceipt]:
    """Return events from contract.

    Returns:
        A list of results.

    """
    return fetch_logs_retrying(
        provider=provider,
        start_block=start_block,
        end_block=end_block,
        address=[address],
        topic_signature=[event_hash],
    )


def get_v3_liquidity_events(
    provider: ProviderAdapter,
    start_block: int,
    end_block: int,
    address: ChecksumAddress | None = None,
) -> dict[ChecksumAddress, list[LogReceipt]]:
    """Fetch new Mint & Burn events for the given range.

    Returns:
        The computed value.

    """
    pool_updates: dict[ChecksumAddress, list[LogReceipt]] = defaultdict(list)

    for liquidity_event in fetch_logs_retrying(
        provider=provider,
        start_block=start_block,
        end_block=end_block,
        address=None if address is None else [address],
        topic_signature=[
            # matches topic0 on `Mint` OR `Burn`
            [UNISWAP_V3_MINT_EVENT_HASH, UNISWAP_V3_BURN_EVENT_HASH],
        ],
    ):
        # Ignore zero-amount events
        if any(liquidity_event["data"][:32]):
            pool_updates[liquidity_event["address"]].append(liquidity_event)

    return pool_updates


def get_v4_liquidity_events(
    provider: ProviderAdapter,
    start_block: int,
    end_block: int,
    address: ChecksumAddress | None = None,
) -> dict[HexBytes, list[LogReceipt]]:
    """Fetch new ModifyLiquidity events for the given range.

    Returns:
        The computed value.

    """
    pool_updates: dict[HexBytes, list[LogReceipt]] = defaultdict(list)

    for liquidity_event in fetch_logs_retrying(
        provider=provider,
        start_block=start_block,
        end_block=end_block,
        address=None if address is None else [address],
        topic_signature=[
            # matches topic0 on `ModifyLiquidity`
            [UNISWAP_V4_MODIFYLIQUIDITY_EVENT_HASH],
        ],
    ):
        # Ignores zero-amount events
        if any(liquidity_event["data"][64:96]):
            pool_updates[liquidity_event["topics"][1]].append(liquidity_event)

    return pool_updates


POOL_UPDATER: dict[
    tuple[ChainId, str],
    Callable[..., None],
] = {
    (eth_typing.ChainId.BASE, "aerodrome_v2"): _pool_updater,
    (eth_typing.ChainId.BASE, "aerodrome_v3"): _pool_updater,
    (eth_typing.ChainId.BASE, "pancakeswap_v2"): _pool_updater,
    (eth_typing.ChainId.BASE, "pancakeswap_v3"): _pool_updater,
    (eth_typing.ChainId.BASE, "sushiswap_v2"): _pool_updater,
    (eth_typing.ChainId.BASE, "sushiswap_v3"): _pool_updater,
    (eth_typing.ChainId.BASE, "swapbased_v2"): _pool_updater,
    (eth_typing.ChainId.BASE, "uniswap_v2"): _pool_updater,
    (eth_typing.ChainId.BASE, "uniswap_v3"): _pool_updater,
    (eth_typing.ChainId.BASE, "uniswap_v4"): _pool_updater,
    (eth_typing.ChainId.ETH, "pancakeswap_v2"): _pool_updater,
    (eth_typing.ChainId.ETH, "pancakeswap_v3"): _pool_updater,
    (eth_typing.ChainId.ETH, "sushiswap_v2"): _pool_updater,
    (eth_typing.ChainId.ETH, "sushiswap_v3"): _pool_updater,
    (eth_typing.ChainId.ETH, "uniswap_v2"): _pool_updater,
    (eth_typing.ChainId.ETH, "uniswap_v3"): _pool_updater,
    (eth_typing.ChainId.ETH, "uniswap_v4"): _pool_updater,
}
