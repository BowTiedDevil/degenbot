import contextlib
import itertools
from collections import defaultdict, deque
from collections.abc import Callable
from pathlib import Path
from typing import Any

import click
import eth_typing
import pydantic
import tqdm
from eth_abi.abi import decode as abi_decode
from eth_typing.evm import BlockParams, ChecksumAddress
from hexbytes import HexBytes
from pydantic import HttpUrl, WebsocketUrl
from sqlalchemy import delete, select
from sqlalchemy.dialects.sqlite import insert as sqlite_upsert
from web3 import HTTPProvider, IPCProvider, LegacyWebSocketProvider, Web3
from web3.types import LogReceipt

from degenbot.checksum_cache import get_checksum_address
from degenbot.cli import cli
from degenbot.config import CONFIG_FILE, settings
from degenbot.connection import connection_manager
from degenbot.constants import MAX_UINT256
from degenbot.database import db_session
from degenbot.database.models.base import ExchangeTable
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import (
    AbstractUniswapV3Pool,
    AbstractUniswapV4Pool,
    AerodromeV2PoolTable,
    AerodromeV3PoolTable,
    InitializationMapTable,
    LiquidityPoolTable,
    LiquidityPositionTable,
    ManagedPoolInitializationMapTable,
    ManagedPoolLiquidityPositionTable,
    PancakeswapV2PoolTable,
    PancakeswapV3PoolTable,
    PoolManagerTable,
    SushiswapV2PoolTable,
    SushiswapV3PoolTable,
    SwapbasedV2PoolTable,
    UniswapV2PoolTable,
    UniswapV3PoolTable,
    UniswapV4PoolTable,
)
from degenbot.functions import (
    encode_function_calldata,
    fetch_logs_retrying,
    get_number_for_block_identifier,
    raw_call,
)
from degenbot.types.aliases import ChainId, Tick, Word
from degenbot.types.concrete import BoundedCache
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3PoolLiquidityMappingUpdate,
    UniswapV3PoolState,
)
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool
from degenbot.uniswap.v4_types import (
    UniswapV4BitmapAtWord,
    UniswapV4LiquidityAtTick,
    UniswapV4PoolKey,
    UniswapV4PoolLiquidityMappingUpdate,
    UniswapV4PoolState,
)


class MockV3LiquidityPool(UniswapV3Pool):
    """
    A lightweight mock for a V3 liquidity pool. Used to simulate liquidity updates and export
    validated mappings.
    """

    def __init__(self) -> None:
        self.sparse_liquidity_map = False
        self._initial_state_block = MAX_UINT256  # Skip the the in-range liquidity modification step

        # No-op context manager to avoid locking overhead
        self._state_lock = contextlib.nullcontext()  # type:ignore[assignment]
        self._state_cache = BoundedCache(max_items=8)
        self.name = "V3 POOL"

    def _invalidate_range_cache_for_ticks(self, *args: Any, **kwargs: Any) -> None: ...

    def _notify_subscribers(self, *args: Any, **kwargs: Any) -> None: ...


class MockV4LiquidityPool(UniswapV4Pool):
    """
    A lightweight mock for a V4 liquidity pool. Used to simulate liquidity updates and export
    validated mappings.
    """

    def __init__(self) -> None:
        self.sparse_liquidity_map = False
        self._initial_state_block = MAX_UINT256  # Skip the the in-range liquidity modification step

        # No-op context manager to avoid locking overhead
        self._state_lock = contextlib.nullcontext()  # type:ignore[assignment]
        self._state_cache = BoundedCache(max_items=8)
        self.name = "V4 POOL"

    def _invalidate_range_cache_for_ticks(self, *args: Any, **kwargs: Any) -> None: ...

    def _notify_subscribers(self, *args: Any, **kwargs: Any) -> None: ...


class TicksAtWord(pydantic.BaseModel):
    bitmap: int


class LiquidityAtTick(pydantic.BaseModel):
    liquidity_net: int
    liquidity_gross: int


class PoolLiquidityMap(pydantic.BaseModel):
    tick_bitmap: dict[Word, TicksAtWord]
    tick_data: dict[Tick, LiquidityAtTick]


AERODROME_V2_POOLCREATED_EVENT_HASH = HexBytes(
    "0x2128d88d14c80cb081c1252a5acff7a264671bf199ce226b53788fb26065005e"
)
AERODROME_V3_POOLCREATED_EVENT_HASH = HexBytes(
    "0xab0d57f0df537bb25e80245ef7748fa62353808c54d6e528a9dd20887aed9ac2"
)

UNISWAP_V2_PAIRCREATED_EVENT_HASH = HexBytes(
    "0x0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9"
)
PANCAKESWAP_V2_PAIRCREATED_EVENT_HASH = UNISWAP_V2_PAIRCREATED_EVENT_HASH
SUSHISWAP_V2_PAIRCREATED_EVENT_HASH = UNISWAP_V2_PAIRCREATED_EVENT_HASH
SWAPBASED_V2_PAIRCREATED_EVENT_HASH = UNISWAP_V2_PAIRCREATED_EVENT_HASH

UNISWAP_V3_MINT_EVENT_HASH = HexBytes(
    "0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde"
)
UNISWAP_V3_BURN_EVENT_HASH = HexBytes(
    "0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c"
)
UNISWAP_V3_POOLCREATED_EVENT_HASH = HexBytes(
    "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"
)
PANCAKESWAP_V3_POOLCREATED_EVENT_HASH = UNISWAP_V3_POOLCREATED_EVENT_HASH
SUSHISWAP_V3_POOLCREATED_EVENT_HASH = UNISWAP_V3_POOLCREATED_EVENT_HASH

UNISWAP_V4_POOLCREATED_EVENT_HASH = HexBytes(
    "0xdd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438"
)

UNISWAP_V4_MODIFYLIQUIDITY_EVENT_HASH = HexBytes(
    "0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec"
)


