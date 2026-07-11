"""Asset and token database operations for Aave V3.

Slimmed by the §4.2 writer retirement (CZM7TI): the per-event write dispatch
(``get_contract``, ``get_gho_asset``, ``get_asset_by_token_type``,
``get_asset_identifier``) is owned by the Rust core
(``degenbot-aave-updater::run::run_aave_update``); those helpers + their ORM
write paths were deleted with the Python writer pipeline. What remains is the
ERC20-token get-or-create used by ``aave activate ethereum_aave_v3`` (the
market-activation path — the last Python ORM writer on the Aave path; port
to Rust tracked by task MPI6Q3).
"""

from eth_typing import ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session

from degenbot.cli.aave.erc20_utils import _fetch_erc20_token_metadata
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.logging import logger
from degenbot.provider.sync_adapter import ProviderAdapter


def get_or_create_erc20_token(
    provider: ProviderAdapter,
    session: Session,
    chain_id: int,
    token_address: ChecksumAddress,
) -> Erc20TokenTable:
    """Get existing ERC20 token or create new one.

    When creating a new token, attempts to fetch name, symbol, and decimals
    from the blockchain and populate the database record.

    Returns:
        The computed value.

    """
    if (
        token := session.scalar(
            select(Erc20TokenTable).where(
                Erc20TokenTable.chain == chain_id,
                Erc20TokenTable.address == token_address,
            ),
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
                f"name='{name}', symbol='{symbol}', decimals={decimals}",
            )

    return token
