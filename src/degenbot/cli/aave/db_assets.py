"""
Asset and token database operations for Aave V3.

Functions for managing ERC20 tokens, Aave assets, contracts, and related lookups.
"""

from typing import assert_never

from eth_typing import ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session, joinedload

from degenbot.cli.aave.erc20_utils import _fetch_erc20_token_metadata
from degenbot.cli.aave_types import TokenType
from degenbot.database.models.aave import AaveGhoToken, AaveV3Asset, AaveV3Contract, AaveV3Market
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.logging import logger
from degenbot.provider.interface import ProviderAdapter


def get_or_create_erc20_token(
    provider: ProviderAdapter,
    session: Session,
    chain_id: int,
    token_address: ChecksumAddress,
) -> Erc20TokenTable:
    """
    Get existing ERC20 token or create new one.

    When creating a new token, attempts to fetch name, symbol, and decimals
    from the blockchain and populate the database record.
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

        # Attempt to fetch metadata from blockchain
        name, symbol, decimals = _fetch_erc20_token_metadata(
            provider=provider,
            token_address=token_address,
        )

        if name is not None:
            token.name = name
        if symbol is not None:
            token.symbol = symbol
        if decimals is not None:
            token.decimals = decimals

        session.add(token)
        session.flush()

        if name is not None or symbol is not None or decimals is not None:
            logger.debug(
                f"Created ERC20 token {token_address} with metadata: "
                f"name='{name}', symbol='{symbol}', decimals={decimals}"
            )

    return token


def get_gho_asset(
    session: Session,
    market: AaveV3Market,
) -> AaveGhoToken:
    """
    Get GHO token asset for a given market.
    """

    gho_asset = session.scalar(
        select(AaveGhoToken)
        .join(AaveGhoToken.token)
        .where(Erc20TokenTable.chain == market.chain_id)
    )
    assert gho_asset is not None
    return gho_asset


def get_contract(
    session: Session,
    market: AaveV3Market,
    contract_name: str,
) -> AaveV3Contract | None:
    """
    Get contract by name for a given market.
    """

    return session.scalar(
        select(AaveV3Contract).where(
            AaveV3Contract.market_id == market.id,
            AaveV3Contract.name == contract_name,
        )
    )


def get_asset_by_token_type(
    session: Session,
    market: AaveV3Market,
    token_address: ChecksumAddress,
    token_type: TokenType,
) -> AaveV3Asset | None:
    """
    Get AaveV3 asset by aToken (collateral) or vToken (debt) address.
    """

    match token_type:
        case TokenType.A_TOKEN:
            return session.scalar(
                select(AaveV3Asset)
                .join(Erc20TokenTable, AaveV3Asset.a_token_id == Erc20TokenTable.id)
                .where(
                    AaveV3Asset.market_id == market.id,
                    Erc20TokenTable.address == token_address,
                )
                .options(joinedload(AaveV3Asset.a_token))
            )
        case TokenType.V_TOKEN:
            return session.scalar(
                select(AaveV3Asset)
                .join(Erc20TokenTable, AaveV3Asset.v_token_id == Erc20TokenTable.id)
                .where(
                    AaveV3Asset.market_id == market.id,
                    Erc20TokenTable.address == token_address,
                )
                .options(joinedload(AaveV3Asset.v_token))
            )
        case _ as unreachable:
            assert_never(unreachable)


def get_asset_identifier(asset: AaveV3Asset) -> str:
    """
    Get a human-readable identifier for an asset.

    This provides consistent asset identification in debug logs and error messages.
    """

    return asset.underlying_token.symbol or asset.underlying_token.address