def apply_v3_liquidity_updates(
    w3: Web3,
    pool_address: ChecksumAddress,
    liquidity_events: deque[LogReceipt],
    exchanges_in_scope: set[ExchangeTable],
) -> None:
    """
    Apply the liquidity updates to the provided pool.

    This function assumes that the liquidity updates are ordered by block number and log index,
    ascending.

    Two invariants must be met:
        The block number for a new event must be equal to or greater than the last update stamp.
        For events from the same block as the last update stamp, the log index must be greater.

    A set of assertions guards these invariants, but the function otherwise makes no effort to
    verify the updates or validate the resulting mapping against the chain state.
    Omitting updates will corrupt the liquidity map!
    """

    pool_in_db = db_session.scalar(
        select(LiquidityPoolTable).where(
            LiquidityPoolTable.address == pool_address,
            LiquidityPoolTable.chain == w3.eth.chain_id,
        )
    )

    if (pool_in_db is None) or (pool_in_db.exchange not in exchanges_in_scope):
        return

    assert isinstance(pool_in_db, AbstractUniswapV3Pool)

    pool_liquidity_map = PoolLiquidityMap.model_construct(
        tick_bitmap={
            mapping.word: TicksAtWord.model_construct(
                bitmap=mapping.bitmap,
            )
            for mapping in pool_in_db.initialization_maps
        },
        tick_data={
            position.tick: LiquidityAtTick.model_construct(
                liquidity_gross=position.liquidity_gross,
                liquidity_net=position.liquidity_net,
            )
            for position in pool_in_db.liquidity_positions
        },
    )

    lp_helper = MockV3LiquidityPool()
    lp_helper.address = pool_address
    lp_helper.tick_spacing = pool_in_db.tick_spacing
    lp_helper._state = UniswapV3PoolState(  # noqa: SLF001
        address=pool_address,
        block=0,
        liquidity=MAX_UINT256,
        sqrt_price_x96=0,
        tick=0,
        tick_bitmap={
            k: UniswapV3BitmapAtWord.model_construct(
                bitmap=v.bitmap,
            )
            for k, v in pool_liquidity_map.tick_bitmap.items()
        },
        tick_data={
            k: UniswapV3LiquidityAtTick.model_construct(
                liquidity_gross=v.liquidity_gross,
                liquidity_net=v.liquidity_net,
            )
            for k, v in pool_liquidity_map.tick_data.items()
        },
    )

    while liquidity_events:
        liquidity_event = liquidity_events.popleft()

        # Guard against applying a liquidity event that occured in the past
        if (
            pool_in_db.liquidity_update_block is not None
            and pool_in_db.liquidity_update_log_index is not None
        ):
            if liquidity_event["blockNumber"] == pool_in_db.liquidity_update_block:
                assert liquidity_event["logIndex"] > pool_in_db.liquidity_update_log_index
            else:
                assert liquidity_event["blockNumber"] > pool_in_db.liquidity_update_block

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

        lp_helper.update_liquidity_map(
            update=UniswapV3PoolLiquidityMappingUpdate(
                block_number=liquidity_event["blockNumber"],
                liquidity=amount,
                tick_lower=tick_lower,
                tick_upper=tick_upper,
            )
        )

        pool_in_db.liquidity_update_block = liquidity_event["blockNumber"]
        pool_in_db.liquidity_update_log_index = liquidity_event["logIndex"]

    # After all events have been processed, write the liquidity positions and tick
    # initialization maps to the DB — adding new, updating existing, and dropping stale entries
    db_ticks = {position.tick for position in pool_in_db.liquidity_positions}
    helper_ticks = set(lp_helper.tick_data)

    # Drop any positions found in the DB but not the helper
    if ticks_to_drop := db_ticks - helper_ticks:
        db_session.execute(
            delete(LiquidityPositionTable).where(
                LiquidityPositionTable.pool_id == pool_in_db.id,
                LiquidityPositionTable.tick.in_(ticks_to_drop),
            )
        )

    # Upsert remaining ticks
    if helper_ticks:
        # Chunk the upserts to stay below SQLite's limit of 32,766 variables
        # per batch statement. ref: https://www.sqlite.org/limits.html
        keys_per_row = 4
        chunk_size = 30_000 // keys_per_row

        for tick_chunk in itertools.batched(helper_ticks, chunk_size):
            stmt = sqlite_upsert(LiquidityPositionTable).values(
                [
                    {
                        "pool_id": pool_in_db.id,
                        "tick": tick,
                        "liquidity_net": lp_helper.tick_data[tick].liquidity_net,
                        "liquidity_gross": lp_helper.tick_data[tick].liquidity_gross,
                    }
                    for tick in tick_chunk
                ]
            )
            stmt = stmt.on_conflict_do_update(
                index_elements=[
                    LiquidityPositionTable.pool_id,
                    LiquidityPositionTable.tick,
                ],
                set_={
                    "liquidity_net": stmt.excluded.liquidity_net,
                    "liquidity_gross": stmt.excluded.liquidity_gross,
                },
                where=(LiquidityPositionTable.liquidity_net != stmt.excluded.liquidity_net)
                | (LiquidityPositionTable.liquidity_gross != stmt.excluded.liquidity_gross),
            )
            db_session.execute(stmt)

    db_words = {map_.word for map_ in pool_in_db.initialization_maps}
    helper_words = {
        word
        for word, map_ in lp_helper.tick_bitmap.items()
        if map_.bitmap != 0  # exclude maps where all ticks are uninitialized
    }

    # Drop any initialization map found in the DB but not the helper
    if words_to_drop := db_words - helper_words:
        db_session.execute(
            delete(InitializationMapTable).where(
                InitializationMapTable.pool_id == pool_in_db.id,
                InitializationMapTable.word.in_(words_to_drop),
            )
        )

    # Upsert remaining maps
    if helper_words:
        # Chunk the upserts to stay below SQLite's limit of 32,766 variables
        # per batch statement. ref: https://www.sqlite.org/limits.html
        keys_per_row = 3
        chunk_size = 30_000 // keys_per_row

        for word_chunk in itertools.batched(helper_words, chunk_size):
            stmt = sqlite_upsert(InitializationMapTable).values(
                [
                    {
                        "pool_id": pool_in_db.id,
                        "word": word,
                        "bitmap": lp_helper.tick_bitmap[word].bitmap,
                    }
                    for word in word_chunk
                    if lp_helper.tick_bitmap[word].bitmap != 0
                ]
            )
            stmt = stmt.on_conflict_do_update(
                index_elements=[
                    InitializationMapTable.pool_id,
                    InitializationMapTable.word,
                ],
                set_={
                    "bitmap": stmt.excluded.bitmap,
                },
                where=InitializationMapTable.bitmap != stmt.excluded.bitmap,
            )
            db_session.execute(stmt)


