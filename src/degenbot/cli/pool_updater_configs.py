"""Parameterized pool creation event updaters.

Replaces 14 near-identical updater functions with 3 parameterized functions:
- ``update_v2_pools``: V2-style events (token0/token1 from topics, pool_address from data)
- ``update_v3_pools``: V3-style events (token0/token1/fee from topics, tick_spacing/pool_address from data)
- ``update_v4_pools``: V4-style events (pool_hash/currency0/currency1 from topics, fee/tick_spacing/hooks from data)

Each accepts a configuration dataclass that captures the DEX-specific variations
(database table, event hash, fee values, optional RPC calls).
"""

from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

import tqdm
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from sqlalchemy import select
from sqlalchemy.orm import Session
from web3.types import LogReceipt

from degenbot import abi_decode
from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.base import ExchangeTable
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import PoolManagerTable
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
    database_type: type
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
    database_type: type
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
    database_type: type
    event_hash: bytes
    fee_denominator: int


def _get_or_create_token(
    session: Session,
    chain_id: int,
    address: ChecksumAddress,
) -> Erc20TokenTable:
    if (
        token := session.scalar(
            select(Erc20TokenTable).where(
                Erc20TokenTable.chain == chain_id,
                Erc20TokenTable.address == address,
            )
        )
    ) is None:
        token = Erc20TokenTable(chain=chain_id, address=address)
        session.add(token)
        session.flush()

    return token


def update_v2_pools(
    provider: ProviderAdapter,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
    session: Session,
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

        token0_in_db = _get_or_create_token(session, exchange.chain_id, token0)
        token1_in_db = _get_or_create_token(session, exchange.chain_id, token1)

        pool_address, _ = abi_decode(
            types=["address", "uint256"],
            data=new_pool_event["data"],
        )

        # Determine fee: either from RPC call or constant
        if config.rpc_fee_call is not None:
            if config.rpc_fee_includes_stable:
                rpc_args = [pool_address, stable]
            else:
                rpc_args = [pool_address]
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

        record_kwargs: dict[str, Any] = {
            "exchange_id": exchange.id,
            "address": get_checksum_address(pool_address),
            "chain": provider.chain_id,
            "token0_id": token0_in_db.id,
            "token1_id": token1_in_db.id,
            "fee_token0": fee_token0,
            "fee_token1": fee_token1,
            "fee_denominator": config.fee_denominator,
        }

        if config.has_stable_flag:
            record_kwargs["stable"] = stable

        session.add(config.database_type(**record_kwargs))


def update_v3_pools(
    provider: ProviderAdapter,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
    session: Session,
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

        token0_in_db = _get_or_create_token(session, exchange.chain_id, token0)
        token1_in_db = _get_or_create_token(session, exchange.chain_id, token1)

        tick_spacing, pool_address = abi_decode(
            types=["int24", "address"],
            data=new_pool_event["data"],
        )

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

        session.add(
            config.database_type(
                exchange_id=exchange.id,
                address=get_checksum_address(pool_address),
                chain=exchange.chain_id,
                token0_id=token0_in_db.id,
                token1_id=token1_in_db.id,
                fee_token0=fee,
                fee_token1=fee,
                fee_denominator=config.fee_denominator,
                tick_spacing=tick_spacing,
            )
        )


def update_v4_pools(
    provider: ProviderAdapter,
    start_block: int,
    end_block: int,
    exchange: ExchangeTable,
    session: Session,
    config: V4PoolUpdateConfig,
    get_events_fn: Callable[..., list[LogReceipt]],
) -> None:
    """Process V4-style pool creation events for a DEX."""
    manager_in_db = session.scalar(
        select(PoolManagerTable).where(PoolManagerTable.address == exchange.factory)
    )
    assert manager_in_db is not None

    new_pool_events = get_events_fn(
        provider=provider,
        start_block=start_block,
        end_block=end_block,
        address=get_checksum_address(exchange.factory),
        event_hash=config.event_hash,
    )

    if not new_pool_events:
        return

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

        currency0_in_db = _get_or_create_token(session, exchange.chain_id, currency0)
        currency1_in_db = _get_or_create_token(session, exchange.chain_id, currency1)

        fee, tick_spacing, hooks = abi_decode(
            ["uint24", "int24", "address"],
            new_pool_event["data"],
        )
        hooks = get_checksum_address(hooks)

        session.add(
            config.database_type(
                manager_id=manager_in_db.id,
                pool_hash=pool_hash,
                hooks=hooks,
                currency0_id=currency0_in_db.id,
                currency1_id=currency1_in_db.id,
                fee_currency0=fee,
                fee_currency1=fee,
                fee_denominator=config.fee_denominator,
                tick_spacing=tick_spacing,
            )
        )
