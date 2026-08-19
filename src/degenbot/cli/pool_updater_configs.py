"""Parameterized pool creation event updaters."""

from collections.abc import Callable
from dataclasses import dataclass, field

import tqdm

from degenbot import abi_decode
from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.base import ExchangeTable
from degenbot.database.models.pools import PoolManagerTable
from degenbot.db import (
    db_apply_v3_liquidity_updates,
    db_apply_v4_liquidity_updates,
    db_fetch_pool_row,
    db_upsert_v2_pools,
    db_upsert_v3_pools,
    db_upsert_v4_pools,
)
from degenbot.provider import AlloyProvider
from degenbot.provider.call_helpers import encode_function_calldata, raw_call
from degenbot.types.rpc_types import LogReceipt
from degenbot.updater import (
    LiquidityUpdateEvent,
    V2PoolRowInput,
    V3PoolRowInput,
    V4PoolRowInput,
)
from degenbot.utils.bytes import to_0x_hex

# V3 liquidity event topic0 hashes (Mint/Burn). Used by the V3 decode shell
# (`apply_v3_liquidity_updates`) to recognize the Burn signature + negate the
# delta. Kept here (not in `cli/pool.py`) so the chunk loop's removal in
# task JJ232N leaves these decode shells with their topic constants.
UNISWAP_V3_MINT_EVENT_HASH = bytes.fromhex(
    "7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde",
)
UNISWAP_V3_BURN_EVENT_HASH = bytes.fromhex(
    "0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c",
)


@dataclass(frozen=True)
class V2PoolUpdateConfig:
    """Configuration for a V2-style pool creation event updater.

    V2 PoolCreated events have:
    - topics[1]: token0 address
    - topics[2]: token1 address
    - topics[3]: (optional) stable bool (Aerodrome)
    - data: ABI-encoded (pool_address, ?)
    """

    name: str
    event_hash: bytes
    fee_token0: int
    fee_token1: int
    fee_denominator: int
    # If set, decode stable bool from topics[3]
    has_stable_flag: bool = False
    # If set, call this RPC method to get fee instead of using constant fee
    rpc_fee_call: str | None = None
    # RPC call return types for fee
    rpc_fee_return_types: list[str] = field(default_factory=lambda: ["uint256"])
    # If rpc_fee_call has stable-dependent behavior (Aerodrome), include it
    rpc_fee_includes_stable: bool = False


@dataclass(frozen=True)
class V3PoolUpdateConfig:
    """Configuration for a V3-style pool creation event updater.

    V3 PoolCreated events have:
    - topics[1]: token0 address
    - topics[2]: token1 address
    - topics[3]: fee (uint24)
    - data: ABI-encoded (tick_spacing, pool_address)
    """

    name: str
    event_hash: bytes
    fee_denominator: int
    # If set, call this RPC method to get fee instead of using topics[3]
    rpc_fee_call: str | None = None
    rpc_fee_return_types: list[str] = field(default_factory=lambda: ["uint24"])


@dataclass(frozen=True)
class V4PoolUpdateConfig:
    """Configuration for a V4-style pool creation event updater.

    V4 PoolCreated events have:
    - topics[1]: pool_hash (bytes32)
    - topics[2]: currency0 address
    - topics[3]: currency1 address
    - data: ABI-encoded (fee, tick_spacing, hooks)
    """

    name: str
    event_hash: bytes
    fee_denominator: int


def update_v2_pools(
    provider: AlloyProvider,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
    *,
    database_path: str,
    config: V2PoolUpdateConfig,
    get_events_fn: Callable[..., list[LogReceipt]],
) -> None:
    """Process V2-style pool creation events for a DEX."""
    new_pool_events = get_events_fn(
        provider=provider,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=config.event_hash,
    )

    if not new_pool_events:
        return

    rows: list[V2PoolRowInput] = []
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

        stable = False
        if config.has_stable_flag:
            (stable,) = abi_decode(["bool"], new_pool_event["topics"][3])

        pool_address, _ = abi_decode(
            types=["address", "uint256"],
            data=new_pool_event["data"],
        )
        pool_address = get_checksum_address(pool_address)

        # Determine fee: either from RPC call or constant
        if config.rpc_fee_call is not None:
            rpc_args = [pool_address, stable] if config.rpc_fee_includes_stable else [pool_address]
            (fee,) = raw_call(
                provider=provider,
                address=get_checksum_address(exchange.factory),
                calldata=encode_function_calldata(
                    function_prototype=config.rpc_fee_call,
                    function_arguments=rpc_args,
                ),
                return_types=config.rpc_fee_return_types,
            )
            fee_token0 = fee
            fee_token1 = fee
        else:
            fee_token0 = config.fee_token0
            fee_token1 = config.fee_token1

        rows.append(
            V2PoolRowInput(
                address=pool_address,
                token0_address=token0,
                token1_address=token1,
                fee_token0=fee_token0,
                fee_token1=fee_token1,
                stable=stable if config.has_stable_flag else None,
            ),
        )

    db_upsert_v2_pools(
        database_path=database_path,
        chain_id=exchange.chain_id,
        kind=exchange.name,
        exchange_id=exchange.id,
        fee_denominator=config.fee_denominator,
        rows=rows,
    )