def apply_v4_liquidity_updates(
    pool_id: HexBytes,
    liquidity_events: deque[LogReceipt],
    pool_manager: PoolManagerTable,
) -> None:
    """
    Apply the liquidity updates to the provided pool.

    This function assumes that the liquidity updates are ordered by block number and log index,
    ascending.

    Two invariants must be met:
        The block number for a new event must be equal to or greater than the last update stamp.
        For events from the same block as the last update stamp, the log index must be greater.

    A set of assertions guards these invariants, but the function otherwise makes no effort to
    verify the updates or validate the resulting mapping against the chain state.
    Omitting updates will corrupt the liquidity map!
    """

    pool_in_db = db_session.scalar(
        select(UniswapV4PoolTable).where(
            UniswapV4PoolTable.pool_hash == pool_id.to_0x_hex(),
            UniswapV4PoolTable.manager.has(id=pool_manager.id),
        )
    )

    assert isinstance(pool_in_db, AbstractUniswapV4Pool)

    pool_liquidity_map = PoolLiquidityMap.model_construct(
        tick_bitmap={
            mapping.word: TicksAtWord.model_construct(
                bitmap=mapping.bitmap,
            )
            for mapping in pool_in_db.initialization_maps
        },
        tick_data={
            position.tick: LiquidityAtTick.model_construct(
                liquidity_gross=position.liquidity_gross,
                liquidity_net=position.liquidity_net,
            )
            for position in pool_in_db.liquidity_positions
        },
    )

    lp_helper = MockV4LiquidityPool()
    lp_helper._pool_manager_address = pool_in_db.manager.exchange.factory  # noqa: SLF001

    # Construct the PoolKey
    lp_helper._pool_key = UniswapV4PoolKey(  # noqa: SLF001
        currency0=pool_in_db.currency0.address,
        currency1=pool_in_db.currency1.address,
        fee=pool_in_db.fee_currency0,
        tick_spacing=pool_in_db.tick_spacing,
        hooks=pool_in_db.hooks,
    )

    pool_liquidity_map = PoolLiquidityMap.model_construct(
        tick_bitmap={
            map_.word: TicksAtWord.model_construct(
                bitmap=map_.bitmap,
            )
            for map_ in pool_in_db.initialization_maps
        },
        tick_data={
            position.tick: LiquidityAtTick.model_construct(
                liquidity_gross=position.liquidity_gross,
                liquidity_net=position.liquidity_net,
            )
            for position in pool_in_db.liquidity_positions
        },
    )

    lp_helper._state = UniswapV4PoolState(  # noqa: SLF001
        address=pool_in_db.manager.exchange.factory,
        block=0,
        liquidity=MAX_UINT256,
        sqrt_price_x96=0,
        tick=0,
        tick_bitmap={
            k: UniswapV4BitmapAtWord.model_construct(
                bitmap=v.bitmap,
            )
            for k, v in pool_liquidity_map.tick_bitmap.items()
        },
        tick_data={
            k: UniswapV4LiquidityAtTick.model_construct(
                liquidity_gross=v.liquidity_gross,
                liquidity_net=v.liquidity_net,
            )
            for k, v in pool_liquidity_map.tick_data.items()
        },
        id=HexBytes(pool_in_db.pool_hash),
    )

    while liquidity_events:
        liquidity_event = liquidity_events.popleft()

        # Guard against applying a liquidity event that occured in the past
        if (
            pool_in_db.liquidity_update_block is not None
            and pool_in_db.liquidity_update_log_index is not None
        ):
            if liquidity_event["blockNumber"] == pool_in_db.liquidity_update_block:
                assert liquidity_event["logIndex"] > pool_in_db.liquidity_update_log_index
            else:
                assert liquidity_event["blockNumber"] > pool_in_db.liquidity_update_block

        tick_lower, tick_upper, liquidity_delta, _ = abi_decode(
            types=["int24", "int24", "int256", "bytes32"],
            data=liquidity_event["data"],
        )

        if liquidity_delta == 0:
            continue

        lp_helper.update_liquidity_map(
            update=UniswapV4PoolLiquidityMappingUpdate(
                block_number=liquidity_event["blockNumber"],
                liquidity=liquidity_delta,
                tick_lower=tick_lower,
                tick_upper=tick_upper,
            )
        )

        pool_in_db.liquidity_update_block = liquidity_event["blockNumber"]
        pool_in_db.liquidity_update_log_index = liquidity_event["logIndex"]

    # After all events have been processed, write the liquidity positions and tick initialization
    # maps to the DB, updating and adding positions as necessary and dropping stale entries

    db_ticks = {position.tick for position in pool_in_db.liquidity_positions}
    helper_ticks = set(lp_helper.tick_data)

    # Drop any positions found in the DB but not the helper
    if ticks_to_drop := db_ticks - helper_ticks:
        db_session.execute(
            delete(ManagedPoolLiquidityPositionTable).where(
                ManagedPoolLiquidityPositionTable.managed_pool_id == pool_in_db.id,
                ManagedPoolLiquidityPositionTable.tick.in_(ticks_to_drop),
            )
        )

    # Upsert remaining ticks
    if helper_ticks:
        # Chunk the upserts to stay below SQLite's limit of 32,766 variables
        # per batch statement. ref: https://www.sqlite.org/limits.html
        keys_per_row = 4
        chunk_size = 30_000 // keys_per_row

        for tick_chunk in itertools.batched(helper_ticks, chunk_size):
            stmt = sqlite_upsert(ManagedPoolLiquidityPositionTable).values(
                [
                    {
                        "managed_pool_id": pool_in_db.id,
                        "tick": tick,
                        "liquidity_net": lp_helper.tick_data[tick].liquidity_net,
                        "liquidity_gross": lp_helper.tick_data[tick].liquidity_gross,
                    }
                    for tick in tick_chunk
                ]
            )
            stmt = stmt.on_conflict_do_update(
                index_elements=[
                    ManagedPoolLiquidityPositionTable.managed_pool_id,
                    ManagedPoolLiquidityPositionTable.tick,
                ],
                set_={
                    "liquidity_net": stmt.excluded.liquidity_net,
                    "liquidity_gross": stmt.excluded.liquidity_gross,
                },
                where=(
                    ManagedPoolLiquidityPositionTable.liquidity_net != stmt.excluded.liquidity_net
                )
                | (
                    ManagedPoolLiquidityPositionTable.liquidity_gross
                    != stmt.excluded.liquidity_gross
                ),
            )
            db_session.execute(stmt)

    db_words = {map_.word for map_ in pool_in_db.initialization_maps}
    helper_words = {
        word
        for word, map_ in lp_helper.tick_bitmap.items()
        if map_.bitmap != 0  # exclude maps where all ticks are uninitialized
    }

    # Drop any initialization map found in the DB but not the helper
    if words_to_drop := db_words - helper_words:
        db_session.execute(
            delete(ManagedPoolInitializationMapTable).where(
                ManagedPoolInitializationMapTable.managed_pool_id == pool_in_db.id,
                ManagedPoolInitializationMapTable.word.in_(words_to_drop),
            )
        )

    # Upsert remaining maps
    if helper_words:
        # Chunk the upserts to stay below SQLite's limit of 32,766 variables
        # per batch statement. ref: https://www.sqlite.org/limits.html
        keys_per_row = 3
        chunk_size = 30_000 // keys_per_row

        for word_chunk in itertools.batched(helper_words, chunk_size):
            stmt = sqlite_upsert(ManagedPoolInitializationMapTable).values(
                [
                    {
                        "managed_pool_id": pool_in_db.id,
                        "word": word,
                        "bitmap": lp_helper.tick_bitmap[word].bitmap,
                    }
                    for word in word_chunk
                    if lp_helper.tick_bitmap[word].bitmap != 0
                ]
            )
            stmt = stmt.on_conflict_do_update(
                index_elements=[
                    ManagedPoolInitializationMapTable.managed_pool_id,
                    ManagedPoolInitializationMapTable.word,
                ],
                set_={
                    "bitmap": stmt.excluded.bitmap,
                },
                where=ManagedPoolInitializationMapTable.bitmap != stmt.excluded.bitmap,
            )
            db_session.execute(stmt)


