"""Parameterized pool creation event updaters."""

from collections.abc import Callable
from dataclasses import dataclass, field

from degenbot import abi_decode
from degenbot.checksum_cache import get_checksum_address
from degenbot.db import (
    ExchangeRow,
    db_upsert_v2_pools,
    db_upsert_v3_pools,
    db_upsert_v4_pools,
)
from degenbot.provider import AlloyProvider
from degenbot.provider.call_helpers import encode_function_calldata, raw_call
from degenbot.types.rpc_types import LogReceipt
from degenbot.updater import V2PoolRowInput, V3PoolRowInput, V4PoolRowInput
from degenbot.utils.bytes import to_0x_hex

# V3 liquidity event topic0 hashes (Mint/Burn): the event vocabulary a V3 pool
# update filter selects on.
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


@dataclass(frozen=True)
class PoolUpdateRequest[ConfigT: V2PoolUpdateConfig | V3PoolUpdateConfig | V4PoolUpdateConfig]:
    """Typed request for one pool-creation event update run.

    Bundles the block bounds, Rust-backed exchange row, database path,
    decode config, and event-fetch callable so each updater shell takes a
    single argument.
    """

    provider: AlloyProvider
    start_block: int
    end_block: int
    exchange: ExchangeRow
    database_path: str
    config: ConfigT
    get_events_fn: Callable[..., list[LogReceipt]]


def update_v2_pools(request: PoolUpdateRequest[V2PoolUpdateConfig]) -> None:
    """Process V2-style pool creation events for a DEX."""
    new_pool_events = request.get_events_fn(
        provider=request.provider,
        start_block=request.start_block,
        end_block=request.end_block,
        address=get_checksum_address(request.exchange.factory),
        event_hash=request.config.event_hash,
    )

    if not new_pool_events:
        return

    rows: list[V2PoolRowInput] = []
    for new_pool_event in new_pool_events:
        (token0,) = abi_decode(["address"], new_pool_event["topics"][1])
        (token1,) = abi_decode(["address"], new_pool_event["topics"][2])
        token0 = get_checksum_address(token0)
        token1 = get_checksum_address(token1)

        stable = False
        if request.config.has_stable_flag:
            (stable,) = abi_decode(["bool"], new_pool_event["topics"][3])

        pool_address, _ = abi_decode(
            types=["address", "uint256"],
            data=new_pool_event["data"],
        )
        pool_address = get_checksum_address(pool_address)

        # Determine fee: either from RPC call or constant
        if request.config.rpc_fee_call is not None:
            rpc_args = (
                [pool_address, stable] if request.config.rpc_fee_includes_stable else [pool_address]
            )
            (fee,) = raw_call(
                provider=request.provider,
                address=get_checksum_address(request.exchange.factory),
                calldata=encode_function_calldata(
                    function_prototype=request.config.rpc_fee_call,
                    function_arguments=rpc_args,
                ),
                return_types=request.config.rpc_fee_return_types,
            )
            fee_token0 = fee
            fee_token1 = fee
        else:
            fee_token0 = request.config.fee_token0
            fee_token1 = request.config.fee_token1

        rows.append(
            V2PoolRowInput(
                address=pool_address,
                token0_address=token0,
                token1_address=token1,
                fee_token0=fee_token0,
                fee_token1=fee_token1,
                stable=stable if request.config.has_stable_flag else None,
            ),
        )

    db_upsert_v2_pools(
        database_path=request.database_path,
        chain_id=request.exchange.chain_id,
        kind=request.exchange.name,
        exchange_id=request.exchange.id,
        fee_denominator=request.config.fee_denominator,
        rows=rows,
    )


def update_v3_pools(request: PoolUpdateRequest[V3PoolUpdateConfig]) -> None:
    """Process V3-style pool creation events for a DEX."""
    new_pool_events = request.get_events_fn(
        provider=request.provider,
        start_block=request.start_block,
        end_block=request.end_block,
        address=get_checksum_address(request.exchange.factory),
        event_hash=request.config.event_hash,
    )

    if not new_pool_events:
        return

    rows: list[V3PoolRowInput] = []
    for new_pool_event in new_pool_events:
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
        if request.config.rpc_fee_call is not None:
            (fee,) = raw_call(
                provider=request.provider,
                address=get_checksum_address(request.exchange.factory),
                calldata=encode_function_calldata(
                    function_prototype=request.config.rpc_fee_call,
                    function_arguments=[pool_address],
                ),
                return_types=request.config.rpc_fee_return_types,
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
        database_path=request.database_path,
        chain_id=request.exchange.chain_id,
        kind=request.exchange.name,
        exchange_id=request.exchange.id,
        fee_denominator=request.config.fee_denominator,
        rows=rows,
    )


def update_v4_pools(request: PoolUpdateRequest[V4PoolUpdateConfig]) -> None:
    """Process V4-style pool creation events for a DEX."""
    new_pool_events = request.get_events_fn(
        provider=request.provider,
        start_block=request.start_block,
        end_block=request.end_block,
        address=get_checksum_address(request.exchange.factory),
        event_hash=request.config.event_hash,
    )

    if not new_pool_events:
        return

    rows: list[V4PoolRowInput] = []
    for new_pool_event in new_pool_events:
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
        database_path=request.database_path,
        chain_id=request.exchange.chain_id,
        pool_manager_address=get_checksum_address(request.exchange.factory),
        fee_denominator=request.config.fee_denominator,
        rows=rows,
    )
