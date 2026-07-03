"""Parameterized pool creation event updaters.

Replaces 14 near-identical updater functions with 3 parameterized functions:
- ``update_v2_pools``: V2-style events
  (token0/token1 from topics, pool_address from data)
- ``update_v3_pools``: V3-style events
  (token0/token1/fee from topics, tick_spacing/pool_address from data)
- ``update_v4_pools``: V4-style events
  (pool_hash/currency0/currency1 from topics, fee/tick_spacing/hooks from data)

Each accepts a configuration dataclass that captures the DEX-specific variations
(event hash, fee values, optional RPC calls).

WR7EA6 (split out of QJSCA5): the ``erc20_tokens`` get-or-create escalate +
the polymorphic pool-row insert + the ``ExchangeTable.last_update_block``
stamp are owned by the Rust core (``db_upsert_v2/v3/v4_pools`` +
``db_set_exchange_last_update_block`` seams over ``degenbot-db``'s
``discovery`` substrate). These shells decode the raw ``PoolCreated``
``LogReceipt``s (topics/data → addresses/fee/tick-spacing) + do the RPC fee
lookup, then build row-input lists + delegate — the event-scan driver loop +
RPC event fetch stay Python (``stays-python``).
"""

from collections.abc import Callable
from dataclasses import dataclass, field

import tqdm
from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot import abi_decode
from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.base import ExchangeTable
from degenbot.degenbot_rs import (
    V2PoolRowInput,
    V3PoolRowInput,
    V4PoolRowInput,
    db_upsert_v2_pools,
    db_upsert_v3_pools,
    db_upsert_v4_pools,
)
from degenbot.provider import ProviderAdapter
from degenbot.provider.call_helpers import encode_function_calldata, raw_call


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
    provider: ProviderAdapter,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
    *,
    database_path: str,
    config: V2PoolUpdateConfig,
    get_events_fn: Callable[..., list[LogReceipt]],
) -> None:
    """Process V2-style pool creation events for a DEX.

    WR7EA6: the ``erc20_tokens`` get-or-create + the polymorphic pool-row
    insert are owned by the Rust core (``db_upsert_v2_pools``); this shell
    decodes the raw ``PoolCreated`` events + does the optional RPC fee
    lookup, then builds row-input records + delegates. The event-scan
    driver loop + RPC event fetch stay Python.

    """
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
    provider: ProviderAdapter,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
    *,
    database_path: str,
    config: V3PoolUpdateConfig,
    get_events_fn: Callable[..., list[LogReceipt]],
) -> None:
    """Process V3-style pool creation events for a DEX.

    WR7EA6: the ``erc20_tokens`` get-or-create + the polymorphic pool-row
    insert are owned by the Rust core (``db_upsert_v3_pools``); this shell
    decodes the raw ``PoolCreated`` events + does the optional RPC fee
    override lookup (Aerodrome), then builds row-input records + delegates.

    """
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
    provider: ProviderAdapter,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
    *,
    database_path: str,
    config: V4PoolUpdateConfig,
    get_events_fn: Callable[..., list[LogReceipt]],
) -> None:
    """Process V4-style pool creation events for a DEX.

    WR7EA6: the ``PoolManagerTable`` lookup (by ``exchange.factory``) + the
    ``erc20_tokens`` get-or-create + the ``managed_pools`` / ``uniswap_v4_pools``
    insert are owned by the Rust core (``db_upsert_v4_pools``); this shell
    decodes the raw ``PoolCreated`` events, then builds row-input records +
    delegates.

    """
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

        pool_hash = HexBytes(pool_hash).to_0x_hex()
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