def base_aerodrome_v2_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Aerodrome V2 liquidity pools deployed on Base mainnet and add their metadata to the
    DB.
    """

    database_type = AerodromeV2PoolTable

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=AERODROME_V2_POOLCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)

            (stable,) = abi_decode(["bool"], new_pool_event["topics"][3])

            token0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token0,
                )
            )
            token1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token1,
                )
            )

            if token0_in_db is None:
                token0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token0,
                )
                db_session.add(token0_in_db)
                db_session.flush()
            if token1_in_db is None:
                token1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token1,
                )
                db_session.add(token1_in_db)
                db_session.flush()

            pool_address, _ = abi_decode(
                types=["address", "uint256"],
                data=new_pool_event["data"],
            )

            (fee,) = raw_call(
                w3=w3,
                address=get_checksum_address(exchange.factory),
                calldata=encode_function_calldata(
                    function_prototype="getFee(address,bool)",
                    function_arguments=[pool_address, stable],
                ),
                return_types=["uint256"],
            )

            db_session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=get_checksum_address(pool_address),
                    chain=w3.eth.chain_id,
                    stable=stable,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=fee,
                    fee_token1=fee,
                    fee_denominator=10_000,
                )
            )


def base_aerodrome_v3_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Aerodrome V3 liquidity pools deployed on Base mainnet and add their metadata to the
    DB.
    """

    database_type = AerodromeV3PoolTable

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=AERODROME_V3_POOLCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)

            (tick_spacing,) = abi_decode(["int24"], new_pool_event["topics"][3])

            (pool_address,) = abi_decode(types=["address"], data=new_pool_event["data"])
            pool_address = get_checksum_address(pool_address)

            token0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token0,
                )
            )
            token1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token1,
                )
            )

            if token0_in_db is None:
                token0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token0,
                )
                db_session.add(token0_in_db)
                db_session.flush()
            if token1_in_db is None:
                token1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token1,
                )
                db_session.add(token1_in_db)
                db_session.flush()

            (fee,) = raw_call(
                w3=w3,
                address=get_checksum_address(exchange.factory),
                calldata=encode_function_calldata(
                    function_prototype="getSwapFee(address)",
                    function_arguments=[pool_address],
                ),
                return_types=["uint24"],
            )

            db_session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=pool_address,
                    chain=w3.eth.chain_id,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=fee,
                    fee_token1=fee,
                    fee_denominator=1_000_000,
                    tick_spacing=tick_spacing,
                )
            )


def base_pancakeswap_v2_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Pancakeswap V2 liquidity pools deployed on Base mainnet and add their metadata to the
    DB.
    """

    database_type = PancakeswapV2PoolTable

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=PANCAKESWAP_V2_PAIRCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)

            token0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token0,
                )
            )
            token1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token1,
                )
            )

            if token0_in_db is None:
                token0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token0,
                )
                db_session.add(token0_in_db)
                db_session.flush()
            if token1_in_db is None:
                token1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token1,
                )
                db_session.add(token1_in_db)
                db_session.flush()

            pool_address, _ = abi_decode(
                types=["address", "uint256"],
                data=new_pool_event["data"],
            )

            db_session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=get_checksum_address(pool_address),
                    chain=w3.eth.chain_id,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=25,
                    fee_token1=25,
                    fee_denominator=10000,
                )
            )


def base_pancakeswap_v3_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Pancakeswap V3 liquidity pools deployed on Base mainnet and add their metadata to the
    DB.
    """

    database_type = PancakeswapV3PoolTable

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=PANCAKESWAP_V3_POOLCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)

            (fee,) = abi_decode(["uint24"], new_pool_event["topics"][3])

            token0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token0,
                )
            )
            token1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token1,
                )
            )

            if token0_in_db is None:
                token0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token0,
                )
                db_session.add(token0_in_db)
                db_session.flush()
            if token1_in_db is None:
                token1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token1,
                )
                db_session.add(token1_in_db)
                db_session.flush()

            tick_spacing, pool_address = abi_decode(
                types=["int24", "address"],
                data=new_pool_event["data"],
            )

            db_session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=get_checksum_address(pool_address),
                    chain=exchange.chain_id,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=fee,
                    fee_token1=fee,
                    fee_denominator=1_000_000,
                    tick_spacing=tick_spacing,
                )
            )


