"""
Shared utility functions for Aave V3 CLI processing.

This module contains helper functions that are used across multiple
Aave CLI modules.
"""

import operator
from typing import assert_never

import eth_abi.exceptions
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from sqlalchemy import select
from sqlalchemy.orm import Session
from web3.exceptions import ContractLogicError
from web3.types import LogReceipt

from degenbot.aave.events import ERC20Event
from degenbot.checksum_cache import get_checksum_address
from degenbot.cli.aave.constants import AAVE_EVENT_TOPIC_TO_CATEGORY
from degenbot.cli.aave.types import TransactionContext
from degenbot.cli.aave_utils import decode_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.database.models.aave import (
    AaveGhoToken,
    AaveV3Asset,
    AaveV3Contract,
    AaveV3Market,
)
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger
from degenbot.provider.interface import ProviderAdapter


def _get_v_token_for_underlying(
    session: Session,
    market: AaveV3Market,
    underlying_address: ChecksumAddress,
) -> ChecksumAddress | None:
    """Get vToken address for an underlying asset."""
    asset = session.scalar(
        select(AaveV3Asset).where(
            AaveV3Asset.market_id == market.id,
            AaveV3Asset.underlying_token.has(address=underlying_address),
        )
    )
    assert asset is not None
    assert asset.v_token is not None
    return asset.v_token.address


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
                AaveV3Asset,
                AaveV3Asset.a_token_id == Erc20TokenTable.id,
            )
            .where(Erc20TokenTable.chain == chain_id)
        ).all()
    )

    v_token_addresses = list(
        session.scalars(
            select(Erc20TokenTable.address)
            .join(
                AaveV3Asset,
                AaveV3Asset.v_token_id == Erc20TokenTable.id,
            )
            .where(Erc20TokenTable.chain == chain_id)
        ).all()
    )

    return a_token_addresses + v_token_addresses


def _log_event_categorization(
    *,
    topic: HexBytes,
    event_address: ChecksumAddress,
    gho_asset: AaveGhoToken,
) -> None:
    """
    Validate event topic is recognized and log its categorization.

    Raises:
        ValueError: If the topic is not recognized.
    """

    # Check module-level cache for Aave events
    if topic in AAVE_EVENT_TOPIC_TO_CATEGORY:
        category = AAVE_EVENT_TOPIC_TO_CATEGORY[topic]
    elif topic == ERC20Event.TRANSFER.value:
        if event_address == (gho_asset.v_gho_discount_token if gho_asset else None):
            category = "stkAAVE_TRANSFER"
        else:
            category = "ERC20_TRANSFER"
    else:
        assert_never(topic)

    logger.debug(f"_build_transaction_contexts: categorized as {category} event")


def _build_transaction_contexts(
    *,
    events: list[LogReceipt],
    market: AaveV3Market,
    session: Session,
    provider: ProviderAdapter,
    gho_asset: AaveGhoToken,
    pool_contract: AaveV3Contract,
) -> dict[HexBytes, TransactionContext]:
    """
    Group events by transaction with full categorization.
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
            f"topic={topic.to_0x_hex()} addr={event_address}"
        )

        if tx_hash not in contexts:
            logger.debug(
                f"_build_transaction_contexts: creating new context for tx={tx_hash.to_0x_hex()}"
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

        # Validate and log event categorization
        _log_event_categorization(
            topic=topic,
            event_address=event_address,
            gho_asset=gho_asset,
        )

    return contexts


def _fetch_discount_token_from_contract(
    provider: ProviderAdapter,
    gho_asset: AaveGhoToken,
    block_number: int,
) -> ChecksumAddress | None:
    """
    Fetch the discount token address from the GHO vToken contract.

    This is used to initialize v_gho_discount_token when it's not set in the database
    and no DISCOUNT_TOKEN_UPDATED events exist in the current block range.
    """

    # vToken not deployed yet
    if gho_asset.v_token is None:
        return None

    try:
        # GHO vToken has a getDiscountToken() function
        discount_token: str
        (discount_token,) = raw_call(
            provider=provider,
            address=gho_asset.v_token.address,
            calldata=encode_function_calldata(
                function_prototype="getDiscountToken()",
                function_arguments=[],
            ),
            return_types=["address"],
            block_identifier=block_number,
        )
        return get_checksum_address(discount_token)
    except (
        ValueError,
        RuntimeError,
        eth_abi.exceptions.DecodingError,
        ContractLogicError,
    ):
        # Function may not exist in older revisions or other errors
        return None