def update_v3_pools(
    provider: AlloyProvider,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
    *,
    database_path: str,
    config: V3PoolUpdateConfig,
    get_events_fn: Callable[..., list[LogReceipt]],
) -> None:
    """Process V3-style pool creation events for a DEX."""
    new_pool_events = get_events_fn(
        provider=provider,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=config.event_hash,
    )

    if not new_pool_events:
        return

    rows: list[V3PoolRowInput] = []
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

        tick_spacing, pool_address = abi_decode(
            types=["int24", "address"],
            data=new_pool_event["data"],
        )
        pool_address = get_checksum_address(pool_address)

        # Aerodrome V3: override fee from RPC
        if config.rpc_fee_call is not None:
            (fee,) = raw_call(
                provider=provider,
                address=get_checksum_address(exchange.factory),
                calldata=encode_function_calldata(
                    function_prototype=config.rpc_fee_call,
                    function_arguments=[pool_address],
                ),
                return_types=config.rpc_fee_return_types,
            )

        rows.append(
            V3PoolRowInput(
                address=pool_address,
                token0_address=token0,
                token1_address=token1,
                fee=fee,
                tick_spacing=tick_spacing,
            ),
        )

    db_upsert_v3_pools(
        database_path=database_path,
        chain_id=exchange.chain_id,
        kind=exchange.name,
        exchange_id=exchange.id,
        fee_denominator=config.fee_denominator,
        rows=rows,
    )


def update_v4_pools(
    provider: AlloyProvider,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
    *,
    database_path: str,
    config: V4PoolUpdateConfig,
    get_events_fn: Callable[..., list[LogReceipt]],
) -> None:
    """Process V4-style pool creation events for a DEX."""
    new_pool_events = get_events_fn(
        provider=provider,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=config.event_hash,
    )

    if not new_pool_events:
        return

    rows: list[V4PoolRowInput] = []
    for new_pool_event in tqdm.tqdm(
        new_pool_events,
        desc="Adding new pools",
        bar_format="{desc}: {percentage:3.1f}% |{bar}| {n_fmt}/{total_fmt}",
        leave=False,
    ):
        (pool_hash,) = abi_decode(["bytes32"], new_pool_event["topics"][1])
        (currency0,) = abi_decode(["address"], new_pool_event["topics"][2])
        (currency1,) = abi_decode(["address"], new_pool_event["topics"][3])

        pool_hash = to_0x_hex(pool_hash)
        currency0 = get_checksum_address(currency0)
        currency1 = get_checksum_address(currency1)

        fee, tick_spacing, hooks = abi_decode(
            ["uint24", "int24", "address"],
            new_pool_event["data"],
        )
        hooks = get_checksum_address(hooks)

        rows.append(
            V4PoolRowInput(
                pool_hash=pool_hash,
                hooks=hooks,
                currency0_address=currency0,
                currency1_address=currency1,
                fee=fee,
                tick_spacing=tick_spacing,
            ),
        )

    db_upsert_v4_pools(
        database_path=database_path,
        chain_id=exchange.chain_id,
        pool_manager_address=get_checksum_address(exchange.factory),
        fee_denominator=config.fee_denominator,
        rows=rows,
    )


def apply_v3_liquidity_updates(
    provider: AlloyProvider,
    pool_address: str,
    liquidity_events: list[LogReceipt],
    exchanges_in_scope: set[ExchangeTable],
    *,
    database_path: str,
) -> None:
    """Apply the V3 liquidity updates to the provided pool."""
    in_scope_exchange_ids = {exchange.id for exchange in exchanges_in_scope}
    pool_row = db_fetch_pool_row(
        database_path=database_path,
        chain_id=provider.chain_id,
        address=pool_address,
    )
    if pool_row is None or pool_row.exchange_id not in in_scope_exchange_ids:
        return

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

    db_apply_v3_liquidity_updates(
        database_path=database_path,
        chain_id=provider.chain_id,
        pool_address=pool_address,
        events=decoded_events,
    )


def apply_v4_liquidity_updates(
    pool_id: bytes,
    liquidity_events: list[LogReceipt],
    pool_manager: PoolManagerTable,
    *,
    database_path: str,
) -> None:
    """Apply the V4 liquidity updates to the provided pool.

    V4 emits a single signed ``ModifyLiquidity`` event (no Burn/Mint split), so the decode unpacks
    ``(tick_lower, tick_upper, liquidity_delta, _)`` straight from ``data``.

    Assumes the liquidity updates are ordered by block number + log index,
    ascending (the Rust apply enforces the ordering invariant).
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

    db_apply_v4_liquidity_updates(
        database_path=database_path,
        pool_hash_hex=to_0x_hex(pool_id),
        pool_manager_chain=pool_manager.chain,
        events=decoded_events,
    )