def base_sushiswap_v2_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Sushiswap V2 liquidity pools deployed on Base mainnet and add their metadata to the
    DB.
    """

    database_type = SushiswapV2PoolTable

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=SUSHISWAP_V2_PAIRCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)

            token0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token0,
                )
            )
            token1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token1,
                )
            )

            if token0_in_db is None:
                token0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token0,
                )
                db_session.add(token0_in_db)
                db_session.flush()
            if token1_in_db is None:
                token1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token1,
                )
                db_session.add(token1_in_db)
                db_session.flush()

            pool_address, _ = abi_decode(
                types=["address", "uint256"],
                data=new_pool_event["data"],
            )

            db_session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=get_checksum_address(pool_address),
                    chain=w3.eth.chain_id,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=3,
                    fee_token1=3,
                    fee_denominator=1000,
                )
            )


def base_sushiswap_v3_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Sushiswap V3 liquidity pools deployed on Base mainnet and add their metadata to the
    DB.
    """

    database_type = SushiswapV3PoolTable

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=SUSHISWAP_V3_POOLCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)

            (fee,) = abi_decode(["uint24"], new_pool_event["topics"][3])

            token0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token0,
                )
            )
            token1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token1,
                )
            )

            if token0_in_db is None:
                token0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token0,
                )
                db_session.add(token0_in_db)
                db_session.flush()
            if token1_in_db is None:
                token1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token1,
                )
                db_session.add(token1_in_db)
                db_session.flush()

            tick_spacing, pool_address = abi_decode(
                types=["int24", "address"],
                data=new_pool_event["data"],
            )

            db_session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=get_checksum_address(pool_address),
                    chain=exchange.chain_id,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=fee,
                    fee_token1=fee,
                    fee_denominator=1_000_000,
                    tick_spacing=tick_spacing,
                )
            )


def base_swapbased_v2_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Swapbased V2 liquidity pools deployed on Base mainnet and add their metadata to the
    DB.
    """

    database_type = SwapbasedV2PoolTable

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=SWAPBASED_V2_PAIRCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)

            token0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token0,
                )
            )
            token1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token1,
                )
            )

            if token0_in_db is None:
                token0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token0,
                )
                db_session.add(token0_in_db)
                db_session.flush()
            if token1_in_db is None:
                token1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token1,
                )
                db_session.add(token1_in_db)
                db_session.flush()

            pool_address, _ = abi_decode(
                types=["address", "uint256"],
                data=new_pool_event["data"],
            )

            db_session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=get_checksum_address(pool_address),
                    chain=w3.eth.chain_id,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=3,
                    fee_token1=3,
                    fee_denominator=1000,
                )
            )


def base_uniswap_v2_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Uniswap V2 liquidity pools deployed on Base mainnet and add their metadata to the DB.
    """

    database_type = UniswapV2PoolTable

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=UNISWAP_V2_PAIRCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)

            token0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token0,
                )
            )
            token1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token1,
                )
            )

            if token0_in_db is None:
                token0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token0,
                )
                db_session.add(token0_in_db)
                db_session.flush()
            if token1_in_db is None:
                token1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token1,
                )
                db_session.add(token1_in_db)
                db_session.flush()

            pool_address, _ = abi_decode(
                types=["address", "uint256"],
                data=new_pool_event["data"],
            )

            db_session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=get_checksum_address(pool_address),
                    chain=w3.eth.chain_id,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=3,
                    fee_token1=3,
                    fee_denominator=1000,
                )
            )


def base_uniswap_v3_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Uniswap V3 liquidity pools deployed on Base mainnet and add their metadata to the
    DB.
    """

    database_type = UniswapV3PoolTable

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=UNISWAP_V3_POOLCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)

            (fee,) = abi_decode(["uint24"], new_pool_event["topics"][3])

            token0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token0,
                )
            )
            token1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token1,
                )
            )

            if token0_in_db is None:
                token0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token0,
                )
                db_session.add(token0_in_db)
                db_session.flush()
            if token1_in_db is None:
                token1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token1,
                )
                db_session.add(token1_in_db)
                db_session.flush()

            tick_spacing, pool_address = abi_decode(
                types=["int24", "address"],
                data=new_pool_event["data"],
            )

            db_session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=get_checksum_address(pool_address),
                    chain=exchange.chain_id,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=fee,
                    fee_token1=fee,
                    fee_denominator=1_000_000,
                    tick_spacing=tick_spacing,
                )
            )


def base_uniswap_v4_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Uniswap V4 liquidity pools deployed on Base mainnet and add their metadata to the
    DB.
    """

    database_type = UniswapV4PoolTable

    manager_in_db = db_session.scalar(
        select(PoolManagerTable).where(PoolManagerTable.address == exchange.factory)
    )
    assert manager_in_db is not None

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=UNISWAP_V4_POOLCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (pool_hash,) = abi_decode(["bytes32"], new_pool_event["topics"][1])
            (currency0,) = abi_decode(["address"], new_pool_event["topics"][2])
            (currency1,) = abi_decode(["address"], new_pool_event["topics"][3])

            pool_hash = HexBytes(pool_hash).to_0x_hex()
            currency0 = get_checksum_address(currency0)
            currency1 = get_checksum_address(currency1)

            currency0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == currency0,
                )
            )
            currency1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == currency1,
                )
            )

            if currency0_in_db is None:
                currency0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=currency0,
                )
                db_session.add(currency0_in_db)
                db_session.flush()
            if currency1_in_db is None:
                currency1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=currency1,
                )
                db_session.add(currency1_in_db)
                db_session.flush()

            fee, tick_spacing, hooks = abi_decode(
                ["uint24", "int24", "address"],
                new_pool_event["data"],
            )
            hooks = get_checksum_address(hooks)

            db_session.add(
                database_type(
                    manager_id=manager_in_db.id,
                    pool_hash=pool_hash,
                    hooks=hooks,
                    currency0_id=currency0_in_db.id,
                    currency1_id=currency1_in_db.id,
                    fee_currency0=fee,
                    fee_currency1=fee,
                    fee_denominator=1_000_000,
                    tick_spacing=tick_spacing,
                )
            )


def ethereum_pancakeswap_v2_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Pancakeswap V2 liquidity pools deployed on Ethereum mainnet and add their metadata to
    the DB.
    """

    database_type = PancakeswapV2PoolTable

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=PANCAKESWAP_V2_PAIRCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)

            token0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token0,
                )
            )
            token1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token1,
                )
            )

            if token0_in_db is None:
                token0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token0,
                )
                db_session.add(token0_in_db)
                db_session.flush()
            if token1_in_db is None:
                token1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token1,
                )
                db_session.add(token1_in_db)
                db_session.flush()

            pool_address, _ = abi_decode(
                types=["address", "uint256"],
                data=new_pool_event["data"],
            )

            db_session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=get_checksum_address(pool_address),
                    chain=w3.eth.chain_id,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=25,
                    fee_token1=25,
                    fee_denominator=10000,
                )
            )


def ethereum_pancakeswap_v3_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Pancakeswap V3 liquidity pools deployed on Ethereum mainnet and add their metadata to
    the DB.
    """

    database_type = PancakeswapV3PoolTable

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=PANCAKESWAP_V3_POOLCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)

            (fee,) = abi_decode(["uint24"], new_pool_event["topics"][3])

            token0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token0,
                )
            )
            token1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token1,
                )
            )

            if token0_in_db is None:
                token0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token0,
                )
                db_session.add(token0_in_db)
                db_session.flush()
            if token1_in_db is None:
                token1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token1,
                )
                db_session.add(token1_in_db)
                db_session.flush()

            tick_spacing, pool_address = abi_decode(
                types=["int24", "address"],
                data=new_pool_event["data"],
            )

            db_session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=get_checksum_address(pool_address),
                    chain=exchange.chain_id,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=fee,
                    fee_token1=fee,
                    fee_denominator=1_000_000,
                    tick_spacing=tick_spacing,
                )
            )


