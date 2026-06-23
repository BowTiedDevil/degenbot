"""Shared utility functions for Aave V3 CLI processing.

This module contains helper functions that are used across multiple
Aave CLI modules.
"""

import operator

from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from sqlalchemy import select
from sqlalchemy.orm import Session, joinedload
from web3.types import LogReceipt

from degenbot.aave.events import ERC20Event
from degenbot.cli.aave.types import TransactionContext
from degenbot.constants import ZERO_ADDRESS
from degenbot.contract.decoding import decode_address
from degenbot.database.models.aave import AaveGhoToken, AaveV3Asset, AaveV3Contract, AaveV3Market
from degenbot.logging import logger
from degenbot.provider.sync_adapter import ProviderAdapter


def _get_v_token_for_underlying(
    session: Session,
    market: AaveV3Market,
    underlying_address: ChecksumAddress,
) -> ChecksumAddress | None:
    """Get vToken address for an underlying asset.

    Returns:
        The computed value.

    """
    asset = session.scalar(
        select(AaveV3Asset).where(
            AaveV3Asset.market_id == market.id,
            AaveV3Asset.underlying_token.has(address=underlying_address),
        ),
    )
    assert asset is not None
    assert asset.v_token is not None
    return asset.v_token.address


def _get_all_scaled_token_addresses(
    session: Session,
    chain_id: int,
) -> list[ChecksumAddress]:
    """Get all aToken and vToken addresses for a given chain.

    Returns:
        A list of results.

    """
    # Query from AaveV3Asset (small table, ~7 rows) instead of Erc20TokenTable
    # (572K+ rows) to avoid expensive full table scans.
    assets = (
        session
        .scalars(
            select(AaveV3Asset)
            .join(AaveV3Market, AaveV3Asset.market_id == AaveV3Market.id)
            .where(AaveV3Market.chain_id == chain_id)
            .options(
                joinedload(AaveV3Asset.a_token),
                joinedload(AaveV3Asset.v_token),
            ),
        )
        .unique()
        .all()
    )

    addresses: list[ChecksumAddress] = []
    for asset in assets:
        if asset.a_token is not None:
            addresses.append(asset.a_token.address)
        if asset.v_token is not None:
            addresses.append(asset.v_token.address)
    return addresses


def _build_transaction_contexts(
    *,
    events: list[LogReceipt],
    market: AaveV3Market,
    session: Session,
    provider: ProviderAdapter,
    gho_asset: AaveGhoToken,
    pool_contract: AaveV3Contract,
) -> dict[HexBytes, TransactionContext]:
    """Group events by transaction with full categorization.

    Returns:
        The computed value.

    """
    assert pool_contract.revision is not None

    contexts: dict[HexBytes, TransactionContext] = {}

    for event in sorted(events, key=operator.itemgetter("blockNumber", "logIndex")):
        tx_hash = event["transactionHash"]
        block_num = event["blockNumber"]
        topic = event["topics"][0]
        event_address = event["address"]

        logger.debug(
            f"_build_transaction_contexts: processing event "
            f"block={block_num} tx={tx_hash.to_0x_hex()} "
            f"topic={topic.to_0x_hex()} addr={event_address}",
        )

        if tx_hash not in contexts:
            logger.debug(
                f"_build_transaction_contexts: creating new context for tx={tx_hash.to_0x_hex()}",
            )
            contexts[tx_hash] = TransactionContext(
                provider=provider,
                tx_hash=tx_hash,
                block_number=block_num,
                events=[],
                market=market,
                session=session,
                gho_asset=gho_asset,
                pool_revision=pool_contract.revision,
            )

        ctx = contexts[tx_hash]
        ctx.events.append(event)

        # Track users involved in stkAAVE transfers (needed for discount calculations)
        if topic == ERC20Event.TRANSFER.value and event_address == (
            gho_asset.v_gho_discount_token if gho_asset else None
        ):
            from_addr = decode_address(event["topics"][1])
            to_addr = decode_address(event["topics"][2])
            if from_addr != ZERO_ADDRESS:
                ctx.stk_aave_transfer_users.add(from_addr)
            if to_addr != ZERO_ADDRESS:
                ctx.stk_aave_transfer_users.add(to_addr)

    return contexts