def ethereum_sushiswap_v2_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Sushiswap V2 liquidity pools deployed on Ethereum mainnet and add their metadata to
    the DB.
    """

    database_type = SushiswapV2PoolTable

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=SUSHISWAP_V2_PAIRCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)

            token0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token0,
                )
            )
            token1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token1,
                )
            )

            if token0_in_db is None:
                token0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token0,
                )
                db_session.add(token0_in_db)
                db_session.flush()
            if token1_in_db is None:
                token1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token1,
                )
                db_session.add(token1_in_db)
                db_session.flush()

            pool_address, _ = abi_decode(
                types=["address", "uint256"],
                data=new_pool_event["data"],
            )

            db_session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=get_checksum_address(pool_address),
                    chain=w3.eth.chain_id,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=3,
                    fee_token1=3,
                    fee_denominator=1000,
                )
            )


def ethereum_sushiswap_v3_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Sushiswap V3 liquidity pools deployed on Ethereum mainnet and add their metadata to
    the DB.
    """

    database_type = SushiswapV3PoolTable

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=SUSHISWAP_V3_POOLCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)

            (fee,) = abi_decode(["uint24"], new_pool_event["topics"][3])

            token0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token0,
                )
            )
            token1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token1,
                )
            )

            if token0_in_db is None:
                token0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token0,
                )
                db_session.add(token0_in_db)
                db_session.flush()
            if token1_in_db is None:
                token1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token1,
                )
                db_session.add(token1_in_db)
                db_session.flush()

            tick_spacing, pool_address = abi_decode(
                types=["int24", "address"],
                data=new_pool_event["data"],
            )

            db_session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=get_checksum_address(pool_address),
                    chain=exchange.chain_id,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=fee,
                    fee_token1=fee,
                    fee_denominator=1_000_000,
                    tick_spacing=tick_spacing,
                )
            )


def ethereum_uniswap_v2_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Uniswap V2 liquidity pools deployed on Ethereum mainnet and add their metadata to the
    DB.
    """

    database_type = UniswapV2PoolTable

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=UNISWAP_V2_PAIRCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)

            token0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token0,
                )
            )
            token1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token1,
                )
            )

            if token0_in_db is None:
                token0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token0,
                )
                db_session.add(token0_in_db)
                db_session.flush()
            if token1_in_db is None:
                token1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token1,
                )
                db_session.add(token1_in_db)
                db_session.flush()

            pool_address, _ = abi_decode(
                types=["address", "uint256"],
                data=new_pool_event["data"],
            )

            db_session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=get_checksum_address(pool_address),
                    chain=w3.eth.chain_id,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=3,
                    fee_token1=3,
                    fee_denominator=1000,
                )
            )


def ethereum_uniswap_v3_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Uniswap V3 liquidity pools deployed on Ethereum mainnet and add their metadata to the
    DB.
    """

    database_type = UniswapV3PoolTable

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=UNISWAP_V3_POOLCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
            (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
            token0 = get_checksum_address(token0)
            token1 = get_checksum_address(token1)

            (fee,) = abi_decode(["uint24"], new_pool_event["topics"][3])

            token0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token0,
                )
            )
            token1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == token1,
                )
            )

            if token0_in_db is None:
                token0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token0,
                )
                db_session.add(token0_in_db)
                db_session.flush()
            if token1_in_db is None:
                token1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=token1,
                )
                db_session.add(token1_in_db)
                db_session.flush()

            tick_spacing, pool_address = abi_decode(
                types=["int24", "address"],
                data=new_pool_event["data"],
            )

            db_session.add(
                database_type(
                    exchange_id=exchange.id,
                    address=get_checksum_address(pool_address),
                    chain=exchange.chain_id,
                    token0_id=token0_in_db.id,
                    token1_id=token1_in_db.id,
                    fee_token0=fee,
                    fee_token1=fee,
                    fee_denominator=1_000_000,
                    tick_spacing=tick_spacing,
                )
            )


def ethereum_uniswap_v4_pool_updater(
    w3: Web3,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
) -> None:
    """
    Fetch new Uniswap V4 liquidity pools deployed on Ethereum mainnet and add their metadata to the
    DB.
    """

    database_type = UniswapV4PoolTable

    manager_in_db = db_session.scalar(
        select(PoolManagerTable).where(PoolManagerTable.address == exchange.factory)
    )
    assert manager_in_db is not None

    new_pool_events = get_events_from_contract(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=UNISWAP_V4_POOLCREATED_EVENT_HASH,
    )

    if new_pool_events:
        for new_pool_event in tqdm.tqdm(
            new_pool_events,
            desc="Adding new pools",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            (pool_hash,) = abi_decode(["bytes32"], new_pool_event["topics"][1])
            (currency0,) = abi_decode(["address"], new_pool_event["topics"][2])
            (currency1,) = abi_decode(["address"], new_pool_event["topics"][3])

            pool_hash = HexBytes(pool_hash).to_0x_hex()
            currency0 = get_checksum_address(currency0)
            currency1 = get_checksum_address(currency1)

            currency0_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == currency0,
                )
            )
            currency1_in_db = db_session.scalar(
                select(Erc20TokenTable).where(
                    Erc20TokenTable.chain == exchange.chain_id,
                    Erc20TokenTable.address == currency1,
                )
            )

            if currency0_in_db is None:
                currency0_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=currency0,
                )
                db_session.add(currency0_in_db)
                db_session.flush()
            if currency1_in_db is None:
                currency1_in_db = Erc20TokenTable(
                    chain=exchange.chain_id,
                    address=currency1,
                )
                db_session.add(currency1_in_db)
                db_session.flush()

            fee, tick_spacing, hooks = abi_decode(
                ["uint24", "int24", "address"],
                new_pool_event["data"],
            )
            hooks = get_checksum_address(hooks)

            db_session.add(
                database_type(
                    manager_id=manager_in_db.id,
                    pool_hash=pool_hash,
                    hooks=hooks,
                    currency0_id=currency0_in_db.id,
                    currency1_id=currency1_in_db.id,
                    fee_currency0=fee,
                    fee_currency1=fee,
                    fee_denominator=1_000_000,
                    tick_spacing=tick_spacing,
                )
            )


@cli.group()
def pool() -> None:
    """
    Pool commands
    """


@pool.command("repair")
@click.argument("pool_address")
@click.option(
    "--chain",
    "chain_id",
    help="The chain ID to use if multiple pools are found with the same address.",
)
@click.option(
    "--chunk",
    "chunk_size",
    default=10_000,
    help="The maximum number of blocks to process before committing changes to the database "
    "(default 10,000).",
)
def pool_repair(chunk_size: int, pool_address: str, chain_id: int | None) -> None:
    """
    Repair the pool liquidity map for a single pool.
    """

    pool_address = get_checksum_address(pool_address)

    if chain_id is None:
        pool = db_session.scalar(
            select(LiquidityPoolTable).where(LiquidityPoolTable.address == pool_address)
        )
        assert pool is not None
        chain_id = pool.chain
    else:
        pool = db_session.scalar(
            select(LiquidityPoolTable).where(
                LiquidityPoolTable.address == pool_address, LiquidityPoolTable.chain == chain_id
            )
        )

    assert isinstance(pool, AbstractUniswapV3Pool)
    assert pool.exchange.last_update_block is not None

    if chain_id not in connection_manager.connections:
        match endpoint := settings.rpc.get(chain_id):
            case HttpUrl():
                w3 = Web3(HTTPProvider(str(endpoint)))
            case WebsocketUrl():
                w3 = Web3(LegacyWebSocketProvider(str(endpoint)))
            case Path():
                w3 = Web3(IPCProvider(str(endpoint)))
            case None:
                msg = (
                    f"Chain ID {chain_id} does not have an RPC defined in config file {CONFIG_FILE}"
                )
                raise ValueError(msg)

        connection_manager.register_web3(w3)

    working_start_block = initial_start_block = 0

    block_pbar = tqdm.tqdm(
        desc="Processing new blocks",
        total=pool.exchange.last_update_block - initial_start_block + 1,
        bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
        leave=False,
    )

    block_pbar.n = working_start_block - initial_start_block
    block_pbar.refresh()

    exchanges_to_update: set[ExchangeTable] = {pool.exchange}

    # Clear the mapping and reset the pool update timestamps
    pool.liquidity_update_block = None
    pool.liquidity_update_log_index = None
    for position in pool.liquidity_positions:
        db_session.delete(position)
    for map_ in pool.initialization_maps:
        db_session.delete(map_)
    db_session.flush()

    while True:
        # Cap the working end block at the lowest of:
        # - the end of the working chunk size
        # - the last update block for the exchange
        working_end_block = min(
            working_start_block + chunk_size - 1,
            pool.exchange.last_update_block,
        )
        assert working_end_block >= working_start_block

        for _address, _events in tqdm.tqdm(
            get_v3_liquidity_events(
                w3=w3,
                start_block=working_start_block,
                end_block=working_end_block,
                address=pool_address,
            ).items(),
            desc="Updating V3 pool liquidity",
            bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
            leave=False,
        ):
            assert _address == pool_address
            apply_v3_liquidity_updates(
                w3=w3,
                pool_address=_address,
                liquidity_events=_events,
                exchanges_in_scope=exchanges_to_update,
            )

        db_session.commit()

        if working_end_block == pool.exchange.last_update_block:
            break
        working_start_block = working_end_block + 1

        block_pbar.n = working_end_block - initial_start_block
        block_pbar.refresh()

    block_pbar.close()


@pool.command("update")
@click.option(
    "--chunk",
    "chunk_size",
    default=10_000,
    help="The maximum number of blocks to process before committing changes to the database "
    "(default 10,000).",
)
@click.option(
    "--to-block",
    "to_block",
    metavar="INTEGER | TEXT",
    default="finalized",
    help=(
        "The last block included in the update range. Can be a number or an identifier: "
        "'earliest', 'finalized' (default), 'safe', 'latest', 'pending'"
    ),
)
def pool_update(chunk_size: int, to_block: BlockParams) -> None:
    """
    Update liquidity pool information for activated exchanges.
    """

    active_chains = set(
        db_session.scalars(select(ExchangeTable.chain_id).where(ExchangeTable.active)).all()
    )

    for chain_id in active_chains:
        match endpoint := settings.rpc.get(chain_id):
            case HttpUrl():
                w3 = Web3(HTTPProvider(str(endpoint)))
            case WebsocketUrl():
                w3 = Web3(LegacyWebSocketProvider(str(endpoint)))
            case Path():
                w3 = Web3(IPCProvider(str(endpoint)))
            case None:
                msg = (
                    f"Chain ID {chain_id} does not have an RPC defined in config file {CONFIG_FILE}"
                )
                raise ValueError(msg)

        if w3.eth.chain_id != chain_id:
            msg = (
                f"The chain ID ({w3.eth.chain_id}) at endpoint {endpoint} does not match "
                f"the chain ID ({chain_id}) defined in the config file."
            )
            raise ValueError(msg)

        active_exchanges = db_session.scalars(
            select(ExchangeTable).where(
                ExchangeTable.active,
                ExchangeTable.chain_id == chain_id,
            )
        ).all()

        initial_start_block = working_start_block = min(
            [
                0 if exchange.last_update_block is None else exchange.last_update_block + 1
                for exchange in active_exchanges
            ]
        )

        last_block = w3.eth.get_block(
            block_identifier=(
                int(to_block)
                if to_block.isdigit()
                else get_number_for_block_identifier(identifier=to_block, w3=w3)
            )
        )["number"]

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
            # Cap the working end block at the lowest of:
            # - the safe block for the chain
            # - the end of the working chunk size
            # - all update blocks for active exchanges
            working_end_block = min(
                [last_block]
                + [working_start_block + chunk_size - 1]
                + [
                    exchange.last_update_block
                    for exchange in active_exchanges
                    if exchange.last_update_block is not None
                    if exchange.last_update_block > working_start_block
                ],
            )
            assert working_end_block >= working_start_block

            exchanges_to_update = {
                exchange
                for exchange in active_exchanges
                if (
                    exchange.last_update_block is None
                    or exchange.last_update_block + 1 == working_start_block
                )
            }

            for exchange in exchanges_to_update:
                pool_updater = POOL_UPDATER[chain_id, exchange.name]
                pool_updater(w3, working_start_block, working_end_block, exchange)

            # Fetch and process V3 liquidity events
            if any("_v3" in exchange.name for exchange in exchanges_to_update):
                for pool_address, liquidity_events in tqdm.tqdm(
                    get_v3_liquidity_events(
                        w3=w3,
                        start_block=working_start_block,
                        end_block=working_end_block,
                    ).items(),
                    desc="Updating V3 pool liquidity",
                    bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
                    leave=False,
                ):
                    # V3 events are emitted by individual pools, which cannot efficiently be
                    # filtered by eth_getLogs to only include in-scope exchanges — some exchanges
                    # may have millions of deployed pools, which quickly scales to violate most
                    # JSON-RPC query limits.
                    # Nevertheless filtering is required to avoid double-applying events during
                    # backfills, so the updater function looks up the exchange for each pool,
                    # checks if it is included in the in-scope set, and returns early if not.
                    apply_v3_liquidity_updates(
                        w3=w3,
                        pool_address=pool_address,
                        liquidity_events=liquidity_events,
                        exchanges_in_scope=exchanges_to_update,
                    )

            # Fetch and process V4 liquidity events
            for v4_exchange in (
                exchange for exchange in exchanges_to_update if "_v4" in exchange.name
            ):
                pool_manager_in_db = db_session.scalar(
                    select(PoolManagerTable).where(
                        PoolManagerTable.address == v4_exchange.factory,
                        PoolManagerTable.chain == chain_id,
                    )
                )
                assert pool_manager_in_db is not None
                pool_manager_address = get_checksum_address(pool_manager_in_db.address)

                for pool_id, liquidity_events in tqdm.tqdm(
                    get_v4_liquidity_events(
                        w3=w3,
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
                    )

            # At this point, all exchanges have been updated and the invariant checks have passed,
            # so stamp the update block and commit to the DB
            for exchange in exchanges_to_update:
                exchange.last_update_block = working_end_block
            exchanges_to_update.clear()
            db_session.commit()

            if working_end_block == last_block:
                break
            working_start_block = working_end_block + 1

            block_pbar.n = working_end_block - initial_start_block
            block_pbar.refresh()

        block_pbar.close()


def get_events_from_contract(
    w3: Web3,
    start_block: int,
    end_block: int,
    address: ChecksumAddress,
    event_hash: HexBytes,
) -> list[LogReceipt]:
    return fetch_logs_retrying(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=[address],
        topic_signature=[event_hash],
    )


def get_v3_liquidity_events(
    w3: Web3,
    start_block: int,
    end_block: int,
    address: ChecksumAddress | None = None,
) -> dict[ChecksumAddress, deque[LogReceipt]]:
    """
    Fetch new Mint & Burn events for the given range.
    """

    pool_updates: dict[ChecksumAddress, deque[LogReceipt]] = defaultdict(deque)

    for liquidity_event in fetch_logs_retrying(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=None if address is None else [address],
        topic_signature=[
            # matches topic0 on `Mint` OR `Burn`
            [UNISWAP_V3_MINT_EVENT_HASH, UNISWAP_V3_BURN_EVENT_HASH],
        ],
    ):
        pool_updates[liquidity_event["address"]].append(liquidity_event)

    return pool_updates


def get_v4_liquidity_events(
    w3: Web3,
    start_block: int,
    end_block: int,
    address: ChecksumAddress | None = None,
) -> dict[HexBytes, deque[LogReceipt]]:
    """
    Fetch new ModifyLiquidity events for the given range.
    """

    pool_updates: dict[HexBytes, deque[LogReceipt]] = defaultdict(deque)

    for liquidity_event in fetch_logs_retrying(
        w3=w3,
        start_block=start_block,
        end_block=end_block,
        address=None if address is None else [address],
        topic_signature=[
            # matches topic0 on `ModifyLiquidity`
            [UNISWAP_V4_MODIFYLIQUIDITY_EVENT_HASH],
        ],
    ):
        pool_updates[liquidity_event["topics"][1]].append(liquidity_event)

    return pool_updates


POOL_UPDATER: dict[tuple[ChainId, str], Callable[[Web3, int, int, ExchangeTable], None]] = {
    (eth_typing.ChainId.BASE, "aerodrome_v2"): base_aerodrome_v2_pool_updater,
    (eth_typing.ChainId.BASE, "aerodrome_v3"): base_aerodrome_v3_pool_updater,
    (eth_typing.ChainId.BASE, "pancakeswap_v2"): base_pancakeswap_v2_pool_updater,
    (eth_typing.ChainId.BASE, "pancakeswap_v3"): base_pancakeswap_v3_pool_updater,
    (eth_typing.ChainId.BASE, "sushiswap_v2"): base_sushiswap_v2_pool_updater,
    (eth_typing.ChainId.BASE, "sushiswap_v3"): base_sushiswap_v3_pool_updater,
    (eth_typing.ChainId.BASE, "swapbased_v2"): base_swapbased_v2_pool_updater,
    (eth_typing.ChainId.BASE, "uniswap_v2"): base_uniswap_v2_pool_updater,
    (eth_typing.ChainId.BASE, "uniswap_v3"): base_uniswap_v3_pool_updater,
    (eth_typing.ChainId.BASE, "uniswap_v4"): base_uniswap_v4_pool_updater,
    (eth_typing.ChainId.ETH, "pancakeswap_v2"): ethereum_pancakeswap_v2_pool_updater,
    (eth_typing.ChainId.ETH, "pancakeswap_v3"): ethereum_pancakeswap_v3_pool_updater,
    (eth_typing.ChainId.ETH, "sushiswap_v2"): ethereum_sushiswap_v2_pool_updater,
    (eth_typing.ChainId.ETH, "sushiswap_v3"): ethereum_sushiswap_v3_pool_updater,
    (eth_typing.ChainId.ETH, "uniswap_v2"): ethereum_uniswap_v2_pool_updater,
    (eth_typing.ChainId.ETH, "uniswap_v3"): ethereum_uniswap_v3_pool_updater,
    (eth_typing.ChainId.ETH, "uniswap_v4"): ethereum_uniswap_v4_pool_updater,
}
